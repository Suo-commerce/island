//! The command line and the MCP helper against a running broker
//! (docs/spec.md, build step 7).
//!
//! The broker runs in this process, on a loopback port, over a log the
//! command line started; `island-cli` runs as it would from a shell.

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use common::{fails, field, hashes, hex, ok, outcome, sign_by_hand};
use island_broker::{http, keeper};
use island_core::SigningKey;
use island_env::keyfile;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

const BIN: &str = env!("CARGO_BIN_EXE_island-cli");

/// A log the command line started, with one writer key that holds grant
/// 3.0 (verses and preferences on `island` and `plan`).
struct Ground {
    _dir: tempfile::TempDir,
    log: String,
    root: String,
    root_key: String,
    appender_key: String,
    writer_key: String,
    writer: String,
    runtime: Runtime,
}

fn ground() -> Ground {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = |name: &str| {
        dir.path()
            .join(name)
            .to_str()
            .expect("UTF-8 path")
            .to_owned()
    };
    let (log, root_key, appender_key, writer_key) = (
        path("island.log"),
        path("root.key"),
        path("appender.key"),
        path("writer.key"),
    );
    let root = field(&ok(&["keygen", &root_key]), "public");
    ok(&["keygen", &appender_key]);
    let writer = field(&ok(&["keygen", &writer_key]), "public");
    let direct = [
        "--log",
        &log,
        "--appender-key",
        &appender_key,
        "--root-key",
        &root_key,
    ];
    ok(&[&["init"], &direct[..]].concat());
    let grant = [
        "--holder",
        &writer,
        "--scope",
        "island,plan",
        "--rights",
        "verse,preference",
    ];
    let out = ok(&[&["grant"], &direct[..], &grant[..]].concat());
    assert_eq!(out.trim(), "entry 3  grant 3.0");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    Ground {
        _dir: dir,
        log,
        root,
        root_key,
        appender_key,
        writer_key,
        writer,
        runtime,
    }
}

/// A broker serving a [`Ground`]'s log.
struct Served {
    url: String,
    port: u16,
    shutdown: CancellationToken,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    thread: std::thread::JoinHandle<()>,
}

impl Ground {
    /// Serve the log: on `port`, or on any free one.
    fn serve(&self, port: u16) -> Served {
        let key = keyfile::load(self.appender_key.as_ref()).expect("the appender key");
        let pin = keyfile::unhex(&self.root).expect("hex");
        let started = keeper::start(
            self.log.as_ref(),
            SigningKey::from_bytes(&key),
            pin,
            "test-build",
        )
        .expect("the keeper starts");
        let shutdown = CancellationToken::new();
        let hosts = http::LOOPBACK.map(str::to_owned).to_vec();
        let router = http::router(started.keeper, hosts, &shutdown);
        let listener = self
            .runtime
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", port)))
            .expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = self
            .runtime
            .spawn(http::serve(listener, router, shutdown.clone()));
        Served {
            url: format!("http://127.0.0.1:{port}"),
            port,
            shutdown,
            server,
            thread: started.thread,
        }
    }

    fn stop(&self, served: Served) {
        served.shutdown.cancel();
        self.runtime
            .block_on(served.server)
            .expect("joined")
            .expect("served");
        served.thread.join().expect("the keeper ended");
    }

    /// The arguments of a writer's command through a broker.
    fn as_writer<'a>(&'a self, url: &'a str) -> [&'a str; 6] {
        [
            "--broker",
            url,
            "--root",
            &self.root,
            "--key",
            &self.writer_key,
        ]
    }

    /// The arguments of root's command through a broker.
    fn as_root<'a>(&'a self, url: &'a str) -> [&'a str; 4] {
        ["--broker", url, "--root-key", &self.root_key]
    }

    /// A reading command's output, through a broker and from the file.
    fn read_both(&self, url: &str, command: &str) -> (String, String) {
        (
            ok(&[command, "--broker", url, "--root", &self.root]),
            ok(&[command, "--log", &self.log, "--root", &self.root]),
        )
    }
}

fn args<'a>(command: &'a str, base: &[&'a str], more: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec![command];
    all.extend_from_slice(base);
    all.extend_from_slice(more);
    all
}

fn refused_hash(error: &str) -> String {
    let (_, hash) = error
        .split_once("the refused submission is ")
        .unwrap_or_else(|| panic!("no refused submission named in: {error}"));
    hash.trim().to_owned()
}

