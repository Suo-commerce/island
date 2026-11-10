//! The broker end to end: a real log file, the real HTTP routes on a
//! loopback port, and `island-client` on the other side (docs/spec.md,
//! "Interfaces" and build step 7).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use island_broker::{http, keeper};
use island_client::wire::{Code, Head, Records, WIRE_VERSION};
use island_client::{Broker, Writer, codec};
use island_core::{
    Body, Evidence, Grant, GrantId, Hash, Invariant, Kind, Policy, PublicKey, Record, Revoke,
    SigningKey, StreamId, Verse, public_key,
};
use island_env::{FileStorage, SystemClock};
use island_log::{Appender, genesis, replay};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn root() -> SigningKey {
    SigningKey::from_bytes(&[0x52; 32])
}

fn appender_key() -> SigningKey {
    SigningKey::from_bytes(&[0x41; 32])
}

fn pin() -> PublicKey {
    public_key(&root())
}

/// Writer `n`: a key of its own.
fn writer(n: u8) -> Writer {
    Writer::new(SigningKey::from_bytes(&[0x10 + n; 32]))
}

/// A grant for `holder`: verses and preferences on `island` and `plan`.
fn grant_for(holder: PublicKey) -> Body {
    Body::Grant(Grant {
        holder,
        scope: [StreamId::from("island"), StreamId::from(StreamId::PLAN)].into(),
        rights: [Kind::Verse, Kind::Preference].into(),
        narrows: None,
    })
}

/// A started log in a new directory: genesis, bootstrap, the first
/// policy, then one grant each for writers 0 to 3 (grants 3.0 to 6.0).
fn started_log() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("island.log");
    let storage = FileStorage::create(&path).expect("create the log");
    let (mut appender, _) =
        Appender::open(storage, SystemClock, appender_key(), pin()).expect("open");
    let root = Writer::new(root());
    let mut first = vec![
        genesis::genesis(pin()),
        genesis::bootstrap(pin(), public_key(&appender_key())),
        genesis::first_policy(pin(), "test"),
    ];
    for n in 0..4 {
        let grant = grant_for(writer(n).public());
        first.push(
            root.submission(StreamId::from(StreamId::AUTHORITY), 4 + u64::from(n), grant)
                .submission,
        );
    }
    for submission in first {
        let signed = root.submission(
            submission.stream.clone(),
            submission.counter,
            submission.body,
        );
        appender
            .submit(signed.submission, signed.sig_writer)
            .expect("accepted");
    }
    (dir, path)
}

/// A broker serving a log on a loopback port.
struct Running {
    url: String,
    port: u16,
    shutdown: CancellationToken,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    thread: std::thread::JoinHandle<()>,
}

impl Running {
    /// Start a broker on `log`; on `port`, or on any free one.
    async fn start(log: &Path, port: u16) -> Self {
        let started = keeper::start(log, appender_key(), pin(), "test-build").expect("start");
        let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
        let port = listener.local_addr().expect("address").port();
        let shutdown = CancellationToken::new();
        let hosts = http::LOOPBACK.map(str::to_owned).to_vec();
        let router = http::router(started.keeper, hosts, &shutdown);
        let server = tokio::spawn(http::serve(listener, router, shutdown.clone()));
        Self {
            url: format!("http://127.0.0.1:{port}"),
            port,
            shutdown,
            server,
            thread: started.thread,
        }
    }

    async fn client(&self) -> Broker {
        Broker::connect(&self.url).await.expect("connect")
    }

    /// Stop serving and close the log.
    async fn stop(self) {
        self.shutdown.cancel();
        self.server.await.expect("joined").expect("served");
        tokio::task::spawn_blocking(move || self.thread.join().expect("the keeper ended"))
            .await
            .expect("joined");
    }
}

fn verse(text: &str) -> Verse {
    Verse::new(Evidence::Decision, text.to_owned(), Vec::new())
}

/// The whole log, pulled and replayed from genesis against the pin.
async fn verified(broker: &Broker) -> Vec<Record> {
    let records = broker.whole_log().await.expect("the log");
    replay(pin(), &records).expect("the pulled log replays from genesis");
    records
}

