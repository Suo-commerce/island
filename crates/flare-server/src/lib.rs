//! Flare server: owns the shared sessions; clients attach over WebSocket
//! (docs/spec.md, "Flare's session protocol" and build step 9).
//!
//! - [`store`]: a session's events on disk.
//! - [`Server`]: the sessions, the server's own verified replica of the
//!   log, and its way to the broker.
//! - [`router`]: the WebSocket at `/session`.
//! - [`agent_router`]: the agent's side, on a Unix socket only the island
//!   socket (tasks) and `island-sender` (replies) may use: the next
//!   message said to the agent, and the agent's reply delivered once per
//!   action key (docs/slice-2.md, "Actions and the sender").
//!
//! The server holds no writer key. A `propose` carries a submission its
//! client signed; the server hands it to the broker as it is. A `see` is
//! answered from the server's replica, verified from genesis against the
//! pinned root key, and names the head it was made at.

#![warn(missing_docs)]

pub mod store;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::header::{HOST, ORIGIN};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use flare_core::session::{
    AgentText, ClientMessage, Event, EventKind, MAX_REPLY, MAX_SAY, Place, Problem, ProposeArgs,
    RECENT, SESSION_VERSION, SayArgs, SayRequest, SeeArgs, ServerMessage, Task, UserMessage, Verb,
    VerbCall, VerbResult, valid_session,
};
use flare_core::{Overview, StreamView, content, hex, kind_name, short};
use flare_link::{PullError, Replica, pull};
use island_client::wire::{AppendRequest, Code};
use island_client::{Broker, Error, Plain, codec};
use island_core::PublicKey;
use tokio::sync::{Mutex, broadcast, watch};

use crate::store::Store;

/// How many events may wait for a slow client before it misses some and
/// is disconnected; it attaches again from its cursor.
const BACKLOG: usize = 256;

/// How the server tells the time: microseconds since the epoch.
pub type Clock = fn() -> u64;

/// What the server is set up with.
pub struct Config {
    /// The broker to pull from and to hand proposals to.
    pub broker: String,
    /// The pinned root key.
    pub root: PublicKey,
    /// Where the sessions and the replica are kept.
    pub data: PathBuf,
    /// The host names and addresses requests may be addressed to.
    pub hosts: Vec<String>,
    /// The clock.
    pub clock: Clock,
    /// The session the agent listens in: `say` is taken there only, and
    /// its tasks and replies are there.
    pub agent_session: String,
}

/// One session: its events, and the channel new ones go out on.
struct Session {
    store: Store,
    live: broadcast::Sender<Event>,
}

/// The server's state.
pub struct Server {
    config: Config,
    sessions: std::sync::Mutex<BTreeMap<String, Session>>,
    replica: Mutex<Replica>,
    plain: Plain,
    broker: Mutex<Option<Broker>>,
    stopping: watch::Sender<bool>,
}

/// Why the server could not start.
pub type StartError = String;

impl Server {
    /// Open the server's replica and get ready to serve.
    pub fn new(config: Config) -> Result<Arc<Self>, StartError> {
        let replica = Replica::open(&config.data.join("island.log"), config.root)
            .map_err(|error| error.to_string())?;
        let plain = Plain::new(&config.broker).map_err(|error| error.to_string())?;
        Ok(Arc::new(Self {
            config,
            sessions: std::sync::Mutex::new(BTreeMap::new()),
            replica: Mutex::new(replica),
            plain,
            broker: Mutex::new(None),
            stopping: watch::Sender::new(false),
        }))
    }

    /// Say goodbye to every attached client, so a stop does not wait on
    /// them. Their sessions stay on disk; they attach again later.
    pub fn stop(&self) {
        self.stopping.send_replace(true);
    }

    /// Whether [`Server::stop`] was called, as a channel to wait on.
    pub fn stopped(&self) -> watch::Receiver<bool> {
        self.stopping.subscribe()
    }