// ── The command line through a broker ───────────────────────────────

#[test]
fn while_a_broker_serves_the_log_every_write_goes_through_it() {
    let ground = ground();
    let served = ground.serve(0);
    let url = served.url.as_str();
    let verse = |evidence: &str, text: &str| {
        let verse = ["--stream", "island", "--evidence", evidence, "--text", text];
        outcome(&args("verse", &ground.as_writer(url), &verse))
    };

    // The broker holds the log: writing to the file directly is refused.
    let direct = [
        "verse",
        "--log",
        &ground.log,
        "--appender-key",
        &ground.appender_key,
        "--root",
        &ground.root,
        "--key",
        &ground.writer_key,
        "--stream",
        "island",
        "--evidence",
        "decision",
        "--text",
        "around the broker",
    ];
    assert!(fails(&direct).contains("held by another appender"));

    // A verse, and a refused one: the same words as without a broker.
    let (code, out, _) = verse("decision", "through the broker");
    assert_eq!(code, 0);
    assert!(out.starts_with("entry 4  "), "{out}");
    let (code, out, error) = verse("none", "trust me");
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        error.contains("refused by the gate: it breaks VerseNamesEvidence"),
        "{error}"
    );
    assert!(error.contains("verdict at entry 5"), "{error}");
    let refused = refused_hash(&error);

    // Outside the grant: refused, and no entry.
    let elsewhere = ["--stream", "flare", "--evidence", "decision", "--text", "x"];
    let (code, _, error) = outcome(&args("verse", &ground.as_writer(url), &elsewhere));
    assert_eq!(code, 1);
    assert!(error.contains("not authorized"), "{error}");

    // An override request for the refused verse.
    let ask = [refused.as_str(), "the evidence is in the text"];
    let out = ok(&args("override", &ground.as_writer(url), &ask));
    assert!(out.starts_with(&format!(
        "entry 6  override requested for submission {refused}"
    )));

    // Root: a grant, a narrowing, a revoke.
    let holder = "44".repeat(32);
    let grant = [
        "--holder",
        &holder,
        "--scope",
        "island,flare",
        "--rights",
        "verse",
    ];
    assert_eq!(
        ok(&args("grant", &ground.as_root(url), &grant)).trim(),
        "entry 7  grant 7.0"
    );
    let narrow = [
        "--holder",
        &holder,
        "--scope",
        "island",
        "--rights",
        "verse",
        "--narrows",
        "7.0",
    ];
    assert_eq!(
        ok(&args("grant", &ground.as_root(url), &narrow)).trim(),
        "entry 8  grant 8.0"
    );
    assert_eq!(
        ok(&args("revoke", &ground.as_root(url), &["--grant", "8.0"])).trim(),
        "entry 9  revoked grant 8.0"
    );
    assert!(
        fails(&args("revoke", &ground.as_root(url), &["--grant", "8.0"]))
            .contains("grant 8.0 is no longer in force")
    );

    // A policy records the build that judges: the broker's, not this
    // command's.
    let out = ok(&args(
        "policy",
        &ground.as_root(url),
        &["--verse-text-max", "200"],
    ));
    assert!(out.starts_with("entry 10  policy in force: VerseNamesEvidence, VerseTextMax(200)"));
    assert_eq!(field(&out, "evaluator"), "test-build");
    let (code, _, error) = outcome(&args(
        "policy",
        &ground.as_root(url),
        &["--verse-text-max", "4096"],
    ));
    assert_eq!(code, 1);
    assert!(
        error.contains("the policy would be weaker than the one in force"),
        "{error}"
    );
    assert!(error.contains("verdict at entry 11"), "{error}");
    assert_eq!(verse("decision", &"x".repeat(201)).0, 1);

    // Reading through the broker is reading the same log as the file.
    for command in ["verify", "replay", "tail", "grants"] {
        let (through, direct) = ground.read_both(url, command);
        assert_eq!(through, direct, "{command}");
    }
    let (replay, _) = ground.read_both(url, "replay");
    assert!(
        replay.contains("13 entries replayed from genesis"),
        "{replay}"
    );
    assert!(
        replay.contains("verdicts           3\noverride requests  1\n"),
        "{replay}"
    );
    assert!(replay.contains("evaluator \"test-build\""), "{replay}");

    // Once the broker has stopped, the file can be written directly again.
    ground.stop(served);
    let out = ok(&direct[..direct.len() - 1]
        .iter()
        .copied()
        .chain(["direct again"])
        .collect::<Vec<_>>());
    assert!(out.starts_with("entry 13  "), "{out}");
}