// ── A verse, there and back ─────────────────────────────────────────

#[tokio::test]
async fn a_verse_travels_through_the_broker_and_comes_back() {
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    let broker = running.client().await;
    let me = writer(0);

    // What may I write, and with which counter?
    let mine = broker
        .capabilities(&me.public())
        .await
        .expect("capabilities");
    assert_eq!(mine.next_counter, 1);
    assert_eq!(mine.grants.len(), 1);
    assert_eq!(mine.grants[0].id, "3.0");
    assert_eq!(mine.grants[0].scope, ["island", "plan"]);
    assert_eq!(mine.grants[0].rights, ["verse", "preference"]);
    assert_eq!(
        (mine.grants[0].revoked_at, &mine.grants[0].replaced_by),
        (None, &None)
    );

    // Sign here, append there.
    let signed = me.verse("island", mine.next_counter, verse("the broker carries it"));
    let (seq, hash) = broker
        .append(&signed.submission, &signed.sig_writer)
        .await
        .expect("answered")
        .expect("accepted");
    assert_eq!(seq, 7);

    // It comes back on its stream, exactly as sent, at that position.
    let (records, more) = broker
        .read_stream("island", None, None)
        .await
        .expect("the stream");
    assert!(!more);
    assert_eq!(records.len(), 1);
    assert_eq!((records[0].entry.seq, records[0].hash), (seq, hash));
    assert_eq!(records[0].entry.submission, signed.submission);
    assert_eq!(records[0].entry.sig_writer, signed.sig_writer);

    // The whole log verifies from genesis, and the head is that entry.
    let all = verified(&broker).await;
    assert_eq!(all.len(), 8);
    assert_eq!(all[7], records[0]);
    let head = broker.head().await.expect("head");
    assert_eq!(
        head,
        Head {
            seq,
            hash: codec::b64(&hash),
            build: "test-build".to_owned(),
            wire: WIRE_VERSION,
        }
    );
    assert_eq!(
        broker
            .capabilities(&me.public())
            .await
            .expect("capabilities")
            .next_counter,
        2
    );

    broker.close().await;
    running.stop().await;
}

// ── Refusals ────────────────────────────────────────────────────────

#[tokio::test]
async fn refusals_come_back_with_their_reason() {
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    let broker = running.client().await;
    let me = writer(0);
    let refused = |signed: island_client::Signed| {
        let broker = &broker;
        async move {
            broker
                .append(&signed.submission, &signed.sig_writer)
                .await
                .expect("answered")
                .expect_err("refused")
        }
    };

    // The gate: a verse that names no evidence. A verdict records it.
    let mut no_evidence = verse("trust me");
    no_evidence.evidence = u8::MAX;
    let unfounded = me.verse("island", 1, no_evidence);
    let refusal = refused(unfounded.clone()).await;
    assert_eq!(refusal.code, Code::Gate);
    assert_eq!(
        refusal.reason,
        "refused by the gate: it breaks VerseNamesEvidence"
    );
    let verdict = refusal.verdict.clone().expect("a verdict entry");
    assert_eq!(verdict.seq, 7);
    let all = verified(&broker).await;
    let Body::Verdict(held) = &all[7].entry.submission.body else {
        panic!("entry 7 is not a verdict");
    };
    assert_eq!(*held.rejected, unfounded.submission);
    assert_eq!(codec::b64(&all[7].hash), verdict.hash);

    // The same again: the same refusal, and nothing added.
    assert_eq!(refused(unfounded).await, refusal);
    assert_eq!(broker.head().await.expect("head").seq, 7);

    // The counter: used, and skipped. Both say what the next one is.
    let old = refused(me.verse("island", 1, verse("again"))).await;
    assert_eq!(
        (old.code, old.next_counter),
        (Code::CounterConflict, Some(2))
    );
    let gap = refused(me.verse("island", 5, verse("ahead"))).await;
    assert_eq!((gap.code, gap.next_counter), (Code::CounterGap, Some(2)));

    // Outside the grant.
    let elsewhere = refused(me.verse("flare", 2, verse("not mine"))).await;
    assert_eq!(elsewhere.code, Code::Denied);
    assert!(
        elsewhere.reason.contains("no grant in force"),
        "{elsewhere:?}"
    );

    // A signature by another key.
    let mut forged = me.verse("island", 2, verse("forged"));
    forged.sig_writer = writer(1).verse("island", 2, verse("forged")).sig_writer;
    assert_eq!(refused(forged).await.code, Code::BadSignature);

    // A verdict from outside.
    let outside = me.submission(
        StreamId::from(StreamId::GATE),
        2,
        all[7].entry.submission.body.clone(),
    );
    assert_eq!(refused(outside).await.code, Code::VerdictFromOutside);

    // None of those took the counter or left an entry.
    assert_eq!(broker.head().await.expect("head").seq, 7);
    let fine = me.verse("island", 2, verse("fine"));
    let accepted = broker
        .append(&fine.submission, &fine.sig_writer)
        .await
        .expect("answered");
    assert_eq!(accepted.map(|(seq, _)| seq), Ok(8));

    broker.close().await;
    running.stop().await;
}

