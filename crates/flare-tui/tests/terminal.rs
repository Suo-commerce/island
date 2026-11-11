//! `flare-tui` end to end: the program, typed at through its standard
//! input, attached to a `flare-server` with the tests' broker behind it.

#[path = "../../flare-server/tests/support/broker.rs"]
mod broker;

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use flare_server::{Config, Server, router};
use tokio::runtime::Runtime;

fn tick() -> u64 {
    1_794_400_000_000_000
}

struct Running {
    runtime: Runtime,
    broker: String,
    session: String,
    log: broker::Shared,
    dir: tempfile::TempDir,
}

fn start() -> Running {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    let dir = tempfile::tempdir().expect("temp dir");
    let log: broker::Shared = Arc::new(Mutex::new(broker::Log::started()));
    let (broker, session) = runtime.block_on(async {
        let broker = broker::serve(log.clone()).await;
        let server = Server::new(Config {
            broker: broker.clone(),
            root: broker::pin(),
            data: dir.path().join("server"),
            hosts: ["localhost", "127.0.0.1"].map(str::to_owned).to_vec(),
            clock: tick,
        })
        .expect("the server starts");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let session = format!("ws://{}/session", listener.local_addr().expect("address"));
        let app = router(server);
        tokio::spawn(async move { axum::serve(listener, app).await });
        (broker, session)
    });
    std::fs::write(dir.path().join("device.key"), flare_core::hex(&[0x11; 32])).expect("key");
    Running {
        runtime,
        broker,
        session,
        log,
        dir,
    }
}

impl Running {
    /// Run the terminal as `client`, with the device key unless the client
    /// is `phone`; type `input`, and read what it said.
    fn type_into(&self, client: &str, input: &str, more: &[&str]) -> (i32, String, String) {
        let key = self.dir.path().join("device.key");
        let replica = self.dir.path().join(format!("{client}.log"));
        let root = flare_core::hex(&broker::pin());
        let with_key = ["--key", key.to_str().expect("UTF-8")];
        let mut child = Command::new(env!("CARGO_BIN_EXE_flare-tui"))
            .args(if client == "phone" {
                &[][..]
            } else {
                &with_key[..]
            })
            .args([
                "--server",
                &self.session,
                "--client",
                client,
                "--broker",
                &self.broker,
                "--root",
                &root,
                "--replica",
                replica.to_str().expect("UTF-8"),
            ])
            .args(more)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("flare-tui starts");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("typed");
        let output = child.wait_with_output().expect("it ends");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8(output.stdout).expect("UTF-8"),
            String::from_utf8(output.stderr).expect("UTF-8"),
        )
    }
}

#[test]
fn a_terminal_sees_proposes_and_a_second_one_shows_the_same_session() {
    let running = start();

    let (code, out, error) = running.type_into(
        "laptop",
        "help\nsee\nverse decision written from the terminal\nverse closed-question typed right after\nsee island 1\nverse opinion nope\nquit\n",
        &[],
    );
    assert_eq!(code, 0, "{error}");
    assert!(
        out.contains("attached to session main: 0 events so far"),
        "{out}"
    );
    assert!(out.contains("see STREAM [N]"), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let find = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no `{needle}` in:\n{out}"))
    };
    // The overview, checked against this machine's replica.
    let seen = find("laptop: see");
    assert!(lines[seen].starts_with("#0 "), "{out}");
    assert!(
        lines[seen + 1].starts_with("#1 ") && lines[seen + 1].contains("4 entries, verified"),
        "{out}"
    );
    assert!(out.contains("✓ matches your replica at entry 3"), "{out}");
    // The verse: signed here, written there.
    let proposed = find("laptop: propose verse on island by");
    assert!(
        lines[proposed].contains("counter 1: [decision] \"written from the terminal\""),
        "{out}"
    );
    assert!(lines[proposed + 1].ends_with("written: entry 4"), "{out}");
    // Typed before the first was answered: it still gets the next counter.
    let next = find("counter 2: [closed question] \"typed right after\"");
    assert!(lines[next + 1].ends_with("written: entry 5"), "{out}");
    assert!(out.contains("✓ matches your replica at entry 5"), "{out}");
    // What is not a command is said on standard error, and nothing is sent.
    assert!(
        error.contains("`opinion` is not a kind of evidence"),
        "{error}"
    );
    assert_eq!(running.log.lock().expect("lock").records.len(), 6);

    // A second terminal, as over ssh: the same session, every event.
    let (code, out_ssh, _) = running.type_into("ssh", "", &["--after", "0"]);
    assert_eq!(code, 0);
    assert!(
        out_ssh.contains("attached to session main: 8 events so far"),
        "{out_ssh}"
    );
    for seq in 1..8 {
        assert!(
            out_ssh
                .lines()
                .any(|line| line.starts_with(&format!("#{seq} "))),
            "{out_ssh}"
        );
    }
    assert!(
        out_ssh.contains("laptop: propose verse on island"),
        "{out_ssh}"
    );
    assert!(
        !out_ssh.lines().any(|line| line.starts_with("#0 ")),
        "{out_ssh}"
    );

    // Without a key, the terminal sees and does not propose.
    let (code, _, error) = running.type_into("phone", "verse decision no key\n", &[]);
    assert_eq!(code, 0);
    assert!(error.contains("no key to sign with"), "{error}");
    let _ = &running.runtime;
}

#[test]
fn a_terminal_without_a_server_says_so() {
    let running = start();
    let root = flare_core::hex(&broker::pin());
    let output = Command::new(env!("CARGO_BIN_EXE_flare-tui"))
        .args(["--server", "ws://127.0.0.1:1/session", "--root", &root])
        .stdin(Stdio::null())
        .output()
        .expect("it ends");
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("ws://127.0.0.1:1/session"), "{error}");
    let _ = &running.dir;
}

#[test]
fn what_the_server_shows_is_checked_against_this_machines_replica() {
    let running = start();
    // The server's broker gets one verse at entry 4 ...
    {
        let mut log = running.log.lock().expect("lock");
        let verse =
            island_core::Verse::new(island_core::Evidence::Decision, "the server's", vec![]);
        let signed = island_client::Writer::new(broker::device()).verse("island", 1, verse);
        log.append(signed.submission, signed.sig_writer)
            .expect("written");
    }
    // ... and the broker this machine pulls from, another: the same root,
    // a different history from entry 4 on.
    let mut other = broker::Log::started();
    let verse = island_core::Verse::new(island_core::Evidence::Decision, "this machine's", vec![]);
    let signed = island_client::Writer::new(broker::device()).verse("island", 1, verse);
    other
        .append(signed.submission, signed.sig_writer)
        .expect("written");
    let elsewhere = running
        .runtime
        .block_on(broker::serve(Arc::new(Mutex::new(other))));

    let key = running.dir.path().join("device.key");
    let replica = running.dir.path().join("checking.log");
    let root = flare_core::hex(&broker::pin());
    let mut child = Command::new(env!("CARGO_BIN_EXE_flare-tui"))
        .args([
            "--server",
            &running.session,
            "--key",
            key.to_str().expect("UTF-8"),
            "--broker",
            &elsewhere,
            "--root",
            &root,
            "--replica",
            replica.to_str().expect("UTF-8"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("flare-tui starts");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"see island\n")
        .expect("typed");
    let output = child.wait_with_output().expect("it ends");
    let out = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(out.contains("\"the server's\""), "{out}");
    assert!(
        out.contains("✗ DOES NOT MATCH your replica: the server's entry 4 is"),
        "{out}"
    );
}