    /// Record an event in a session and send it to every attached client.
    fn record(
        &self,
        session: &str,
        kind: EventKind,
        payload: impl serde::Serialize,
    ) -> Result<Event, String> {
        let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "the sessions are lost".to_owned())?;
        let session = self.session(&mut sessions, session)?;
        let event = session
            .store
            .append((self.config.clock)(), kind, payload)
            .map_err(|error| format!("{}: {error}", session.store.path().display()))?;
        // Nobody attached is not an error.
        let _ = session.live.send(event.clone());
        Ok(event)
    }

    /// The session, opened from disk the first time it is asked for.
    fn session<'a>(
        &self,
        sessions: &'a mut BTreeMap<String, Session>,
        name: &str,
    ) -> Result<&'a mut Session, String> {
        if !sessions.contains_key(name) {
            let path = self
                .config
                .data
                .join("sessions")
                .join(format!("{name}.jsonl"));
            let store =
                Store::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let (live, _) = broadcast::channel(BACKLOG);
            sessions.insert(name.to_owned(), Session { store, live });
        }
        sessions
            .get_mut(name)
            .ok_or_else(|| "the session vanished".to_owned())
    }

    /// Join a session: what to replay, the place of the first new event,
    /// and the channel the new ones come on. Taken together, so that no
    /// event falls between the replay and the channel.
    fn attach(
        &self,
        name: &str,
        after: Option<u64>,
    ) -> Result<(Vec<Event>, u64, broadcast::Receiver<Event>), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "the sessions are lost".to_owned())?;
        let session = self.session(&mut sessions, name)?;
        let events = session.store.events();
        let from = match after {
            Some(after) => usize::try_from(after.saturating_add(1)).unwrap_or(usize::MAX),
            None => events.len().saturating_sub(RECENT as usize),
        };
        let replay = events.get(from..).unwrap_or(&[]).to_vec();
        Ok((replay, session.store.next(), session.live.subscribe()))
    }

    /// Run one command: its call, then its result, as events.
    async fn command(
        &self,
        session: &str,
        client: &str,
        version: u32,
        id: String,
        verb: Verb,
        args: serde_json::Value,
    ) {
        if verb == Verb::Say {
            let said = self.say(session, client, version, id.clone(), args);
            if let Err(text) = said {
                let _ = self.record(session, EventKind::Error, Problem { id: Some(id), text });
            }
            return;
        }
        let outcome = match verb {
            Verb::Say => unreachable!("handled above"),
            Verb::See => match serde_json::from_value::<SeeArgs>(args) {
                Ok(args) => {
                    let what = match &args.stream {
                        Some(stream) => format!("see {stream}"),
                        None => "see".to_owned(),
                    };
                    Ok((what, Work::See(args)))
                }
                Err(error) => Err(format!("see: {error}")),
            },
            Verb::Propose => match serde_json::from_value::<ProposeArgs>(args) {
                Ok(args) => {
                    let request = AppendRequest {
                        submission: args.submission,
                        sig_writer: args.sig_writer,
                    };
                    match codec::parse_append(&request) {
                        Ok((submission, signature)) => {
                            let what = format!(
                                "propose {} on {} by {}, counter {}: {}",
                                kind_name(submission.kind()),
                                codec::stream_name(&submission.stream),
                                short(&submission.writer),
                                submission.counter,
                                content(&submission.body)
                            );
                            Ok((what, Work::Propose(Box::new((submission, signature)))))
                        }
                        Err(malformed) => Err(format!("propose: {malformed}")),
                    }
                }
                Err(error) => Err(format!("propose: {error}")),
            },
        };
        let (what, work) = match outcome {
            Ok(ready) => ready,
            Err(text) => {
                let _ = self.record(session, EventKind::Error, Problem { id: Some(id), text });
                return;
            }
        };
        let call = VerbCall {
            id: id.clone(),
            client: client.to_owned(),
            verb,
            what,
        };
        if self.record(session, EventKind::VerbCall, call).is_err() {
            return;
        }
        let result = match work {
            Work::See(args) => self.see(id, &args).await,
            Work::Propose(signed) => self.propose(id, &signed.0, &signed.1).await,
        };
        let _ = self.record(session, EventKind::VerbResult, result);
    }

    /// `say`: a message for the agent, recorded as a `user_message`. Its
    /// place is the task's id; there is no call or result, the agent's
    /// reply is the answer.
    fn say(
        &self,
        session: &str,
        client: &str,
        version: u32,
        id: String,
        args: serde_json::Value,
    ) -> Result<(), String> {
        if version < 2 {
            return Err("say needs session protocol version 2; attach with version 2".to_owned());
        }
        if session != self.config.agent_session {
            return Err(format!(
                "the agent listens in session {}, not here",
                self.config.agent_session
            ));
        }
        let args: SayArgs =
            serde_json::from_value(args).map_err(|error| format!("say: {error}"))?;
        if args.text.trim().is_empty() {
            return Err("say: nothing to say".to_owned());
        }
        if args.text.len() > MAX_SAY {
            return Err(format!("say: at most {MAX_SAY} bytes"));
        }
        let message = UserMessage {
            id,
            client: client.to_owned(),
            text: args.text,
        };
        self.record(session, EventKind::UserMessage, message)
            .map(|_| ())
    }

    /// The first message said to the agent after `after` (every one, if
    /// `None`), and a channel for the ones still to come.
    fn next_task(
        &self,
        after: Option<u64>,
    ) -> Result<(Option<Task>, broadcast::Receiver<Event>), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "the sessions are lost".to_owned())?;
        let name = self.config.agent_session.clone();
        let session = self.session(&mut sessions, &name)?;
        let task = session
            .store
            .events()
            .iter()
            .filter(|event| after.is_none_or(|after| event.seq > after))
            .find_map(task_of);
        Ok((task, session.live.subscribe()))
    }

    /// Deliver the agent's reply once: the event already recorded for its
    /// key, or a new `agent_text`.
    fn deliver(&self, vessel: &str, key: &str, request: &str) -> Result<u64, Undeliverable> {
        let said: SayRequest = serde_json::from_str(request).map_err(|error| {
            Undeliverable::Malformed(format!("not a flare.say request: {error}"))
        })?;
        if said.text.len() > MAX_REPLY {
            return Err(Undeliverable::Malformed(format!(
                "a reply is at most {MAX_REPLY} bytes"
            )));
        }
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| Undeliverable::Down("the sessions are lost".to_owned()))?;
        let name = self.config.agent_session.clone();
        let session = self
            .session(&mut sessions, &name)
            .map_err(Undeliverable::Down)?;
        let seen = session.store.events().iter().find(|event| {
            event.kind == EventKind::AgentText
                && serde_json::from_value::<AgentText>(event.payload.clone())
                    .is_ok_and(|text| text.key == key)
        });
        if let Some(event) = seen {
            return Ok(event.seq);
        }
        let payload = serde_json::to_value(AgentText {
            task: said.task,
            text: said.text,
            vessel: vessel.to_owned(),
            key: key.to_owned(),
        })
        .map_err(|error| Undeliverable::Down(error.to_string()))?;
        let event = session
            .store
            .append((self.config.clock)(), EventKind::AgentText, payload)
            .map_err(|error| {
                Undeliverable::Down(format!("{}: {error}", session.store.path().display()))
            })?;
        let _ = session.live.send(event.clone());
        Ok(event.seq)
    }

    /// `see`, from the replica, pulled first.
    async fn see(&self, id: String, args: &SeeArgs) -> VerbResult {
        let mut replica = self.replica.lock().await;
        let note = match pull(&mut replica, &self.plain).await {
            Ok(_) => String::new(),
            Err(PullError::Broker(error)) => format!("(not pulled: {error})\n"),
            Err(error) => {
                return VerbResult {
                    id,
                    ok: false,
                    text: error.to_string(),
                    at: None,
                };
            }
        };
        let records = replica.records();
        let view = match &args.stream {
            None => Overview::of(records, replica.chain()).to_string(),
            Some(stream) => StreamView::of(records, stream, args.entries.unwrap_or(20)).to_string(),
        };
        VerbResult {
            id,
            ok: true,
            text: format!("{note}{view}"),
            at: replica.head().map(|(seq, hash)| Place {
                seq,
                hash: hex(&hash),
            }),
        }
    }

    /// `propose`: the signed submission to the broker, as it is.
    async fn propose(
        &self,
        id: String,
        submission: &island_core::Submission,
        signature: &island_core::Signature,
    ) -> VerbResult {
        let mut slot = self.broker.lock().await;
        let mut answer = Err(Error::Unreachable("not tried".to_owned()));
        // A broken connection is made again once; a resend is safe.
        for _ in 0..2 {
            let broker = match slot.take() {
                Some(broker) => broker,
                None => match Broker::connect(&self.config.broker).await {
                    Ok(broker) => broker,
                    Err(error) => {
                        answer = Err(error);
                        continue;
                    }
                },
            };
            answer = broker.append(submission, signature).await;
            if matches!(answer, Err(Error::Unreachable(_))) {
                broker.close().await;
            } else {
                *slot = Some(broker);
                break;
            }
        }
        match answer {
            Ok(Ok((seq, hash))) => VerbResult {
                id,
                ok: true,
                text: format!("written: entry {seq}"),
                at: Some(Place {
                    seq,
                    hash: hex(&hash),
                }),
            },
            Ok(Err(refusal)) => {
                let at = refusal.verdict.as_ref().and_then(|verdict| {
                    let (seq, hash) = codec::parse_position(verdict).ok()?;
                    Some(Place {
                        seq,
                        hash: hex(&hash),
                    })
                });
                let text = match (refusal.code, &at) {
                    (Code::Gate, Some(place)) => format!(
                        "not written: {}; the refusal is recorded as entry {}",
                        refusal.reason, place.seq
                    ),
                    _ => format!("not written: {}", refusal.reason),
                };
                VerbResult {
                    id,
                    ok: false,
                    text,
                    at,
                }
            }
            Err(error) => VerbResult {
                id,
                ok: false,
                text: format!(
                    "not known whether it was written: {error}. Propose the same again; \
                     it is never written twice"
                ),
                at: None,
            },
        }
    }
}

