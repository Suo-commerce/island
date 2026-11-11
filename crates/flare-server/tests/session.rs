//! The shared session, end to end: `flare-server` on a loopback port, a
//! broker of the tests' own behind it, and clients speaking the session
//! protocol over WebSocket (docs/spec.md, build step 9).

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flare_core::session::{
    ClientMessage, Event, EventKind, ProposeArgs, RECENT, SeeArgs, ServerMessage, Verb, VerbCall,
    VerbResult,
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
            })
            .await;
        match client.receive().await {
            ServerMessage::Attached { next, version, .. } => {
                assert_eq!(version, 1);
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
