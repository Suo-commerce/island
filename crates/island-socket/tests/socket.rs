//! `island-socket` over a real Unix socket, against a stub model provider
//! on loopback. The caller is this test process: the Warden config puts
//! its uid in a Vessel's slot (or, for the refusal, in none).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::HeaderMap;
use axum::routing::post;
use island_socket::access::call_digest;
use island_socket::callers::RANGE;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// What the stub provider was sent.
#[derive(Default)]
struct Seen {
    bodies: Vec<Bytes>,
    auth: Vec<Option<String>>,
}

/// A stub OpenAI-compatible provider: a streamed answer in `pieces`
/// pieces when asked to stream, one JSON answer otherwise.
async fn stub(pieces: usize) -> (String, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let record = seen.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, body: Bytes| {
            let record = record.clone();
            async move {
                let request: Value = serde_json::from_slice(&body).unwrap();
                {
                    let mut seen = record.lock().unwrap();
                    seen.bodies.push(body.clone());
                    seen.auth.push(
                        headers
                            .get("authorization")
                            .map(|v| v.to_str().unwrap().to_owned()),
                    );
                }
                let model = request["model"].as_str().unwrap_or("?").to_owned();
                if request["stream"].as_bool() == Some(true) {
                    let chunks = (0..pieces).map(move |i| {
                        let line = format!(
                            "data: {}\n\n",
                            json!({"model": model, "choices": [{"delta": {"content": format!("piece {i} ")}}]})
                        );
                        Ok::<Bytes, std::io::Error>(Bytes::from(line))
                    });
                    let done = std::iter::once(Ok(Bytes::from_static(b"data: [DONE]\n\n")));
                    let stream = futures_util::stream::iter(chunks.chain(done));
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from_stream(stream))
                        .unwrap()
                } else {
                    axum::response::Response::builder()
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"model": model, "choices": [{"message": {"content": "hello"}}]})
                                .to_string(),
                        ))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

fn uid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.uid()
        })
        .unwrap()
}

struct World {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    shared: Arc<island_socket::server::Shared>,
}

/// The socket, serving: this process is flare-home's prime, unless
/// `stranger`, when its uid is no Vessel's.
async fn world(provider_url: &str, budget_calls: u64, stranger: bool) -> World {
    let dir = tempfile::tempdir().unwrap();
    let me = uid();
    let base = if stranger { me + 1 } else { me - (me % RANGE) };
    let slot = if stranger { 0 } else { (me - base) / RANGE };
    let warden = dir.path().join("warden.json");
    std::fs::write(
        &warden,
        json!({
            "runtime": {"host_uid_base": base},
            "vessels": [{"namespace": "151a4d00000000000000000000000052", "subject": "flare-home",
                         "uid_slot": slot, "forks": {"slots": [slot + 1, slot + 2]}}]
        })
        .to_string(),
    )
    .unwrap();
    let key_file = dir.path().join("local.key");
    std::fs::write(&key_file, "sk-local-123\n").unwrap();
    let socket = dir.path().join("island.sock");
    let config = format!(
        "socket = {socket:?}\nwarden_config = {warden:?}\nstate_dir = {state:?}\n\
         broker = \"http://127.0.0.1:9\"\nroot = \"{root}\"\nkey = \"/nonexistent\"\n\
         default = \"local\"\n\
         [provider.local]\nbase_url = \"{provider_url}\"\nmodels = [\"small\", \"large\"]\n\
         key_file = {key_file:?}\nbudget = {{ calls = {budget_calls}, bytes = 100000000 }}\n\
         [provider.down]\nbase_url = \"http://127.0.0.1:9/v1\"\nmodels = [\"nowhere\"]\n\
         budget = {{ calls = 10, bytes = 1000 }}\n",
        state = dir.path().join("state"),
        root = "a3d2b14059647d18c5334b853e1f712e62d1ddae66f959a3224bf5130acf7e5d",
    );
    let config = island_socket::config::Config::parse(&config).unwrap();
    let shared = island_socket::shared(config).unwrap();
    let listener = island_socket::bind(&socket).unwrap();
    let app = island_socket::server::router(shared.clone())
        .into_make_service_with_connect_info::<island_socket::server::Peer>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    World {
        _dir: dir,
        socket,
        shared,
    }
}

