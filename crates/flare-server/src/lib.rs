//! Flare server: owns the shared sessions; clients attach over WebSocket
//! (docs/spec.md, "Flare's session protocol" and build step 9).
//!
//! - [`store`]: a session's events on disk.
//! - [`Server`]: the sessions, the server's own verified replica of the
//!   log, and its way to the broker.
//! - [`router`]: the WebSocket at `/session`.
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
    ClientMessage, Event, EventKind, Place, Problem, ProposeArgs, RECENT, SESSION_VERSION, SeeArgs,
    ServerMessage, Verb, VerbCall, VerbResult, valid_session,
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
        id: String,
        verb: Verb,
        args: serde_json::Value,
    ) {
        let outcome = match verb {
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
    let (session, after, client) = match serde_json::from_str::<ClientMessage>(&first) {
        Ok(ClientMessage::Attach {
            session,
            after,
            client,
        }) => (session, after, client),
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
        version: SESSION_VERSION,
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
                server.command(&session, &client, id, verb, args).await;
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