#[tokio::test]
async fn what_is_not_a_signed_submission_is_malformed() {
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    let me = writer(0);
    let signed = me.verse("island", 1, verse("canonical or nothing"));
    let good = codec::append_request(&signed.submission, &signed.sig_writer);

    // The tool, called as any MCP client would call it.
    let http = reqwest::Client::new();
    let call = |arguments: serde_json::Value| {
        let request = http.post(format!("{}/mcp", running.url));
        async move {
            let body = serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "append", "arguments": arguments},
            });
            let response = request
                .header("accept", "application/json, text/event-stream")
                .json(&body)
                .send()
                .await
                .expect("answered");
            assert!(response.status().is_success(), "{}", response.status());
            response.json::<serde_json::Value>().await.expect("JSON")
        }
    };

    // One byte too many is not the canonical form; nor is half a
    // signature, nor something that is not base64 at all.
    let mut bytes = signed.submission.encode();
    bytes.push(0);
    let cases = [
        (codec::b64(&bytes), good.sig_writer.clone()),
        (
            good.submission.clone(),
            codec::b64(&signed.sig_writer[..32]),
        ),
        ("not base64!".to_owned(), good.sig_writer.clone()),
    ];
    for (submission, sig_writer) in cases {
        let answer = call(serde_json::json!({
            "submission": submission, "sig_writer": sig_writer,
        }))
        .await;
        let result = &answer["result"];
        assert_eq!(result["isError"], true, "{answer}");
        assert_eq!(
            result["structuredContent"]["refused"]["code"], "malformed",
            "{answer}"
        );
    }

    // The good one, by the same road: `{seq, hash}` and nothing else.
    let answer = call(serde_json::to_value(&good).expect("JSON")).await;
    let result = &answer["result"];
    assert_eq!(result["isError"], false, "{answer}");
    let content = result["structuredContent"].as_object().expect("an object");
    assert_eq!(content.keys().collect::<Vec<_>>(), ["hash", "seq"]);
    assert_eq!(content["seq"], 7);

    running.stop().await;
}

// ── Root, by the same road ──────────────────────────────────────────