/// What a command will do, once its call is recorded.
enum Work {
    See(SeeArgs),
    Propose(Box<(island_core::Submission, island_core::Signature)>),
}

/// The server's routes: the session WebSocket at `/session`.
pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/session", get(upgrade))
        .with_state(server)
}

/// Whether `authority` (a `Host` header) is one of `hosts`, with or
/// without a port.
pub fn host_allowed(authority: &str, hosts: &[String]) -> bool {
    let authority = authority.to_ascii_lowercase();
    let name = match authority.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or(rest, |(name, _)| name),
        None => authority
            .rsplit_once(':')
            .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
            .map_or(authority.as_str(), |(name, _)| name),
    };
    hosts.iter().any(|allowed| {
        let allowed = allowed.to_ascii_lowercase();
        allowed == authority || allowed == name
    })
}

async fn upgrade(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    socket: WebSocketUpgrade,
) -> Response {
    // A web page may open a WebSocket to any address; browsers say where
    // the page came from, terminals do not. No page may attach.
    if headers.contains_key(ORIGIN) {
        return (
            StatusCode::FORBIDDEN,
            "no web page may attach to a session\n",
        )
            .into_response();
    }
    let addressed = headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        .is_some_and(|host| host_allowed(host, &server.config.hosts));
    if !addressed {
        return (
            StatusCode::FORBIDDEN,
            "this server does not answer to that host name\n",
        )
            .into_response();
    }
    socket.on_upgrade(move |socket| attached(server, socket))
}