#[test]
fn root_signs_by_hand_through_a_broker() {
    let ground = ground();
    let served = ground.serve(0);
    let holder = "55".repeat(32);
    let command = [
        "grant",
        "--broker",
        &served.url,
        "--root",
        &ground.root,
        "--holder",
        &holder,
        "--scope",
        "island",
        "--rights",
        "verse",
    ];

    // First run: the hash to sign, and nothing written.
    let (code, instructions, _) = outcome(&command);
    assert_eq!(code, 3);
    let hashes = hashes(&instructions);
    assert_eq!(hashes.len(), 1);
    let (verify, _) = ground.read_both(&served.url, "verify");
    assert!(verify.contains("ok: 4 entries"), "{verify}");

    // A signature by another key is refused before anything is sent.
    let wrong = hex(&sign_by_hand(&ground.writer_key, &hashes[0]));
    let (code, _, error) = outcome(&[&command[..], &["--signature", &wrong]].concat());
    assert_eq!(code, 1);
    assert!(error.contains("does not verify"), "{error}");

    let signature = hex(&sign_by_hand(&ground.root_key, &hashes[0]));
    let out = ok(&[&command[..], &["--signature", &signature]].concat());
    assert_eq!(out.trim(), "entry 4  grant 4.0");
    ground.stop(served);
}

#[test]
fn a_command_is_told_where_the_log_is_exactly_once() {
    let ground = ground();
    let read = |more: &[&str]| outcome(&[&["verify", "--root", &ground.root], more].concat());

    // Neither, and both: usage errors.
    let (code, _, error) = read(&[]);
    assert_eq!(code, 2);
    assert!(error.contains("--log"), "{error}");
    let (code, _, error) = read(&["--log", &ground.log, "--broker", "http://127.0.0.1:1"]);
    assert_eq!(code, 2);
    assert!(error.contains("cannot be used with"), "{error}");

    // Writing to the file takes the appender key.
    let verse = [
        "verse",
        "--log",
        &ground.log,
        "--root",
        &ground.root,
        "--key",
        &ground.writer_key,
        "--stream",
        "island",
        "--evidence",
        "decision",
        "--text",
        "x",
    ];
    assert!(fails(&verse).contains("give --appender-key too"));

    // A broker that is not there.
    let (code, _, error) = read(&["--broker", "http://127.0.0.1:1"]);
    assert_eq!(code, 1);
    assert!(error.contains("the broker cannot be reached"), "{error}");
}

#[test]
fn a_broker_is_believed_only_as_far_as_its_log_verifies() {
    // The broker serves a real log, but not the one this reader pinned.
    let ground = ground();
    let served = ground.serve(0);
    let other_root = "66".repeat(32);
    for command in ["verify", "replay", "tail", "grants"] {
        let error = fails(&[command, "--broker", &served.url, "--root", &other_root]);
        assert!(
            error.contains("the broker's log does not verify"),
            "{command}: {error}"
        );
        assert!(error.contains("entry 0"), "{command}: {error}");
    }
    let verse = [
        "verse",
        "--broker",
        &served.url,
        "--root",
        &other_root,
        "--key",
        &ground.writer_key,
        "--stream",
        "island",
        "--evidence",
        "decision",
        "--text",
        "x",
    ];
    assert!(fails(&verse).contains("the broker's log does not verify"));
    ground.stop(served);
}

// ── The MCP helper ──────────────────────────────────────────────────

type Session = RunningService<RoleClient, ClientConfig>;

/// Call one of the helper's tools: whether it failed, and what it said.
async fn call(session: &Session, tool: &'static str, input: serde_json::Value) -> (bool, String) {
    let serde_json::Value::Object(arguments) = input else {
        panic!("tool input is an object");
    };
    let result = session
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .unwrap_or_else(|error| panic!("{tool}: {error}"));
    let text = result
        .content
        .first()
        .and_then(|block| block.as_text())
        .map(|text| text.text.clone())
        .unwrap_or_default();
    (result.is_error == Some(true), text)
}

fn verse_input(evidence: &str, text: &str) -> serde_json::Value {
    serde_json::json!({"stream": "island", "evidence": evidence, "text": text})
}