/// One HTTP/1.1 request over the socket; the status and the body,
/// de-chunked, as the agent receives it.
async fn ask(socket: &Path, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nhost: island\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .unwrap()
        .unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
    let status: u16 = head[9..12].parse().unwrap();
    let mut rest = &raw[split + 4..];
    if !head.contains("transfer-encoding: chunked") {
        return (status, rest.to_vec());
    }
    let mut body = Vec::new();
    loop {
        let line_end = rest.windows(2).position(|w| w == b"\r\n").unwrap();
        let size =
            usize::from_str_radix(std::str::from_utf8(&rest[..line_end]).unwrap().trim(), 16)
                .unwrap();
        rest = &rest[line_end + 2..];
        if size == 0 {
            return (status, body);
        }
        body.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// The calls batched so far, after the accounting tasks ran.
async fn batched(world: &World) -> BTreeMap<String, (u32, u64, u64, [u8; 32])> {
    tokio::time::sleep(Duration::from_millis(100)).await;
    world
        .shared
        .batches
        .lock()
        .await
        .take()
        .into_iter()
        .map(|a| (a.provider, (a.calls, a.bytes_in, a.bytes_out, a.digest)))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn whoami_says_who_from_the_peer_uid() {
    let (url, _) = stub(1).await;
    let world = world(&url, 10, false).await;
    let (status, body) = ask(&world.socket, "GET", "/whoami", b"").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let me: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(me["role"], "prime");
    assert_eq!(me["subject"], "flare-home");
    assert_eq!(me["vessel"], "914c503c980b8f7f1750c664b5743d2c");
    assert_eq!(me["forks_left"], 2);
    assert_eq!(me["default_model"], "small");

    let (status, body) = ask(&world.socket, "GET", "/v1/models", b"").await;
    assert_eq!(status, 200);
    let models: Value = serde_json::from_slice(&body).unwrap();
    let ids: Vec<&str> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["nowhere", "small", "large"]);

    let (status, _) = ask(&world.socket, "GET", "/task?after=0", b"").await;
    assert_eq!(status, 404, "not served in this step");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_uid_that_is_no_vessels_is_refused() {
    let (url, seen) = stub(1).await;
    let world = world(&url, 10, true).await;
    for (method, path) in [("GET", "/whoami"), ("POST", "/v1/chat/completions")] {
        let (status, _) = ask(&world.socket, method, path, br#"{"model":"small"}"#).await;
        assert_eq!(status, 403, "{path}");
    }
    assert!(seen.lock().unwrap().bodies.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_answer_passes_through_counted_and_digested() {
    let (url, seen) = stub(50).await;
    let world = world(&url, 10, false).await;
    let request = br#"{"model":"large","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let (status, body) = ask(&world.socket, "POST", "/v1/chat/completions", request).await;
    assert_eq!(status, 200);
    let text = String::from_utf8(body.clone()).unwrap();
    assert!(
        text.contains("piece 0 ")
            && text.contains("piece 49 ")
            && text.ends_with("data: [DONE]\n\n")
    );
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.bodies[0].as_ref(),
            request,
            "the agent's bytes, as they are"
        );
        assert_eq!(
            seen.auth[0].as_deref(),
            Some("Bearer sk-local-123"),
            "the key is the socket's"
        );
    }
    let batches = batched(&world).await;
    let (calls, bytes_in, bytes_out, digest) = batches["local"];
    assert_eq!(
        (calls, bytes_in, bytes_out),
        (1, request.len() as u64, body.len() as u64)
    );
    assert_eq!(
        digest,
        island_socket::access::batch_digest(&[call_digest(request, &body)])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_model_named_is_the_default_and_an_unlisted_one_is_refused() {
    let (url, seen) = stub(1).await;
    let world = world(&url, 10, false).await;
    let (status, _) = ask(
        &world.socket,
        "POST",
        "/v1/chat/completions",
        br#"{"messages":[]}"#,
    )
    .await;
    assert_eq!(status, 200);
    let sent: Value = serde_json::from_slice(&seen.lock().unwrap().bodies[0]).unwrap();
    assert_eq!(sent["model"], "small");

    let (status, body) = ask(
        &world.socket,
        "POST",
        "/v1/chat/completions",
        br#"{"model":"gpt-9"}"#,
    )
    .await;
    assert_eq!(status, 404);
    assert!(String::from_utf8_lossy(&body).contains("no provider here lists"));
    let (status, _) = ask(&world.socket, "POST", "/v1/chat/completions", b"not json").await;
    assert_eq!(status, 400);
    assert_eq!(
        seen.lock().unwrap().bodies.len(),
        1,
        "refusals reach no provider"
    );
    assert_eq!(batched(&world).await["local"].0, 1, "and are not calls");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spent_budget_is_refused_and_counted() {
    let (url, seen) = stub(1).await;
    let world = world(&url, 2, false).await;
    let call = br#"{"model":"small"}"#;
    for _ in 0..2 {
        assert_eq!(
            ask(&world.socket, "POST", "/v1/chat/completions", call)
                .await
                .0,
            200
        );
    }
    let (status, body) = ask(&world.socket, "POST", "/v1/chat/completions", call).await;
    assert_eq!(status, 429);
    assert!(String::from_utf8_lossy(&body).contains("2 of 2 calls"));
    assert_eq!(seen.lock().unwrap().bodies.len(), 2);
    let vessel = island_env::keyfile::unhex::<16>("914c503c980b8f7f1750c664b5743d2c").unwrap();
    let spent = world.shared.usage.lock().unwrap().spent(vessel, "local");
    assert_eq!((spent.calls, spent.refused), (2, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_that_does_not_answer_is_a_counted_call() {
    let (url, _) = stub(1).await;
    let world = world(&url, 10, false).await;
    let request = br#"{"model":"nowhere"}"#;
    let (status, body) = ask(&world.socket, "POST", "/v1/chat/completions", request).await;
    assert_eq!(status, 502);
    assert!(String::from_utf8_lossy(&body).contains("down did not answer"));
    let (calls, _, bytes_out, digest) = batched(&world).await["down"];
    assert_eq!((calls, bytes_out), (1, body.len() as u64));
    assert_eq!(
        digest,
        island_socket::access::batch_digest(&[call_digest(request, &body)])
    );
}
