//! The shared session, end to end: `flare-server` on a loopback port, a
//! broker of the tests' own behind it, and clients speaking the session
//! protocol over WebSocket (docs/spec.md, build step 9).

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flare_core::session::{
    AgentPhase, AgentState, ClientMessage, Event, EventKind, ProposeArgs, RECENT, SeeArgs,
    ServerMessage, Verb, VerbCall, VerbResult,
};
use flare_server::{Config, Server, router};
use futures_util::{SinkExt, StreamExt};
use island_client::{Writer, codec};
use island_core::{Evidence, Verse};
use support::broker::{self, Log, Shared};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

static CLOCK: AtomicU64 = AtomicU64::new(1_794_400_000_000_000);

fn tick() -> u64 {
    CLOCK.fetch_add(1_000_000, Ordering::SeqCst)
}

/// A broker and a session server, on loopback ports.
struct Running {
    broker: String,
    log: Shared,
    url: String,
    server: Arc<Server>,
    serving: tokio::task::JoinHandle<()>,
}

async fn start(data: &Path, log: Shared, broker: Option<String>) -> Running {
    start_with(data, log, broker, None).await
}

/// The server, serving the face in `face` if there is one.
async fn start_with(
    data: &Path,
    log: Shared,
    broker: Option<String>,
    face: Option<&Path>,
) -> Running {
    let broker = match broker {
        Some(url) => url,
        None => broker::serve(log.clone()).await,
    };
    let server = Server::new(Config {
        broker: broker.clone(),
        root: broker::pin(),
        data: data.to_owned(),
        hosts: ["localhost", "127.0.0.1", "::1"]
            .map(str::to_owned)
            .to_vec(),
        clock: tick,
        agent_session: "main".to_owned(),
        face: face.map(Path::to_owned),
    })
    .expect("the server starts");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("ws://{}/session", listener.local_addr().expect("address"));
    let app = router(server.clone());
    let serving = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Running {
        broker,
        log,
        url,
        server,
        serving,
    }
}

fn started_log() -> Shared {
    Arc::new(Mutex::new(Log::started()))
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A client of the session protocol.
struct Client {
    socket: Socket,
    name: String,
}

impl Client {
    async fn attach(url: &str, session: &str, after: Option<u64>, name: &str) -> (Self, u64) {
        let (socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connects");
        let mut client = Self {
            socket,
            name: name.to_owned(),
        };
        client
            .send(&ClientMessage::Attach {
                session: session.to_owned(),
                after,
                client: name.to_owned(),
                version: Some(2),
            })
            .await;
        match client.receive().await {
            ServerMessage::Attached { next, version, .. } => {
                assert_eq!(version, 2);
                (client, next)
            }
            other => panic!("not attached: {other:?}"),
        }
    }

    async fn send(&mut self, message: &ClientMessage) {
        let text = serde_json::to_string(message).expect("JSON");
        self.socket
            .send(Message::Text(text.into()))
            .await
            .expect("sent");
    }

    async fn receive(&mut self) -> ServerMessage {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(10), self.socket.next())
                .await
                .expect("an answer in time")
                .expect("the session is open")
                .expect("a message");
            if let Message::Text(text) = message {
                return serde_json::from_str(&text).expect("a message of the protocol");
            }
        }
    }

    async fn event(&mut self) -> Event {
        match self.receive().await {
            ServerMessage::Event(event) => event,
            other => panic!("not an event: {other:?}"),
        }
    }

    async fn events(&mut self, count: usize) -> Vec<Event> {
        let mut events = Vec::new();
        for _ in 0..count {
            events.push(self.event().await);
        }
        events
    }

    async fn command(&mut self, id: &str, verb: Verb, args: serde_json::Value) {
        self.send(&ClientMessage::Command {
            id: id.to_owned(),
            verb,
            args,
        })
        .await;
    }

    async fn see(&mut self, id: &str, stream: Option<&str>) {
        let args = SeeArgs {
            stream: stream.map(str::to_owned),
            entries: None,
        };
        self.command(id, Verb::See, serde_json::to_value(args).expect("JSON"))
            .await;
    }
}

fn call(event: &Event) -> VerbCall {
    assert_eq!(event.kind, EventKind::VerbCall, "{event:?}");
    serde_json::from_value(event.payload.clone()).expect("a call")
}

fn result(event: &Event) -> VerbResult {
    assert_eq!(event.kind, EventKind::VerbResult, "{event:?}");
    serde_json::from_value(event.payload.clone()).expect("a result")
}