impl Ground {
    /// Start the helper as a harness would, on standard input and output.
    async fn helper(&self, broker: &str) -> Session {
        let mut command = tokio::process::Command::new(BIN);
        command.args([
            "mcp",
            "--broker",
            broker,
            "--key",
            &self.writer_key,
            "--root",
            &self.root,
        ]);
        let transport = TokioChildProcess::new(command).expect("the helper starts");
        ClientConfig::default()
            .serve(transport)
            .await
            .expect("the session starts")
    }
}

#[test]
fn a_session_writes_and_reads_verses_through_the_helper() {
    let ground = ground();
    let served = ground.serve(0);
    ground.runtime.block_on(async {
        let session = ground.helper(&served.url).await;

        // Three tools, described for an agent. A verse is given in plain
        // fields: no key, no signature, no counter.
        let tools = session.list_all_tools().await.expect("tools");
        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, ["append_verse", "my_capabilities", "read_stream"]);
        let append = tools
            .iter()
            .find(|tool| tool.name == "append_verse")
            .expect("append_verse");
        let schema = serde_json::to_value(&append.input_schema).expect("JSON");
        let mut fields: Vec<&String> = schema["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect();
        fields.sort();
        assert_eq!(fields, ["evidence", "refs", "stream", "text"]);
        let kinds = schema.to_string();
        for kind in ["decision", "artefact", "measurement", "closed-question"] {
            assert!(kinds.contains(kind), "{kind} not in {kinds}");
        }

        // What may this key write?
        let (failed, mine) = call(&session, "my_capabilities", serde_json::json!({})).await;
        assert!(!failed, "{mine}");
        assert!(
            mine.contains(&format!("writer key  {}", ground.writer)),
            "{mine}"
        );
        assert!(
            mine.contains("may write verse preference on island plan  (in force)"),
            "{mine}"
        );
        assert!(mine.contains("next counter  1"), "{mine}");

        // A verse.
        let (failed, written) = call(
            &session,
            "append_verse",
            verse_input("decision", "the helper signs for me"),
        )
        .await;
        assert!(!failed, "{written}");
        assert!(
            written.starts_with("written: entry 4 on stream island, hash "),
            "{written}"
        );

        // An artefact with a ref.
        let mut artefact = verse_input("artefact", "step 7 is built");
        artefact["refs"] = serde_json::json!(["ab".repeat(32)]);
        let (failed, written) = call(&session, "append_verse", artefact).await;
        assert!(
            !failed && written.starts_with("written: entry 5 "),
            "{written}"
        );

        // Refused by the gate: too long for the policy. Said as a failure,
        // with what to do.
        let (failed, refused) = call(
            &session,
            "append_verse",
            verse_input("measurement", &"x".repeat(4097)),
        )
        .await;
        assert!(failed, "{refused}");
        assert!(
            refused.starts_with("refused by the gate: it breaks VerseTextMax(4096)."),
            "{refused}"
        );
        assert!(refused.contains("recorded as entry 6"), "{refused}");

        // Outside the grant.
        let mut elsewhere = verse_input("decision", "not mine to write");
        elsewhere["stream"] = "flare".into();
        let (failed, denied) = call(&session, "append_verse", elsewhere).await;
        assert!(failed && denied.contains("no grant in force"), "{denied}");
        assert!(denied.contains("my_capabilities"), "{denied}");

        // A ref that is not a hash.
        let mut bad = verse_input("artefact", "x");
        bad["refs"] = serde_json::json!(["abc"]);
        let (failed, said) = call(&session, "append_verse", bad).await;
        assert!(failed && said.contains("64 hex digits"), "{said}");

        // A kind of evidence that does not exist is not a verse at all:
        // the tool's input does not allow it, and nothing is sent.
        let none = CallToolRequestParams::new("append_verse").with_arguments(
            verse_input("none", "trust me")
                .as_object()
                .expect("an object")
                .clone(),
        );
        match session.call_tool(none).await {
            Err(_) => {}
            Ok(result) => {
                assert_eq!(result.is_error, Some(true), "{result:?}");
                let said = format!("{:?}", result.content);
                assert!(said.contains("none") || said.contains("evidence"), "{said}");
            }
        }

        // Reading: verified, newest last.
        let (failed, stream) = call(
            &session,
            "read_stream",
            serde_json::json!({"stream": "island"}),
        )
        .await;
        assert!(!failed, "{stream}");
        let lines: Vec<&str> = stream.lines().collect();
        assert_eq!(
            lines[0],
            "stream island: 2 entries, showing the newest 2; log verified from genesis (7 entries)"
        );
        assert!(
            lines[1].contains("[decision] \"the helper signs for me\""),
            "{stream}"
        );
        assert!(
            lines[2].contains("[artefact] \"step 7 is built\""),
            "{stream}"
        );
        let (_, newest) = call(
            &session,
            "read_stream",
            serde_json::json!({"stream": "island", "entries": 1}),
        )
        .await;
        assert_eq!(newest.lines().count(), 2);
        assert!(newest.contains("step 7 is built"), "{newest}");

        session.cancel().await.expect("the session ends");
    });

    // The log the helper wrote verifies, and its verses carry the
    // writer's key.
    let (replay, _) = ground.read_both(&served.url, "replay");
    assert!(
        replay.contains("7 entries replayed from genesis"),
        "{replay}"
    );
    assert!(
        replay.contains(&format!(
            "{}  counter 3, refused, verdict at entry 6",
            ground.writer
        )),
        "{replay}"
    );
    ground.stop(served);
}