#[tokio::test]
async fn root_writes_through_the_broker_too() {
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    let broker = running.client().await;
    let root = Writer::new(root());
    let newcomer = writer(9);
    let authority = || StreamId::from(StreamId::AUTHORITY);
    let send = |signed: island_client::Signed| {
        let broker = &broker;
        async move {
            broker
                .append(&signed.submission, &signed.sig_writer)
                .await
                .expect("answered")
        }
    };

    // A grant, then a narrowing of it, then a revoke.
    let counter = broker
        .capabilities(&root.public())
        .await
        .expect("capabilities")
        .next_counter;
    assert_eq!(counter, 8);
    let granted = send(root.submission(authority(), counter, grant_for(newcomer.public()))).await;
    assert_eq!(granted.map(|(seq, _)| seq), Ok(7));
    let narrowed = Body::Grant(Grant {
        holder: newcomer.public(),
        scope: [StreamId::from("island")].into(),
        rights: [Kind::Verse].into(),
        narrows: Some(GrantId::new(7, 0)),
    });
    assert!(
        send(root.submission(authority(), 9, narrowed))
            .await
            .is_ok()
    );
    let revoke = Body::Revoke(Revoke {
        grant: GrantId::new(8, 0),
    });
    assert!(send(root.submission(authority(), 10, revoke)).await.is_ok());

    let theirs = broker
        .capabilities(&newcomer.public())
        .await
        .expect("capabilities");
    assert_eq!(theirs.grants.len(), 2);
    let (first, second) = (&theirs.grants[0], &theirs.grants[1]);
    assert_eq!(
        (first.id.as_str(), first.replaced_by.as_deref()),
        ("7.0", Some("8.0"))
    );
    assert_eq!(first.revoked_at, None);
    assert_eq!(
        (second.id.as_str(), second.narrows.as_deref()),
        ("8.0", Some("7.0"))
    );
    assert_eq!((second.revoked_at, &second.replaced_by), (Some(9), &None));
    assert_eq!(
        (&second.scope[..], &second.rights[..]),
        (&["island".to_owned()][..], &["verse".to_owned()][..])
    );

    // A tightening is accepted; a loosening is the gate's to refuse.
    let policy = |bytes| {
        Body::Policy(Policy {
            evaluator: "test".to_owned(),
            invariants: vec![
                Invariant::VerseNamesEvidence,
                Invariant::VerseTextMax { bytes },
            ],
        })
    };
    let policies = || StreamId::from(StreamId::POLICY);
    assert!(
        send(root.submission(policies(), 11, policy(100)))
            .await
            .is_ok()
    );
    let loosened = send(root.submission(policies(), 12, policy(4096)))
        .await
        .expect_err("refused");
    assert_eq!(loosened.code, Code::Gate);
    assert_eq!(
        loosened.reason,
        "refused by the gate: the policy would be weaker than the one in force"
    );

    // An ordinary key cannot do any of it.
    let me = writer(0);
    let denied = send(me.submission(authority(), 1, grant_for(me.public())))
        .await
        .expect_err("refused");
    assert_eq!(denied.code, Code::Denied);

    verified(&broker).await;
    broker.close().await;
    running.stop().await;
}

// ── Reading ─────────────────────────────────────────────────────────

#[tokio::test]
async fn the_log_and_a_stream_come_in_pages() {
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    let broker = running.client().await;
    let me = writer(0);
    // Twelve entries: verses on `island`, every third a preference on
    // `plan`.
    for counter in 1..=12u64 {
        let signed = if counter.is_multiple_of(3) {
            me.preference("plan", counter, "colour", &format!("{counter}"))
        } else {
            me.verse("island", counter, verse(&format!("verse {counter}")))
        };
        broker
            .append(&signed.submission, &signed.sig_writer)
            .await
            .expect("answered")
            .expect("accepted");
    }
    let all = verified(&broker).await;
    assert_eq!(all.len(), 19);
    let seqs = |records: &[Record]| records.iter().map(|r| r.entry.seq).collect::<Vec<_>>();

    // The log, five at a time.
    let (page, more) = broker.log(None, Some(5)).await.expect("a page");
    assert_eq!((seqs(&page), more), (vec![0, 1, 2, 3, 4], true));
    let (page, more) = broker.log(Some(4), Some(5)).await.expect("a page");
    assert_eq!((seqs(&page), more), (vec![5, 6, 7, 8, 9], true));
    let (page, more) = broker.log(Some(13), Some(5)).await.expect("a page");
    assert_eq!((seqs(&page), more), (vec![14, 15, 16, 17, 18], false));
    // Past the end, at the end, and with more asked for than allowed.
    for after in [18, 19, u64::MAX] {
        let (page, more) = broker.log(Some(after), None).await.expect("a page");
        assert_eq!((page.len(), more), (0, false));
    }
    let (page, more) = broker.log(None, Some(100_000)).await.expect("a page");
    assert_eq!((page.len(), more), (19, false));
    assert_eq!(page, all);

    // A stream: only its own entries, by position in the log.
    let island: Vec<u64> = all
        .iter()
        .filter(|r| r.entry.submission.stream == StreamId::from("island"))
        .map(|r| r.entry.seq)
        .collect();
    assert_eq!(island.len(), 8);
    let (page, more) = broker
        .read_stream("island", None, Some(3))
        .await
        .expect("a page");
    assert_eq!((seqs(&page), more), (island[..3].to_vec(), true));
    // `after` is a position in the log, and need not be on the stream:
    // here it is the preference between two verses.
    assert_eq!(island[1] + 2, island[2]);
    let (page, more) = broker
        .read_stream("island", Some(island[1] + 1), Some(500))
        .await
        .expect("a page");
    assert_eq!((seqs(&page), more), (island[2..].to_vec(), false));
    let (plan, _) = broker
        .read_stream("plan", None, None)
        .await
        .expect("a page");
    assert_eq!(plan.len(), 4);
    let (gate, more) = broker
        .read_stream("gate", None, None)
        .await
        .expect("a page");
    assert_eq!((gate.len(), more), (0, false));
    let (none, more) = broker
        .read_stream("no such stream", None, None)
        .await
        .expect("a page");
    assert_eq!((none.len(), more), (0, false));

    broker.close().await;
    running.stop().await;
}