fn proposal(counter: u64, text: &str) -> serde_json::Value {
    let verse = Verse::new(Evidence::Decision, text.to_owned(), Vec::new());
    let signed = Writer::new(broker::device()).verse("island", counter, verse);
    let request = codec::append_request(&signed.submission, &signed.sig_writer);
    serde_json::to_value(ProposeArgs {
        submission: request.submission,
        sig_writer: request.sig_writer,
    })
    .expect("JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_see_the_same_session() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = start(dir.path(), started_log(), None).await;
    let (mut laptop, next) = Client::attach(&running.url, "main", None, "laptop").await;
    assert_eq!(next, 0);
    let (mut ssh, _) = Client::attach(&running.url, "main", None, "ssh").await;

    // The laptop asks to see; both get the call and the result.
    laptop.see("l-1", None).await;
    let seen_by_laptop = laptop.events(2).await;
    let seen_by_ssh = ssh.events(2).await;
    assert_eq!(seen_by_laptop, seen_by_ssh);
    let asked = call(&seen_by_laptop[0]);
    assert_eq!(
        (asked.client.as_str(), asked.verb, asked.what.as_str()),
        ("laptop", Verb::See, "see")
    );
    let answer = result(&seen_by_laptop[1]);
    assert!(answer.ok, "{answer:?}");
    assert!(
        answer.text.starts_with("4 entries, verified from genesis"),
        "{}",
        answer.text
    );
    let head = running.log.lock().expect("lock").records[3].hash;
    let at = answer.at.expect("the head it was made at");
    assert_eq!((at.seq, at.hash), (3, flare_core::hex(&head)));

    // The ssh terminal proposes a verse signed with the device key; it is
    // written, and both see it happen.
    ssh.command("s-1", Verb::Propose, proposal(1, "the session carries it"))
        .await;
    let events = laptop.events(2).await;
    assert_eq!(events, ssh.events(2).await);
    let asked = call(&events[0]);
    assert_eq!(asked.client, "ssh");
    assert!(
        asked.what.starts_with("propose verse on island by "),
        "{}",
        asked.what
    );
    assert!(
        asked.what.contains("\"the session carries it\""),
        "{}",
        asked.what
    );
    let answer = result(&events[1]);
    assert!(answer.ok, "{answer:?}");
    assert_eq!(answer.text, "written: entry 4");
    let written = running.log.lock().expect("lock").records[4].hash;
    assert_eq!(answer.at.map(|at| at.hash), Some(flare_core::hex(&written)));

    // A refusal is a result too: the same counter again.
    laptop
        .command(
            "l-2",
            Verb::Propose,
            proposal(1, "a second verse, an old counter"),
        )
        .await;
    let events = laptop.events(2).await;
    let answer = result(&events[1]);
    assert!(!answer.ok);
    assert!(answer.text.starts_with("not written: "), "{}", answer.text);
    assert_eq!(ssh.events(2).await, events);

    // `see` a stream, after the write: the new entry is there.
    ssh.see("s-2", Some("island")).await;
    let answer = result(&ssh.events(2).await[1]);
    assert!(
        answer.text.contains("\"the session carries it\""),
        "{}",
        answer.text
    );
    assert_eq!(answer.at.map(|at| at.seq), Some(4));

    // The order is the server's: 0, 1, 2, ...
    let seqs: Vec<u64> = laptop
        .events(2)
        .await
        .iter()
        .map(|event| event.seq)
        .collect();
    assert_eq!(seqs, [6, 7]);
    running.serving.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_attaches_again_at_its_cursor_and_the_session_outlives_the_server() {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = started_log();
    let running = start(dir.path(), log.clone(), None).await;
    let (mut first, _) = Client::attach(&running.url, "main", None, "laptop").await;
    for n in 0..15 {
        first.see(&format!("l-{n}"), None).await;
        first.events(2).await;
    }
    drop(first);

    // After event 25: four more.
    let (mut again, next) = Client::attach(&running.url, "main", Some(25), "laptop").await;
    assert_eq!(next, 30);
    let replayed: Vec<u64> = again
        .events(4)
        .await
        .iter()
        .map(|event| event.seq)
        .collect();
    assert_eq!(replayed, [26, 27, 28, 29]);

    // No cursor: the newest few.
    let (mut fresh, _) = Client::attach(&running.url, "main", None, "phone").await;
    let replayed = fresh.events(RECENT as usize).await;
    assert_eq!(replayed.first().map(|event| event.seq), Some(30 - RECENT));

    // Another session is another list.
    let (_, next) = Client::attach(&running.url, "flare-dev", None, "laptop").await;
    assert_eq!(next, 0);

    // The server stops: every client is told, and the events stay.
    running.server.stop();
    match again.receive().await {
        ServerMessage::Refused { reason } => assert!(reason.contains("stopping"), "{reason}"),
        other => panic!("{other:?}"),
    }
    drop((again, fresh));
    running.serving.abort();
    drop(running);
    let running = start(dir.path(), log, None).await;
    let (mut back, next) = Client::attach(&running.url, "main", Some(28), "laptop").await;
    assert_eq!(next, 30);
    assert_eq!(back.event().await.seq, 29);
    back.see("l-99", None).await;
    assert_eq!(back.event().await.seq, 30);
    running.serving.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_typing_at_once_get_one_order() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = start(dir.path(), started_log(), None).await;
    let mut clients = Vec::new();
    for name in ["a", "b", "c"] {
        clients.push(Client::attach(&running.url, "main", None, name).await.0);
    }
    for client in &mut clients {
        for n in 0..10 {
            let id = format!("{}-{n}", client.name);
            client.see(&id, None).await;
        }
    }
    let mut seen = Vec::new();
    for client in &mut clients {
        seen.push(client.events(60).await);
    }
    // Every client has the same 60 events, in the same order, numbered
    // 0 to 59; and each call comes before its result.
    assert!(seen.iter().all(|events| events == &seen[0]));
    let seqs: Vec<u64> = seen[0].iter().map(|event| event.seq).collect();
    assert_eq!(seqs, (0..60).collect::<Vec<_>>());
    for event in seen[0]
        .iter()
        .filter(|event| event.kind == EventKind::VerbResult)
    {
        let id = result(event).id;
        let called = seen[0]
            .iter()
            .position(|other| other.kind == EventKind::VerbCall && call(other).id == id)
            .expect("its call");
        assert!((called as u64) < event.seq);
    }
    running.serving.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_the_protocol_does_not_allow_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = start(dir.path(), started_log(), None).await;
    let refused = |message: serde_json::Value| {
        let url = running.url.clone();
        async move {
            let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
                .await
                .expect("connects");
            socket
                .send(Message::Text(message.to_string().into()))
                .await
                .expect("sent");
            loop {
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(&text).expect("protocol") {
                            ServerMessage::Refused { reason } => return reason,
                            other => panic!("{other:?}"),
                        }
                    }
                    Some(Ok(_)) => {}
                    other => panic!("{other:?}"),
                }
            }
        }
    };
    let reason =
        refused(serde_json::json!({"type": "command", "id": "x", "verb": "see", "args": {}})).await;
    assert_eq!(reason, "attach first");
    let reason =
        refused(serde_json::json!({"type": "attach", "session": "Main!", "client": "x"})).await;
    assert!(reason.contains("1 to 64 of a-z"), "{reason}");
    let reason = refused(
        serde_json::json!({"type": "attach", "session": "main", "client": "x", "extra": 1}),
    )
    .await;
    assert!(
        reason.contains("not a message of this protocol"),
        "{reason}"
    );

    // A web page may not attach: its browser says where it came from.
    let http = |origin: Option<&str>, host: &str| {
        let url = running.url.replace("ws://", "http://");
        let origin = origin.map(str::to_owned);
        let host = host.to_owned();
        async move {
            let mut request = reqwest_like(&url, &host, origin.as_deref());
            request.status().await
        }
    };
    assert_eq!(http(Some("https://evil.example"), "127.0.0.1").await, 403);
    assert_eq!(http(None, "flare.evil.example").await, 403);
    assert_eq!(http(None, "127.0.0.1").await, 101);

    // Commands that are not right become error events, not results.
    let (mut client, _) = Client::attach(&running.url, "main", None, "laptop").await;
    client
        .command(
            "p-1",
            Verb::Propose,
            serde_json::json!({"submission": "%%", "sig_writer": ""}),
        )
        .await;
    let event = client.event().await;
    assert_eq!(event.kind, EventKind::Error);
    assert!(
        event.payload["text"]
            .as_str()
            .is_some_and(|text| text.starts_with("propose: submission"))
    );
    assert_eq!(event.payload["id"], "p-1");
    client
        .command("s-1", Verb::See, serde_json::json!({"stream": 5}))
        .await;
    assert_eq!(client.event().await.kind, EventKind::Error);
    running.serving.abort();
}