#[test]
fn the_helper_serves_a_session_that_cannot_start_it() {
    let ground = ground();
    let served = ground.serve(0);

    // Started beside the harness, on a port of its own.
    let mut helper = Command::new(BIN)
        .args([
            "mcp",
            "--broker",
            &served.url,
            "--key",
            &ground.writer_key,
            "--root",
            &ground.root,
            "--listen",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("the helper starts");
    let mut lines = BufReader::new(helper.stdout.take().expect("stdout")).lines();
    let first = lines.next().expect("a line").expect("UTF-8");
    let address = first
        .strip_prefix("island-cli mcp  ")
        .unwrap_or_else(|| panic!("unexpected first line: {first}"))
        .to_owned();
    assert!(
        address.starts_with("http://127.0.0.1:") && address.ends_with("/mcp"),
        "{address}"
    );

    ground.runtime.block_on(async {
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(address.clone()),
        );
        let session = ClientConfig::default()
            .serve(transport)
            .await
            .expect("connects");
        let (failed, written) = call(
            &session,
            "append_verse",
            verse_input("decision", "from inside a sandbox"),
        )
        .await;
        assert!(
            !failed && written.starts_with("written: entry 4 "),
            "{written}"
        );
        session.cancel().await.expect("the session ends");

        // A second session meets the same helper, with the same key and
        // the next counter.
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(address.clone()),
        );
        let session = ClientConfig::default()
            .serve(transport)
            .await
            .expect("connects");
        let (_, mine) = call(&session, "my_capabilities", serde_json::json!({})).await;
        assert!(mine.contains("next counter  2"), "{mine}");
        session.cancel().await.expect("the session ends");

        // Addressed by a name it was not told it has: refused.
        let status = reqwest::Client::new()
            .post(&address)
            .header("host", "helper.evil.example")
            .header("accept", "application/json, text/event-stream")
            .json(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .send()
            .await
            .expect("answered")
            .status();
        assert_eq!(status, 403);
    });

    helper.kill().expect("stopped");
    helper.wait().expect("reaped");
    ground.stop(served);
}

#[test]
fn the_helper_outlives_a_broker_restart() {
    let ground = ground();
    let served = ground.serve(0);
    let (url, port) = (served.url.clone(), served.port);
    let session = ground.runtime.block_on(ground.helper(&url));
    let say = |text: &'static str| {
        ground.runtime.block_on(call(
            &session,
            "append_verse",
            verse_input("decision", text),
        ))
    };

    let (failed, written) = say("before the restart");
    assert!(
        !failed && written.starts_with("written: entry 4 "),
        "{written}"
    );

    // The broker is down: nothing is written, and the session is told so.
    ground.stop(served);
    let (failed, said) = say("while it is down");
    assert!(failed, "{said}");
    assert!(
        said.starts_with("not written: the broker cannot be reached"),
        "{said}"
    );

    // Up again: the same session writes on.
    let served = ground.serve(port);
    let (failed, written) = say("after the restart");
    assert!(
        !failed && written.starts_with("written: entry 5 "),
        "{written}"
    );

    ground
        .runtime
        .block_on(session.cancel())
        .expect("the session ends");
    ground.stop(served);
}

// ── An answer that never arrives ────────────────────────────────────