// ── One order ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_writers_at_once_get_one_order() {
    const EACH: u64 = 30;
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;

    // Four writers, each with its own connection, all writing at once.
    let mut tasks = Vec::new();
    for n in 0..4u8 {
        let url = running.url.clone();
        tasks.push(tokio::spawn(async move {
            let broker = Broker::connect(&url).await.expect("connect");
            let me = writer(n);
            let mut answers: Vec<(u64, Hash)> = Vec::new();
            for counter in 1..=EACH {
                let signed = me.verse("island", counter, verse(&format!("{n}:{counter}")));
                let answer = broker
                    .append(&signed.submission, &signed.sig_writer)
                    .await
                    .expect("answered")
                    .expect("accepted");
                // An entry is readable the moment it is acknowledged.
                let (after, _) = broker
                    .log(Some(answer.0 - 1), Some(1))
                    .await
                    .expect("a page");
                assert_eq!((after[0].entry.seq, after[0].hash), answer);
                answers.push(answer);
            }
            broker.close().await;
            (me.public(), answers)
        }));
    }
    let mut acknowledged: BTreeMap<u64, (PublicKey, Hash)> = BTreeMap::new();
    for task in tasks {
        let (who, answers) = task.await.expect("joined");
        // Each writer's own entries are in the order it sent them.
        assert!(answers.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for (seq, hash) in answers {
            assert!(
                acknowledged.insert(seq, (who, hash)).is_none(),
                "seq {seq} twice"
            );
        }
    }

    // One log, valid from genesis, holding every acknowledged entry at
    // its position.
    let broker = running.client().await;
    let all = verified(&broker).await;
    assert_eq!(all.len() as u64, 7 + 4 * EACH);
    for (seq, (who, hash)) in &acknowledged {
        let record = &all[*seq as usize];
        assert_eq!((record.hash, record.entry.submission.writer), (*hash, *who));
    }
    broker.close().await;
    running.stop().await;

    // And the file on disk is that same log.
    let bytes = std::fs::read(&log).expect("read the log");
    let on_disk = island_log::read(&bytes, pin()).expect("valid");
    assert_eq!(on_disk.records, all);
}

// ── Restart ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_restart_loses_nothing_and_a_client_carries_on() {
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    let port = running.port;
    let broker = running.client().await;
    let me = writer(0);

    let first = me.verse("island", 1, verse("before the restart"));
    let answer = broker
        .append(&first.submission, &first.sig_writer)
        .await
        .expect("answered")
        .expect("accepted");
    let head = broker.head().await.expect("head");

    // While the broker is down, a call fails; nothing is half done.
    running.stop().await;
    let second = me.verse("island", 2, verse("after the restart"));
    assert!(broker.head().await.is_err());
    assert!(
        broker
            .append(&second.submission, &second.sig_writer)
            .await
            .is_err()
    );

    // Up again on the same address: the same client, without connecting
    // anew, finds everything, and a resend of an acknowledged submission
    // gets its first answer.
    let running = Running::start(&log, port).await;
    assert_eq!(broker.head().await.expect("head"), head);
    assert_eq!(
        broker
            .append(&first.submission, &first.sig_writer)
            .await
            .expect("answered"),
        Ok(answer)
    );
    let accepted = broker
        .append(&second.submission, &second.sig_writer)
        .await
        .expect("answered");
    assert_eq!(accepted.map(|(seq, _)| seq), Ok(8));
    verified(&broker).await;

    broker.close().await;
    running.stop().await;
}