/// A plain HTTP GET of the session path with the headers a browser or a
/// stranger would send, to read the status the server answers with.
fn reqwest_like(url: &str, host: &str, origin: Option<&str>) -> RawGet {
    RawGet {
        url: url.to_owned(),
        host: host.to_owned(),
        origin: origin.map(str::to_owned),
    }
}

struct RawGet {
    url: String,
    host: String,
    origin: Option<String>,
}

impl RawGet {
    async fn status(&mut self) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let address = self
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/session");
        let mut stream = TcpStream::connect(address).await.expect("connects");
        let origin = self
            .origin
            .as_ref()
            .map_or(String::new(), |origin| format!("Origin: {origin}\r\n"));
        // What a browser sends to open a WebSocket.
        let request = format!(
            "GET /session HTTP/1.1\r\nHost: {}\r\n{origin}Connection: Upgrade\r\n\
             Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            self.host
        );
        stream.write_all(request.as_bytes()).await.expect("sent");
        let mut buffer = [0u8; 256];
        let read = stream.read(&mut buffer).await.expect("an answer");
        let answer = String::from_utf8_lossy(&buffer[..read]).into_owned();
        answer
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("a status line")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_the_broker_down_see_shows_the_replica_and_a_proposal_is_not_known() {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = started_log();
    let running = start(dir.path(), log.clone(), None).await;
    let (mut client, _) = Client::attach(&running.url, "main", None, "laptop").await;
    client.see("l-1", None).await;
    client.events(2).await;
    running.server.stop();
    drop(client);
    running.serving.abort();
    drop(running);

    // The same data, a broker that is not there.
    let running = start(dir.path(), log, Some("http://127.0.0.1:1".to_owned())).await;
    let (mut client, _) = Client::attach(&running.url, "main", Some(1), "laptop").await;
    client.see("l-2", None).await;
    let answer = result(&client.events(2).await[1]);
    assert!(answer.ok);
    assert!(
        answer
            .text
            .starts_with("(not pulled: the broker cannot be reached"),
        "{}",
        answer.text
    );
    assert!(
        answer.text.contains("4 entries, verified"),
        "{}",
        answer.text
    );

    client
        .command("l-3", Verb::Propose, proposal(1, "into the void"))
        .await;
    let answer = result(&client.events(2).await[1]);
    assert!(!answer.ok);
    assert!(
        answer.text.starts_with("not known whether it was written"),
        "{}",
        answer.text
    );
    let _ = running.broker;
    running.serving.abort();
}

/// One request on the agent socket: status and body.
async fn agent_ask(socket: &Path, method: &str, target: &str, body: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .expect("the agent socket answers");
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: flare\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.expect("sent");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.expect("read");
    let status = answer
        .get(9..12)
        .and_then(|code| code.parse().ok())
        .expect("a status");
    let body = answer
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// The agent socket of `server` at `path`, letting `tasks` and `replies`
/// in by uid.
fn agent_socket(server: Arc<Server>, path: &Path, tasks: u32, replies: u32) {
    let listener = tokio::net::UnixListener::bind(path).expect("bind the agent socket");
    let app = flare_server::agent_router(server, flare_server::AgentPeers { tasks, replies })
        .into_make_service_with_connect_info::<flare_server::Peer>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
}

/// An `agent_state` event's state and task.
fn state(event: &Event) -> (AgentPhase, Option<u64>) {
    assert_eq!(event.kind, EventKind::AgentState, "{event:?}");
    let state: AgentState = serde_json::from_value(event.payload.clone()).expect("agent state");
    (state.state, state.task)
}

fn my_uid(dir: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir).expect("the temp dir").uid()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_say_is_a_task_and_the_reply_comes_back_once() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = start(&dir.path().join("server"), started_log(), None).await;
    let me = my_uid(dir.path());
    let socket = dir.path().join("agent.sock");
    agent_socket(running.server.clone(), &socket, me, me);
    let (mut laptop, _) = Client::attach(&running.url, "main", None, "laptop").await;

    // No task yet: a short wait ends with nothing. The agent asks only
    // when it has no task: it is idle, said once however often it asks.
    assert_eq!(agent_ask(&socket, "GET", "/task?wait=0", "").await.0, 204);
    assert_eq!(agent_ask(&socket, "GET", "/task?wait=0", "").await.0, 204);
    assert_eq!(state(&laptop.event().await), (AgentPhase::Idle, None));

    // Said in the session, it is the agent's next task.
    laptop
        .command(
            "s-1",
            Verb::Say,
            serde_json::json!({ "text": "draft the verse for that" }),
        )
        .await;
    let said = laptop.event().await;
    assert_eq!(said.kind, EventKind::UserMessage);
    let (status, body) = agent_ask(&socket, "GET", "/task", "").await;
    assert_eq!(status, 200, "{body}");
    let task: flare_core::session::Task = serde_json::from_str(&body).expect("a task");
    assert_eq!(task.id, said.seq);
    assert_eq!(task.text, "draft the verse for that");
    assert_eq!(task.client, "laptop");
    // Handed a task, it is working on it; handed it again, nothing new.
    assert_eq!(
        state(&laptop.event().await),
        (AgentPhase::Working, Some(task.id))
    );
    assert_eq!(agent_ask(&socket, "GET", "/task", "").await.0, 200);

    // After it, a waiting request gets the next one as it is said.
    let waiting = {
        let socket = socket.clone();
        let target = format!("/task?after={}&wait=20", task.id);
        tokio::spawn(async move { agent_ask(&socket, "GET", &target, "").await })
    };
    // It asked for the next one: it is done with the first.
    assert_eq!(state(&laptop.event().await), (AgentPhase::Idle, None));
    laptop
        .command(
            "s-2",
            Verb::Say,
            serde_json::json!({ "text": "and another" }),
        )
        .await;
    let second = laptop.event().await;
    assert_eq!(second.kind, EventKind::UserMessage);
    let (status, body) = waiting.await.expect("the wait ends");
    assert_eq!(status, 200, "{body}");
    let next: flare_core::session::Task = serde_json::from_str(&body).expect("a task");
    assert_eq!((next.id, next.text.as_str()), (second.seq, "and another"));
    assert_eq!(
        state(&laptop.event().await),
        (AgentPhase::Working, Some(next.id))
    );

    // The reply is delivered once per key: every client sees it once.
    let delivery = serde_json::json!({
        "vessel": "914c503c980b8f7f1750c664b5743d2c",
        "key": "ab".repeat(32),
        "request": serde_json::json!({ "task": task.id, "text": "Here is a draft." }).to_string(),
    })
    .to_string();
    let (status, body) = agent_ask(&socket, "POST", "/say", &delivery).await;
    assert_eq!(status, 200, "{body}");
    let reply = laptop.event().await;
    assert_eq!(reply.kind, EventKind::AgentText);
    let text: flare_core::session::AgentText =
        serde_json::from_value(reply.payload.clone()).expect("agent text");
    assert_eq!(
        (text.task, text.text.as_str()),
        (task.id, "Here is a draft.")
    );
    assert!(body.contains(&format!("\"event\":{}", reply.seq)), "{body}");
    let (status, again) = agent_ask(&socket, "POST", "/say", &delivery).await;
    assert_eq!((status, again), (200, body));

    // A malformed reply is refused for good; a task is never a reply.
    let bad = serde_json::json!({ "vessel": "00".repeat(16), "key": "cd".repeat(32), "request": "hello" })
        .to_string();
    assert_eq!(agent_ask(&socket, "POST", "/say", &bad).await.0, 422);

    // Nothing else was recorded: the next event is the next say.
    laptop
        .command("s-3", Verb::Say, serde_json::json!({ "text": "third" }))
        .await;
    assert_eq!(laptop.event().await.seq, reply.seq + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn say_is_refused_where_the_agent_does_not_listen_and_to_strangers() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = start(&dir.path().join("server"), started_log(), None).await;
    let me = my_uid(dir.path());
    let socket = dir.path().join("agent.sock");
    // Neither tasks nor replies are this uid's.
    agent_socket(running.server.clone(), &socket, me + 1, me + 2);

    let (mut other, _) = Client::attach(&running.url, "elsewhere", None, "laptop").await;
    other
        .command("s-1", Verb::Say, serde_json::json!({ "text": "hello" }))
        .await;
    let refused = other.event().await;
    assert_eq!(refused.kind, EventKind::Error);
    assert!(
        refused
            .payload
            .to_string()
            .contains("listens in session main"),
        "{refused:?}"
    );

    // A version 1 client cannot say.
    let (socket_v1, _) = tokio_tungstenite::connect_async(&running.url)
        .await
        .expect("connects");
    let mut v1 = Client {
        socket: socket_v1,
        name: "old".to_owned(),
    };
    v1.send(&ClientMessage::Attach {
        session: "main".to_owned(),
        after: None,
        client: "old".to_owned(),
        version: None,
    })
    .await;
    match v1.receive().await {
        ServerMessage::Attached { version, .. } => assert_eq!(version, 1),
        other => panic!("not attached: {other:?}"),
    }
    v1.command("s-1", Verb::Say, serde_json::json!({ "text": "hello" }))
        .await;
    let refused = v1.event().await;
    assert!(
        refused.payload.to_string().contains("version 2"),
        "{refused:?}"
    );

    assert_eq!(agent_ask(&socket, "GET", "/task?wait=0", "").await.0, 403);
    let delivery = serde_json::json!({
        "vessel": "00".repeat(16), "key": "ab".repeat(32),
        "request": serde_json::json!({ "task": 0, "text": "x" }).to_string(),
    })
    .to_string();
    assert_eq!(agent_ask(&socket, "POST", "/say", &delivery).await.0, 403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_action_waits_once_settles_once_and_only_an_approval_is_relayed_as_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = start(&dir.path().join("server"), started_log(), None).await;
    let me = my_uid(dir.path());
    let socket = dir.path().join("agent.sock");
    agent_socket(running.server.clone(), &socket, me, me);
    let (mut laptop, _) = Client::attach(&running.url, "main", None, "laptop").await;

    let verse = island_core::Verse::new(Evidence::Decision, "agreed", Vec::new());
    let request = island_client::actions::verse_request("flare", &verse);
    let hash = island_client::approval::request_hash(island_client::actions::VERSE, &request);
    let report = |state: &str, entry: Option<u64>| {
        serde_json::json!({
            "vessel": "914c503c980b8f7f1750c664b5743d2c", "op": 7, "provider": "island.verse",
            "request_hash": island_client::approval::hex(&hash),
            "request": island_client::approval::hex(&request),
            "state": state, "entry": entry, "why": null,
        })
        .to_string()
    };
    let (status, body) = agent_ask(&socket, "POST", "/action", &report("waiting", None)).await;
    assert_eq!(status, 200, "{body}");
    let waiting = laptop.event().await;
    assert_eq!(waiting.kind, EventKind::ActionWaiting);
    assert_eq!(
        waiting.payload["shown"],
        "a verse on flare: [decision] \"agreed\""
    );
    // Told again (a sender restarted): no second event.
    let (_, again) = agent_ask(&socket, "POST", "/action", &report("waiting", None)).await;
    assert_eq!(again, "{\"event\":null}");
    let (status, _) = agent_ask(&socket, "POST", "/action", &report("sent", Some(42))).await;
    assert_eq!(status, 200);
    let settled = laptop.event().await;
    assert_eq!(settled.kind, EventKind::ActionSettled);
    assert_eq!(
        (
            settled.payload["settled"].as_str(),
            settled.payload["entry"].as_u64()
        ),
        (Some("sent"), Some(42))
    );
    let (_, again) = agent_ask(&socket, "POST", "/action", &report("sent", Some(42))).await;
    assert_eq!(again, "{\"event\":null}");
    assert_eq!(
        agent_ask(&socket, "POST", "/action", &report("lost", None))
            .await
            .0,
        422
    );

    // `approve` relays an approval, and nothing else.
    laptop
        .command("a-1", Verb::Approve, proposal(1, "not an approval"))
        .await;
    let refused = laptop.event().await;
    assert_eq!(refused.kind, EventKind::Error);
    assert!(
        refused.payload.to_string().contains("not an approval"),
        "{refused:?}"
    );
    let approval = Writer::new(broker::device()).submission(
        island_core::StreamId::from(island_core::StreamId::APPROVALS),
        1,
        island_core::Body::Approval(island_core::Approval {
            request: hash,
            verdict: island_core::Answered::Decline,
            proof: None,
        }),
    );
    let args = {
        let request = codec::append_request(&approval.submission, &approval.sig_writer);
        serde_json::to_value(ProposeArgs {
            submission: request.submission,
            sig_writer: request.sig_writer,
        })
        .expect("JSON")
    };
    // A decline is not an approve.
    laptop.command("a-2", Verb::Approve, args.clone()).await;
    assert_eq!(laptop.event().await.kind, EventKind::Error);
    laptop.command("a-3", Verb::Decline, args).await;
    let call = call(&laptop.event().await);
    assert!(
        call.what.starts_with(&format!(
            "decline request {}",
            &island_client::approval::hex(&hash)[..8]
        )),
        "{}",
        call.what
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forks_note_is_an_event_once_and_only_from_the_island_socket() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = start(&dir.path().join("server"), started_log(), None).await;
    let me = my_uid(dir.path());
    let socket = dir.path().join("agent.sock");
    agent_socket(running.server.clone(), &socket, me, me);
    let (mut laptop, _) = Client::attach(&running.url, "main", None, "laptop").await;

    let note = |text: &str, done: bool| {
        serde_json::json!({ "child": "flare-home-fork-11", "text": text, "done": done }).to_string()
    };
    let (status, body) = agent_ask(&socket, "POST", "/note", &note("started", false)).await;
    assert_eq!(status, 200, "{body}");
    let event = laptop.event().await;
    assert_eq!(event.kind, EventKind::ForkNote);
    assert_eq!(
        (
            event.payload["child"].as_str(),
            event.payload["text"].as_str(),
            event.payload["done"].as_bool()
        ),
        (Some("flare-home-fork-11"), Some("started"), Some(false))
    );
    // Posted again (the island socket was not answered): no second event.
    let (_, again) = agent_ask(&socket, "POST", "/note", &note("started", false)).await;
    assert_eq!(again, "{\"event\":null}");
    // The child is done: a new event; another child's same words too.
    let (_, done) = agent_ask(
        &socket,
        "POST",
        "/note",
        &note("a draft, in draft.md", true),
    )
    .await;
    assert_ne!(done, "{\"event\":null}");
    assert_eq!(laptop.event().await.payload["done"], true);
    let other = note("a draft, in draft.md", true).replace("fork-11", "fork-12");
    let (_, other) = agent_ask(&socket, "POST", "/note", &other).await;
    assert_ne!(other, "{\"event\":null}");
    // Too long, or not a note: refused for good.
    let long = note(&"x".repeat(flare_core::session::MAX_NOTE + 1), true);
    assert_eq!(agent_ask(&socket, "POST", "/note", &long).await.0, 422);

    // Only the island socket's user posts notes.
    let other_socket = dir.path().join("agent-2.sock");
    agent_socket(running.server.clone(), &other_socket, me + 1, me);
    assert_eq!(
        agent_ask(&other_socket, "POST", "/note", &note("x", false))
            .await
            .0,
        403
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forks_show_in_the_agents_state_and_a_new_client_is_told_where_things_stand() {
    let dir = tempfile::tempdir().expect("temp dir");
    let running = start(&dir.path().join("server"), started_log(), None).await;
    let me = my_uid(dir.path());
    let socket = dir.path().join("agent.sock");
    agent_socket(running.server.clone(), &socket, me, me);
    let (mut laptop, _) = Client::attach(&running.url, "main", None, "laptop").await;

    // Two actions wait; one of them is settled.
    let report = |text: &str, state: &str| {
        let verse = island_core::Verse::new(Evidence::Decision, text, Vec::new());
        let request = island_client::actions::verse_request("flare", &verse);
        let hash = island_client::approval::request_hash(island_client::actions::VERSE, &request);
        serde_json::json!({
            "vessel": "914c503c980b8f7f1750c664b5743d2c", "op": 7, "provider": "island.verse",
            "request_hash": island_client::approval::hex(&hash),
            "request": island_client::approval::hex(&request),
            "state": state, "entry": null, "why": null,
        })
        .to_string()
    };
    for (text, state) in [
        ("still waits", "waiting"),
        ("answered", "waiting"),
        ("answered", "declined"),
    ] {
        let (status, body) = agent_ask(&socket, "POST", "/action", &report(text, state)).await;
        assert_eq!(status, 200, "{body}");
    }
    let waits = laptop.event().await;
    assert_eq!(waits.kind, EventKind::ActionWaiting);
    laptop.events(2).await;

    // A task, and two children of the agent at work on it.
    laptop
        .command("s-1", Verb::Say, serde_json::json!({ "text": "two ways" }))
        .await;
    let said = laptop.event().await;
    assert_eq!(agent_ask(&socket, "GET", "/task", "").await.0, 200);
    assert_eq!(
        state(&laptop.event().await),
        (AgentPhase::Working, Some(said.seq))
    );
    let note = |child: &str, text: &str| {
        serde_json::json!({ "child": child, "text": text, "done": false }).to_string()
    };
    let children = |event: &Event| {
        let state: AgentState = serde_json::from_value(event.payload.clone()).expect("a state");
        assert_eq!(
            (state.state, state.task),
            (AgentPhase::Forked, Some(said.seq))
        );
        state.children
    };
    agent_ask(&socket, "POST", "/note", &note("home-fork-11", "started")).await;
    assert_eq!(laptop.event().await.kind, EventKind::ForkNote);
    assert_eq!(children(&laptop.event().await), ["home-fork-11"]);
    agent_ask(&socket, "POST", "/note", &note("home-fork-12", "started")).await;
    assert_eq!(laptop.event().await.kind, EventKind::ForkNote);
    let forked = laptop.event().await;
    assert_eq!(children(&forked), ["home-fork-11", "home-fork-12"]);
    // A child's next note changes nothing about the agent.
    agent_ask(&socket, "POST", "/note", &note("home-fork-11", "half")).await;
    assert_eq!(laptop.event().await.kind, EventKind::ForkNote);

    // Much later, a client attaches without a cursor: the newest events,
    // and before them what still stands, however old: the action that
    // waits (not the one answered) and the agent's state.
    for round in 0..RECENT {
        laptop.see(&format!("c-{round}"), None).await;
        laptop.events(2).await;
    }
    let (mut phone, next) = Client::attach(&running.url, "main", None, "phone").await;
    let told = phone.events(2 + RECENT as usize).await;
    assert_eq!(
        (told[0].seq, told[0].kind),
        (waits.seq, EventKind::ActionWaiting)
    );
    assert_eq!(told[1], forked);
    assert_eq!(told[2].seq, next - RECENT);
    assert!(told.windows(2).all(|pair| pair[0].seq < pair[1].seq));

    // With a cursor, only what came after it.
    let (mut again, _) = Client::attach(&running.url, "main", Some(next - 3), "phone").await;
    assert_eq!(again.event().await.seq, next - 2);
    running.serving.abort();
}

/// One plain HTTP request to the server: the status, the headers (lower
/// case) and the body.
async fn http_get(url: &str, target: &str, host: &str) -> (u16, String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let address = url.trim_start_matches("ws://").trim_end_matches("/session");
    let mut stream = TcpStream::connect(address).await.expect("connects");
    let request = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("sent");
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.expect("read");
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let (head, body) = answer.split_once("\r\n\r\n").expect("a head and a body");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    (status, head.to_ascii_lowercase(), body.to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_face_is_served_from_the_server_and_only_its_page_may_attach() {
    let dir = tempfile::tempdir().expect("temp dir");
    let face = dir.path().join("face");
    std::fs::create_dir_all(face.join("assets")).expect("the face");
    std::fs::write(
        face.join("index.html"),
        "<!doctype html><title>Flare</title>",
    )
    .expect("page");
    std::fs::write(face.join("assets/app-1a2b.js"), "console.log(1)").expect("script");
    std::fs::write(dir.path().join("secret.txt"), "not the face's").expect("a file outside");
    std::os::unix::fs::symlink(dir.path().join("secret.txt"), face.join("leak.txt"))
        .expect("a link out");
    let running = start_with(&dir.path().join("server"), started_log(), None, Some(&face)).await;
    let address = running
        .url
        .trim_start_matches("ws://")
        .trim_end_matches("/session")
        .to_owned();
    let get = |target: &'static str| http_get(&running.url, target, &address);

    // The page, at / and at each of its own routes; its files.
    // /session is the WebSocket and, loaded by a browser, the page's own
    // Session screen.
    for route in ["/", "/session", "/session-view", "/island/ledger"] {
        let (status, head, body) = get(route).await;
        assert_eq!(status, 200, "{route}");
        assert!(head.contains("content-type: text/html"), "{head}");
        assert!(head.contains("cache-control: no-cache"), "{head}");
        assert!(
            head.contains(&format!("connect-src 'self' ws://{address};")),
            "{head}"
        );
        assert!(body.contains("<title>Flare</title>"));
    }
    let (status, head, body) = get("/assets/app-1a2b.js").await;
    assert_eq!((status, body.as_str()), (200, "console.log(1)"));
    assert!(head.contains("content-type: text/javascript"), "{head}");
    assert!(head.contains("immutable"), "{head}");
    assert!(head.contains("x-content-type-options: nosniff"), "{head}");
    // Nothing that is not there, nothing outside, nothing by a link out.
    for target in ["/assets/none.js", "/../secret.txt", "/leak.txt", "/.env"] {
        assert_eq!(get(target).await.0, 404, "{target}");
    }
    // Only by this server's names.
    assert_eq!(
        http_get(&running.url, "/", "flare.evil.example").await.0,
        403
    );
    assert_eq!(
        http_get(&running.url, "/log", "flare.evil.example").await.0,
        403
    );

    // The log, as the page reads it: the head and the newest entries.
    let (status, head, body) = get("/log?entries=1").await;
    assert_eq!(status, 200, "{body}");
    assert!(head.contains("cache-control: no-store"), "{head}");
    assert!(!head.contains("access-control-allow-origin"), "{head}");
    let log: serde_json::Value = serde_json::from_str(&body).expect("json");
    let entries = running.log.lock().expect("the log").records.len() as u64;
    assert_eq!(log["entries"], entries);
    assert_eq!(log["head"]["seq"], entries - 1);
    assert_eq!(log["problem"], serde_json::Value::Null);
    assert_eq!(log["recent"].as_array().map(Vec::len), Some(1));
    assert_eq!(log["recent"][0]["seq"], entries - 1);
    assert_eq!(log["recent"][0]["writer"].as_str().map(str::len), Some(64));

    // Narrowed: by kind, by stream, and to what is older than an entry.
    let seqs = |body: &str| {
        let log: serde_json::Value = serde_json::from_str(body).expect("json");
        log["recent"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry["seq"].as_u64().expect("a seq"))
            .collect::<Vec<_>>()
    };
    assert_eq!(seqs(&get("/log?kind=grant").await.2), [3]);
    assert_eq!(seqs(&get("/log?kind=policy").await.2), [2]);
    assert_eq!(seqs(&get("/log?stream=authority&before=3").await.2), [1, 0]);
    assert_eq!(seqs(&get("/log?stream=island").await.2), [] as [u64; 0]);

    // What the log amounts to: the streams, every grant with its role,
    // the policy, and who wrote.
    let (status, head, body) = get("/log/state").await;
    assert_eq!(status, 200, "{body}");
    assert!(head.contains("cache-control: no-store"), "{head}");
    let state: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(state["entries"], entries);
    assert_eq!(state["head"]["seq"], entries - 1);
    assert_eq!(state["problem"], serde_json::Value::Null);
    assert_eq!(state["state_hash"].as_str().map(str::len), Some(64));
    assert_eq!(state["kinds"]["grant"], 1);
    assert_eq!(state["kinds"]["genesis"], 1);
    let streams: Vec<&str> = state["streams"]
        .as_array()
        .expect("streams")
        .iter()
        .filter_map(|stream| stream["name"].as_str())
        .collect();
    assert!(streams.contains(&"authority"), "{streams:?}");
    let grants = state["grants"].as_array().expect("grants");
    let roles: Vec<&str> = grants
        .iter()
        .filter_map(|grant| grant["role"].as_str())
        .collect();
    assert!(
        roles.contains(&"root") && roles.contains(&"appender"),
        "{roles:?}"
    );
    let device = grants.last().expect("the device's grant");
    assert_eq!(
        (&device["id"], &device["seq"], &device["role"]),
        (
            &serde_json::json!("3.0"),
            &serde_json::json!(3),
            &serde_json::json!("ordinary")
        )
    );
    assert_eq!(device["scope"], serde_json::json!(["island"]));
    assert_eq!(device["rights"], serde_json::json!(["verse"]));
    assert_eq!(device["by"], state["root"]);
    assert_eq!(device["ended_at"], serde_json::Value::Null);
    assert_eq!(
        state["appender"],
        grants
            .iter()
            .find(|grant| grant["role"] == "appender")
            .expect("the appender's grant")["holder"]
    );
    let policy = state["policy"].as_array().expect("policy");
    assert_eq!(policy.len(), 1);
    assert_eq!(policy[0]["seq"], 2);
    assert!(
        !policy[0]["invariants"]
            .as_array()
            .expect("invariants")
            .is_empty()
    );
    assert_eq!(state["writers"][0]["key"], state["root"]);
    assert_eq!(state["writers"][0]["entries"], entries);
    assert_eq!(
        http_get(&running.url, "/log/state", "flare.evil.example")
            .await
            .0,
        403
    );

    // A page of another site may not even ask.
    for (site, status) in [
        ("cross-site", 403),
        ("same-site", 403),
        ("same-origin", 200),
    ] {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = TcpStream::connect(&address).await.expect("connects");
        let request = format!(
            "GET /log HTTP/1.1\r\nHost: {address}\r\nSec-Fetch-Site: {site}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.expect("sent");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.expect("read");
        assert!(
            answer.starts_with(&format!("HTTP/1.1 {status}")),
            "{site}: {answer}"
        );
    }

    // The server's own page may attach; any other page may not.
    let upgrade = |origin: String| {
        let url = running.url.replace("ws://", "http://");
        let host = address.clone();
        async move { reqwest_like(&url, &host, Some(&origin)).status().await }
    };
    assert_eq!(upgrade(format!("http://{address}")).await, 101);
    assert_eq!(upgrade("https://evil.example".to_owned()).await, 403);
    assert_eq!(upgrade(format!("https://{address}")).await, 403);
    assert_eq!(upgrade(format!("http://{address}.evil.example")).await, 403);
    assert_eq!(upgrade("null".to_owned()).await, 403);
    running.serving.abort();

    // Without a face there is no page, no /log, and no page may attach.
    let plain = start(&dir.path().join("plain"), started_log(), None).await;
    let address = plain
        .url
        .trim_start_matches("ws://")
        .trim_end_matches("/session")
        .to_owned();
    assert_eq!(http_get(&plain.url, "/", &address).await.0, 404);
    assert_eq!(http_get(&plain.url, "/log", &address).await.0, 404);
    assert_ne!(http_get(&plain.url, "/session", &address).await.0, 200);
    let url = plain.url.replace("ws://", "http://");
    let own = format!("http://{address}");
    assert_eq!(reqwest_like(&url, &address, Some(&own)).status().await, 403);
    plain.serving.abort();
}

/// A certificate for this machine's names, as the files a renewal
/// writes; and a client that trusts it and nothing else.
fn certificate(dir: &Path) -> tokio_rustls::TlsConnector {
    let made =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("a certificate");
    std::fs::write(dir.join("cert.pem"), made.cert.pem()).expect("the certificate");
    std::fs::write(dir.join("key.pem"), made.signing_key.serialize_pem()).expect("its key");
    let mut trusted = rustls::RootCertStore::empty();
    trusted.add(made.cert.der().clone()).expect("trusted");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_root_certificates(trusted)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(config))
}

/// One request over TLS: the answer's head, lower case; `None` if the
/// client does not trust what the server showed.
async fn over_tls(client: &tokio_rustls::TlsConnector, port: u16, request: &str) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connects");
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("a name");
    let mut stream = client.connect(name, tcp).await.ok()?;
    stream.write_all(request.as_bytes()).await.expect("sent");
    let mut answer = Vec::new();
    let mut chunk = [0_u8; 1024];
    // The head is enough; an upgraded connection never ends by itself.
    while !answer.windows(4).any(|window| window == b"\r\n\r\n") {
        match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
            Ok(Ok(read)) if read > 0 => answer.extend_from_slice(&chunk[..read]),
            _ => break,
        }
    }
    Some(String::from_utf8_lossy(&answer).to_ascii_lowercase())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_over_tls_attaches_over_tls_only_and_a_renewed_certificate_is_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    let face = dir.path().join("face");
    std::fs::create_dir_all(&face).expect("the face");
    std::fs::write(
        face.join("index.html"),
        "<!doctype html><title>Flare</title>",
    )
    .expect("page");
    let running = start_with(&dir.path().join("server"), started_log(), None, Some(&face)).await;
    let first = certificate(dir.path());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let port = tcp.local_addr().expect("an address").port();
    let secure = flare_server::tls::Listener::new(
        tcp,
        flare_server::tls::Files {
            cert: dir.path().join("cert.pem"),
            key: dir.path().join("key.pem"),
        },
    )
    .expect("the certificate is read");
    let app = flare_server::tls_router(running.server.clone());
    let serving = tokio::spawn(async move {
        let _ = axum::serve(secure, app).await;
    });
    let host = format!("127.0.0.1:{port}");
    let knock = |origin: String| {
        format!(
            "GET /session HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nOrigin: {origin}\r\n\r\n"
        )
    };
    let status = |head: Option<String>| {
        head.and_then(|head| head.split_whitespace().nth(1).map(str::to_owned))
            .unwrap_or_else(|| "no answer".to_owned())
    };

    // The page, with the session's address as it is over TLS.
    let page = over_tls(
        &first,
        port,
        &format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    )
    .await
    .expect("trusted");
    assert!(page.starts_with("http/1.1 200"), "{page}");
    assert!(
        page.contains(&format!("connect-src 'self' wss://{host};")),
        "{page}"
    );

    // Loaded over https, the page attaches over TLS; a page that says it
    // was loaded plainly does not, and nor does the https page over the
    // plain listener.
    assert_eq!(
        status(over_tls(&first, port, &knock(format!("https://{host}"))).await),
        "101"
    );
    assert_eq!(
        status(over_tls(&first, port, &knock(format!("http://{host}"))).await),
        "403"
    );
    assert_eq!(
        status(over_tls(&first, port, &knock("https://flare.example".to_owned())).await),
        "403"
    );
    let plain = running
        .url
        .trim_start_matches("ws://")
        .trim_end_matches("/session")
        .to_owned();
    let url = running.url.replace("ws://", "http://");
    assert_eq!(
        reqwest_like(&url, &plain, Some(&format!("https://{plain}")))
            .status()
            .await,
        403
    );
    // A terminal names no origin and attaches over TLS as well.
    let bare = knock(String::new()).replace("Origin: \r\n", "");
    assert_eq!(status(over_tls(&first, port, &bare).await), "101");

    // A renewal: new files. The next connection is shown the new
    // certificate, with no restart; the old one is not served any more.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let renewed = certificate(dir.path());
    let get = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    assert!(over_tls(&first, port, &get).await.is_none());
    let page = over_tls(&renewed, port, &get).await.expect("trusted");
    assert!(page.starts_with("http/1.1 200"), "{page}");

    // Files that cannot be read leave the certificate in use as it is.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(dir.path().join("cert.pem"), "not a certificate").expect("spoiled");
    let page = over_tls(&renewed, port, &get)
        .await
        .expect("still the last good one");
    assert!(page.starts_with("http/1.1 200"), "{page}");

    serving.abort();
    running.serving.abort();
}

/// The page signs approvals itself, so it encodes them itself (in
/// TypeScript, face/src/lib/approval.ts). These vectors hold both sides to
/// the same bytes: this test checks them against island-core, the page's
/// tests against its own encoder. `UPDATE_VECTORS=1` writes them anew.
#[test]
fn the_pages_approval_vectors_are_what_island_core_encodes() {
    use island_core::{Answered, Approval, Body, StreamId, Submission};
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../face/src/test/vectors/approval.json");
    let key = island_core::SigningKey::from_bytes(&[7; 32]);
    let writer = island_core::public_key(&key);
    let cases: [(u64, [u8; 32], Answered); 4] = [
        (1, [0; 32], Answered::Approve),
        (2, [0xff; 32], Answered::Decline),
        (300, std::array::from_fn(|i| i as u8), Answered::Approve),
        (
            u64::from(u32::MAX) + 5,
            std::array::from_fn(|i| (i * 7) as u8),
            Answered::Decline,
        ),
    ];
    let vectors: Vec<serde_json::Value> = cases
        .iter()
        .map(|(counter, request, verdict)| {
            let submission = Submission {
                stream: StreamId::from(StreamId::APPROVALS),
                writer,
                counter: *counter,
                body: Body::Approval(Approval {
                    request: *request,
                    verdict: *verdict,
                    proof: None,
                }),
            };
            let hash = submission.hash();
            let signature = island_core::sign_hash(&key, &hash);
            let request = codec::append_request(&submission, &signature);
            // What the server makes of it is the approval that went in.
            let (parsed, _) = codec::parse_append(&request).expect("canonical");
            assert_eq!(parsed, submission);
            serde_json::json!({
                "writer": flare_core::hex(&writer),
                "counter": counter.to_string(),
                "request": flare_core::hex(request_hash_of(&submission)),
                "approve": *verdict == Answered::Approve,
                "submission": request.submission,
                "hash": flare_core::hex(&hash),
                "signature": request.sig_writer,
            })
        })
        .collect();
    let now = serde_json::to_string_pretty(&serde_json::json!({
        "domain": island_core::SUBMISSION_DOMAIN,
        "stream": StreamId::APPROVALS,
        "seed": flare_core::hex(&[7; 32]),
        "vectors": vectors,
    }))
    .expect("json")
        + "\n";
    if std::env::var_os("UPDATE_VECTORS").is_some() {
        std::fs::write(&path, &now).expect("the vectors are written");
    }
    let kept = std::fs::read_to_string(&path).expect("face/src/test/vectors/approval.json");
    assert_eq!(
        kept, now,
        "run with UPDATE_VECTORS=1 if the encoding was meant to change"
    );
}

fn request_hash_of(submission: &island_core::Submission) -> &[u8; 32] {
    match &submission.body {
        island_core::Body::Approval(approval) => &approval.request,
        _ => unreachable!("an approval"),
    }
}