/// Stands between the helper and the broker. While `cut` is set, an
/// `append` call is passed on to the broker and its answer thrown away:
/// the helper sees a failure for a submission the broker has written.
#[derive(Clone)]
struct Between {
    broker: String,
    cut: Arc<AtomicBool>,
    http: reqwest::Client,
}

async fn pass(State(between): State<Between>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX)
        .await
        .expect("a body");
    let is_append = serde_json::from_slice::<serde_json::Value>(&body)
        .is_ok_and(|call| call["params"]["name"] == "append");
    let path = parts.uri.path_and_query().map_or("/", |path| path.as_str());
    let mut headers = parts.headers;
    headers.remove("host");
    let answer = between
        .http
        .request(parts.method, format!("{}{path}", between.broker))
        .headers(headers)
        .body(body)
        .send()
        .await
        .expect("the broker answers");
    if is_append && between.cut.load(Ordering::SeqCst) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let (status, headers) = (answer.status(), answer.headers().clone());
    let mut response = answer.bytes().await.expect("a body").into_response();
    *response.status_mut() = status;
    for name in ["content-type", "mcp-session-id"] {
        if let Some(value) = headers.get(name) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

#[test]
fn a_verse_whose_answer_was_lost_is_never_written_twice() {
    let ground = ground();
    let served = ground.serve(0);
    let cut = Arc::new(AtomicBool::new(false));
    let between = Between {
        broker: served.url.clone(),
        cut: cut.clone(),
        http: reqwest::Client::new(),
    };
    let listener = ground
        .runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("bind");
    let through = format!("http://{}", listener.local_addr().expect("address"));
    let proxy = ground.runtime.spawn(async move {
        let router = axum::Router::new().fallback(pass).with_state(between);
        axum::serve(listener, router).await
    });

    let session = ground.runtime.block_on(ground.helper(&through));
    let say = |text: &'static str| {
        ground.runtime.block_on(call(
            &session,
            "append_verse",
            verse_input("decision", text),
        ))
    };
    let count = |text: &str| {
        ok(&[
            "tail",
            "--broker",
            &served.url,
            "--root",
            &ground.root,
            "-n",
            "100",
        ])
        .matches(text)
        .count()
    };

    // The broker writes the verse, and the answer is lost on the way
    // back. The session is told the truth: not known.
    cut.store(true, Ordering::SeqCst);
    let (failed, said) = say("written once");
    assert!(failed, "{said}");
    assert!(
        said.contains("not known whether the verse was written"),
        "{said}"
    );
    assert!(said.contains("it will not be written twice"), "{said}");
    assert_eq!(count("written once"), 1);

    // Still cut off: the same again.
    let (failed, _) = say("written once");
    assert!(failed);
    assert_eq!(count("written once"), 1);

    // The way back is open: the same verse again gets the first answer,
    // and the log holds it once.
    cut.store(false, Ordering::SeqCst);
    let (failed, said) = say("written once");
    assert!(!failed, "{said}");
    assert!(
        said.starts_with("written: entry 4 on stream island"),
        "{said}"
    );
    assert_eq!(count("written once"), 1);

    // Lost again, and this time the session moves on to another verse:
    // it is told what became of the lost one, and both are there once.
    cut.store(true, Ordering::SeqCst);
    assert!(say("lost and left behind").0);
    cut.store(false, Ordering::SeqCst);
    let (failed, said) = say("the next thing");
    assert!(!failed, "{said}");
    assert_eq!(
        said.split_once(')').map(|(before, _)| before),
        Some("(The earlier verse that got no answer is entry 5.")
    );
    assert!(said.contains("written: entry 6 on stream island"), "{said}");
    assert_eq!(count("lost and left behind"), 1);
    assert_eq!(count("the next thing"), 1);

    ground
        .runtime
        .block_on(session.cancel())
        .expect("the session ends");
    proxy.abort();
    ground.stop(served);
}

#[test]
fn the_helper_will_not_listen_on_every_interface() {
    let ground = ground();
    let (code, _, error) = outcome(&[
        "mcp",
        "--broker",
        "http://127.0.0.1:1",
        "--key",
        &ground.writer_key,
        "--root",
        &ground.root,
        "--listen",
        "0.0.0.0:0",
    ]);
    assert_eq!(code, 1);
    assert!(
        error.contains("does not listen on every interface"),
        "{error}"
    );
}