async fn send(socket: &mut WebSocket, message: &ServerMessage) -> bool {
    match serde_json::to_string(message) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn refuse(mut socket: WebSocket, reason: String) {
    let _ = send(&mut socket, &ServerMessage::Refused { reason }).await;
    let _ = socket.send(Message::Close(None)).await;
}

/// One client, from its attach until it goes.
async fn attached(server: Arc<Server>, mut socket: WebSocket) {
    // First, the attach.
    let first = loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => break text,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            _ => return,
        }
    };
    let (session, after, client, version) = match serde_json::from_str::<ClientMessage>(&first) {
        Ok(ClientMessage::Attach {
            session,
            after,
            client,
            version,
        }) => (
            session,
            after,
            client,
            version.unwrap_or(1).clamp(1, SESSION_VERSION),
        ),
        Ok(_) => return refuse(socket, "attach first".to_owned()).await,
        Err(error) => {
            return refuse(socket, format!("not a message of this protocol: {error}")).await;
        }
    };
    if !valid_session(&session) {
        return refuse(
            socket,
            "a session is named with 1 to 64 of a-z, 0-9 and -".to_owned(),
        )
        .await;
    }
    let client: String = client
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect();
    let (replay, next, mut live) = match server.attach(&session, after) {
        Ok(joined) => joined,
        Err(error) => return refuse(socket, error).await,
    };
    let attached = ServerMessage::Attached {
        session: session.clone(),
        next,
        version,
    };
    if !send(&mut socket, &attached).await {
        return;
    }
    for event in replay {
        if !send(&mut socket, &ServerMessage::Event(event)).await {
            return;
        }
    }

    // One client's commands run one after another, in the order typed: a
    // `see` after a `propose` shows what the propose did. Different
    // clients' commands run side by side; the session orders their events.
    let (queue, mut commands) =
        tokio::sync::mpsc::unbounded_channel::<(String, Verb, serde_json::Value)>();
    let worker = {
        let (server, session, client) = (server.clone(), session.clone(), client.clone());
        tokio::spawn(async move {
            while let Some((id, verb, args)) = commands.recv().await {
                server
                    .command(&session, &client, version, id, verb, args)
                    .await;
            }
        })
    };
    // The worker finishes what was asked even if the client goes.
    drop(worker);

    // Then commands in, events out, until the client goes.
    let mut stopping = server.stopping.subscribe();
    loop {
        tokio::select! {
            () = async { let _ = stopping.wait_for(|stop| *stop).await; } => {
                return refuse(socket, "the server is stopping; attach again later".to_owned()).await;
            }
            incoming = socket.recv() => {
                let text = match incoming {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => return,
                };
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(ClientMessage::Command { id, verb, args }) => {
                        if id.is_empty() || id.len() > 64 {
                            return refuse(socket, "a command id is 1 to 64 bytes".to_owned()).await;
                        }
                        if queue.send((id, verb, args)).is_err() {
                            return;
                        }
                    }
                    Ok(ClientMessage::Attach { .. }) => {
                        return refuse(socket, "already attached".to_owned()).await;
                    }
                    Err(error) => {
                        return refuse(socket, format!("not a message of this protocol: {error}")).await;
                    }
                }
            }
            event = live.recv() => match event {
                Ok(event) => {
                    if event.seq >= next && !send(&mut socket, &ServerMessage::Event(event)).await {
                        return;
                    }
                }
                // Too slow to keep up: it attaches again from its cursor.
                Err(_) => {
                    return refuse(socket, "fell behind; attach again from the last event seen".to_owned()).await;
                }
            },
        }
    }
}