#[tokio::test]
async fn a_log_is_served_by_one_broker_and_must_be_started() {
    let (dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    // The log file is locked: a second keeper cannot open it.
    let second = keeper::start(&log, appender_key(), pin(), "test-build");
    assert!(
        second.is_err_and(|error| error.contains("held by another appender")),
        "a second broker opened the log"
    );
    running.stop().await;

    // Another key than the one the log names as its appender.
    let other = keeper::start(&log, root(), pin(), "test-build");
    assert!(other.is_err());

    // A log without its first entries.
    let empty = dir.path().join("empty.log");
    drop(FileStorage::create(&empty).expect("create"));
    let unstarted = keeper::start(&empty, appender_key(), pin(), "test-build");
    assert!(
        unstarted.is_err_and(|error| error.contains("island-cli init")),
        "an unstarted log was served"
    );
}

// ── The plain paths, and who may ask ────────────────────────────────

#[tokio::test]
async fn the_plain_paths_answer_json_to_the_right_host_only() {
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    let http = reqwest::Client::new();
    let get = |path: &str| http.get(format!("{}{path}", running.url));

    let head: serde_json::Value = get("/head")
        .send()
        .await
        .expect("answered")
        .json()
        .await
        .expect("JSON");
    let keys: Vec<&String> = head.as_object().expect("an object").keys().collect();
    assert_eq!(keys, ["build", "hash", "seq", "wire"]);
    assert_eq!((&head["seq"], &head["wire"]), (&6.into(), &1.into()));

    let page: Records = get("/log?after=5&limit=10")
        .send()
        .await
        .expect("answered")
        .json()
        .await
        .expect("JSON");
    assert_eq!((page.records.len(), page.more), (1, false));
    assert_eq!(
        codec::parse_records(&page).expect("decodes")[0].entry.seq,
        6
    );

    // A query it does not know is refused, not ignored.
    for bad in [
        "/log?from=3",
        "/log?after=minus-one",
        "/log?limit=5000000000",
    ] {
        let status = get(bad).send().await.expect("answered").status();
        assert_eq!(status, 400, "{bad}");
    }
    assert_eq!(
        get("/nothing").send().await.expect("answered").status(),
        404
    );

    // Addressed by a name the broker was not told it has: refused on
    // every path, the tools included.
    for path in ["/head", "/log"] {
        let status = get(path)
            .header("host", "island.evil.example")
            .send()
            .await
            .expect("answered")
            .status();
        assert_eq!(status, 403, "{path}");
    }
    let tools = http
        .post(format!("{}/mcp", running.url))
        .header("host", "island.evil.example")
        .header("accept", "application/json, text/event-stream")
        .json(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .expect("answered");
    assert_eq!(tools.status(), 403);

    running.stop().await;
}

#[tokio::test]
async fn the_tools_describe_themselves() {
    let (_dir, log) = started_log();
    let running = Running::start(&log, 0).await;
    let http = reqwest::Client::new();
    let listed: serde_json::Value = http
        .post(format!("{}/mcp", running.url))
        .header("accept", "application/json, text/event-stream")
        .json(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .expect("answered")
        .json()
        .await
        .expect("JSON");
    let tools = listed["result"]["tools"].as_array().expect("tools");
    let required = |name: &str| {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name} in {listed}"));
        assert!(
            tool["description"]
                .as_str()
                .is_some_and(|text| text.len() > 40)
        );
        tool["inputSchema"]["required"].clone()
    };
    assert_eq!(tools.len(), 3);
    assert_eq!(
        required("append"),
        serde_json::json!(["submission", "sig_writer"])
    );
    assert_eq!(required("read_stream"), serde_json::json!(["stream"]));
    assert_eq!(required("my_capabilities"), serde_json::json!(["writer"]));

    running.stop().await;
}