/// A `user_message` event as a task.
fn task_of(event: &Event) -> Option<Task> {
    if event.kind != EventKind::UserMessage {
        return None;
    }
    let message: UserMessage = serde_json::from_value(event.payload.clone()).ok()?;
    Some(Task {
        id: event.seq,
        at: event.at,
        client: message.client,
        text: message.text,
    })
}

/// Why a reply could not be delivered.
#[derive(Debug)]
enum Undeliverable {
    /// The request is not one: it never will be.
    Malformed(String),
    /// The server could not record it now: try again.
    Down(String),
}

/// The longest a task request may wait for a message, in seconds.
pub const MAX_TASK_WAIT: u64 = 60;

/// Who may use the agent socket, by uid.
#[derive(Clone, Copy, Debug)]
pub struct AgentPeers {
    /// May take tasks: the island socket's user.
    pub tasks: u32,
    /// May deliver replies: `island-sender`, root.
    pub replies: u32,
}

/// A connection's peer on the agent socket.
#[derive(Clone, Copy, Debug)]
pub struct Peer {
    /// Its uid, as the kernel says.
    pub uid: Option<u32>,
}

impl
    axum::extract::connect_info::Connected<
        axum::serve::IncomingStream<'_, tokio::net::UnixListener>,
    > for Peer
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        Self {
            uid: stream.io().peer_cred().ok().map(|cred| cred.uid()),
        }
    }
}

#[derive(Clone)]
struct Agent {
    server: Arc<Server>,
    peers: AgentPeers,
}

/// The agent socket's routes:
///
/// | Request | Who | Answer |
/// | --- | --- | --- |
/// | `GET /task?after=N&wait=S` | [`AgentPeers::tasks`] | the next [`Task`] after N, waiting up to S seconds; 204: none |
/// | `POST /say` `{ vessel, key, request }` | [`AgentPeers::replies`] | `{ event }`: the `agent_text`, recorded once per key |
pub fn agent_router(server: Arc<Server>, peers: AgentPeers) -> Router {
    Router::new()
        .route("/task", get(agent_task))
        .route("/say", axum::routing::post(agent_say))
        .with_state(Agent { server, peers })
}

#[derive(serde::Deserialize)]
struct TaskQuery {
    after: Option<u64>,
    #[serde(default)]
    wait: u64,
}

async fn agent_task(
    State(agent): State<Agent>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<Peer>,
    axum::extract::Query(query): axum::extract::Query<TaskQuery>,
) -> Response {
    if peer.uid != Some(agent.peers.tasks) {
        return (StatusCode::FORBIDDEN, "tasks are for the island socket\n").into_response();
    }
    let (found, mut live) = match agent.server.next_task(query.after) {
        Ok(next) => next,
        Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
    };
    if let Some(task) = found {
        return axum::Json(task).into_response();
    }
    let wait = std::time::Duration::from_secs(query.wait.min(MAX_TASK_WAIT));
    let mut stopping = agent.server.stopping.subscribe();
    let waited = tokio::time::timeout(wait, async {
        loop {
            tokio::select! {
                () = async { let _ = stopping.wait_for(|stop| *stop).await; } => return None,
                event = live.recv() => match event {
                    Ok(event) => {
                        let wanted = query.after.is_none_or(|after| event.seq > after);
                        if wanted && let Some(task) = task_of(&event)
                        {
                            return Some(task);
                        }
                    }
                    // Missed some: the store has them.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        return agent.server.next_task(query.after).ok().and_then(|(task, _)| task);
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
            }
        }
    })
    .await;
    match waited {
        Ok(Some(task)) => axum::Json(task).into_response(),
        _ => StatusCode::NO_CONTENT.into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    vessel: String,
    key: String,
    request: String,
}

async fn agent_say(
    State(agent): State<Agent>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<Peer>,
    axum::Json(delivery): axum::Json<Delivery>,
) -> Response {
    if peer.uid != Some(agent.peers.replies) {
        return (
            StatusCode::FORBIDDEN,
            "replies are delivered by island-sender\n",
        )
            .into_response();
    }
    let hex_of = |text: &str, bytes: usize| {
        text.len() == 2 * bytes && text.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    if !hex_of(&delivery.vessel, 16) || !hex_of(&delivery.key, 32) {
        return (StatusCode::BAD_REQUEST, "vessel is 32 hex digits, key 64\n").into_response();
    }
    match agent.server.deliver(
        &delivery.vessel.to_ascii_lowercase(),
        &delivery.key.to_ascii_lowercase(),
        &delivery.request,
    ) {
        Ok(event) => axum::Json(serde_json::json!({ "event": event })).into_response(),
        Err(Undeliverable::Malformed(why)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, why).into_response()
        }
        Err(Undeliverable::Down(why)) => (StatusCode::SERVICE_UNAVAILABLE, why).into_response(),
    }
}
