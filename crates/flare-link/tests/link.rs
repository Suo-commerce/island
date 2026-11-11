//! The replica pull, against a broker that serves whatever the test
//! tells it to: honest, tampered, rolled back or lying (docs/spec.md,
//! definition of done: "`flare-link` pulls the log to the laptop,
//! verifies the chain and both signatures against the pinned root key,
//! and refuses a tampered record").
//!
//! The Flare half may not use the broker crate, even in tests, so the
//! broker here is a few lines of axum serving `/head` and `/log` in the
//! wire format of `island-client`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Query, State};
use flare_link::{PullError, Replica, ReplicaError, pull};
use island_client::wire::{DEFAULT_LIMIT, Head, LogQuery, MAX_LIMIT, Records, WIRE_VERSION};
use island_client::{Plain, codec};
use island_core::{
    Body, Entry, Evidence, Grant, Kind, PublicKey, Record, SigningKey, StreamId, Submission, Verse,
    entry_hash, public_key, sign_hash,
};
use island_log::{Chain, genesis};
use tokio::runtime::Runtime;

fn root() -> SigningKey {
    SigningKey::from_bytes(&[0x52; 32])
}

fn appender() -> SigningKey {
    SigningKey::from_bytes(&[0x41; 32])
}

fn writer() -> SigningKey {
    SigningKey::from_bytes(&[0x11; 32])
}

fn pin() -> PublicKey {
    public_key(&root())
}

// ── A log, built entry by entry ─────────────────────────────────────

/// A valid log, sealed here by the appender's key and checked entry by
/// entry as it grows.
#[derive(Clone)]
struct Log {
    chain: Chain,
    records: Vec<Record>,
    counter: u64,
}

impl Log {
    /// Genesis, bootstrap, the first policy, and a grant for `writer()`
    /// to write verses on `island`: entries 0 to 3.
    fn started() -> Self {
        let mut log = Self {
            chain: Chain::new(pin()),
            records: Vec::new(),
            counter: 0,
        };
        log.add(genesis::genesis(pin()), &root());
        log.add(genesis::bootstrap(pin(), public_key(&appender())), &root());
        log.add(genesis::first_policy(pin(), "test"), &root());
        let grant = Body::Grant(Grant {
            holder: public_key(&writer()),
            scope: [StreamId::from("island")].into(),
            rights: [Kind::Verse].into(),
            narrows: None,
        });
        let grant = Submission {
            stream: StreamId::from(StreamId::AUTHORITY),
            writer: pin(),
            counter: 4,
            body: grant,
        };
        log.add(grant, &root());
        log
    }

    /// The next record for `submission`, sealed honestly but not added.
    fn seal(&self, submission: Submission, signer: &SigningKey) -> Record {
        let sig_writer = sign_hash(signer, &submission.hash());
        let head = self.chain.head();
        Record::seal(
            Entry {
                seq: self.chain.next_seq(),
                at: head.map_or(1_794_000_000_000_000, |head| head.at + 1_000_000),
                policy: self.chain.policy_seq(),
                prev: self.chain.prev(),
                submission,
                sig_writer,
            },
            &appender(),
        )
    }

    fn add(&mut self, submission: Submission, signer: &SigningKey) {
        let record = self.seal(submission, signer);
        self.chain
            .apply(&record)
            .expect("the test builds a valid log");
        self.records.push(record);
    }

    fn verse_submission(&self, text: &str) -> Submission {
        Submission {
            stream: StreamId::from("island"),
            writer: public_key(&writer()),
            counter: self.counter + 1,
            body: Body::Verse(Verse::new(Evidence::Decision, text.to_owned(), Vec::new())),
        }
    }

    fn verse(&mut self, text: &str) {
        let submission = self.verse_submission(text);
        self.counter += 1;
        self.add(submission, &writer());
    }

    fn verses(mut self, count: usize, from: usize) -> Self {
        for n in from..from + count {
            self.verse(&format!("verse {n}"));
        }
        self
    }
}

// ── A broker that serves what it is told ────────────────────────────

#[derive(Default)]
struct Served {
    records: Vec<Record>,
    /// When set, `/head` says this instead of the truth.
    head: Option<(u64, [u8; 32])>,
}

type Shared = Arc<Mutex<Served>>;

async fn head(State(served): State<Shared>) -> Json<Head> {
    let served = served.lock().expect("lock");
    let (seq, hash) = served.head.unwrap_or_else(|| {
        let last = served
            .records
            .last()
            .expect("the test serves a started log");
        (last.entry.seq, last.hash)
    });
    Json(Head {
        seq,
        hash: codec::b64(&hash),
        build: "test".to_owned(),
        wire: WIRE_VERSION,
    })
}

async fn log(State(served): State<Shared>, Query(query): Query<LogQuery>) -> Json<Records> {
    let served = served.lock().expect("lock");
    // Like the real broker, by index: a record is served at the place it
    // is in the list, whatever its own seq says.
    let from = query.after.map_or(0, |after| {
        usize::try_from(after).map_or(usize::MAX, |a| a.saturating_add(1))
    });
    let rest = served.records.get(from..).unwrap_or(&[]);
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let page = &rest[..rest.len().min(limit)];
    Json(codec::records(page, page.len() < rest.len()))
}

struct Broker {
    runtime: Runtime,
    url: String,
    served: Shared,
}

impl Broker {
    fn start(records: Vec<Record>) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        let served: Shared = Arc::new(Mutex::new(Served {
            records,
            head: None,
        }));
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let router = axum::Router::new()
            .route("/head", axum::routing::get(head))
            .route("/log", axum::routing::get(log))
            .with_state(served.clone());
        runtime.spawn(async move { axum::serve(listener, router).await });
        Self {
            runtime,
            url,
            served,
        }
    }

    fn serve(&self, records: Vec<Record>) {
        let mut served = self.served.lock().expect("lock");
        served.records = records;
        served.head = None;
    }

    fn lie_about_head(&self, seq: u64, hash: [u8; 32]) {
        self.served.lock().expect("lock").head = Some((seq, hash));
    }

    /// Pull into the replica at `path`, pinned to `pin`.
    fn pull_into(&self, path: &Path, pin: PublicKey) -> Result<flare_link::Pulled, PullError> {
        let mut replica = Replica::open(path, pin).expect("the replica opens");
        let plain = Plain::new(&self.url).expect("a client");
        self.runtime.block_on(pull(&mut replica, &plain))
    }
}

fn replica_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("replica").join("island.log")
}

fn seqs(path: &Path) -> Vec<u64> {
    let replica = Replica::open(path, pin()).expect("the replica opens");
    replica
        .records()
        .iter()
        .map(|record| record.entry.seq)
        .collect()
}

// ── Pulling ─────────────────────────────────────────────────────────

#[test]
fn a_replica_is_created_verified_and_only_grows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = replica_path(&dir);
    let log = Log::started().verses(3, 0);
    let broker = Broker::start(log.records.clone());

    // The first pull creates the replica and takes everything.
    let pulled = broker.pull_into(&path, pin()).expect("pulled");
    assert_eq!(pulled.added, 7);
    assert_eq!(pulled.head, Some((6, log.records[6].hash)));

    // Nothing new: nothing added.
    let pulled = broker.pull_into(&path, pin()).expect("pulled");
    assert_eq!(
        (pulled.added, pulled.head.map(|(seq, _)| seq)),
        (0, Some(6))
    );

    // The log grows: only the new entries come.
    let log = log.verses(2, 3);
    broker.serve(log.records.clone());
    let pulled = broker.pull_into(&path, pin()).expect("pulled");
    assert_eq!(pulled.added, 2);

    // The file is the log, in the log's own format, record for record.
    let bytes = std::fs::read(&path).expect("read");
    let read = island_log::read(&bytes, pin()).expect("a valid log");
    assert_eq!(read.records, log.records);
    assert_eq!(read.chain, log.chain);
}

#[test]
fn a_long_log_comes_in_pages() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = replica_path(&dir);
    let log = Log::started().verses(1_200, 0);
    let broker = Broker::start(log.records.clone());
    let pulled = broker.pull_into(&path, pin()).expect("pulled");
    assert_eq!(pulled.added, 1_204);
    assert_eq!(seqs(&path), (0..1_204).collect::<Vec<_>>());
}

// ── Tampering ───────────────────────────────────────────────────────

/// Ways a broker can serve entry 6 of a log otherwise valid.
fn tampered(log: &Log) -> Vec<(&'static str, Vec<Record>, &'static str)> {
    let honest = log.records.clone();
    let at = 6;
    let before = Log {
        chain: island_log::replay(pin(), &honest[..at]).expect("valid"),
        records: honest[..at].to_vec(),
        counter: 2,
    };
    let with = |record: Record| {
        let mut records = honest.clone();
        records[at] = record;
        records
    };
    let text_changed = |record: &mut Record| {
        let Body::Verse(verse) = &mut record.entry.submission.body else {
            panic!("entry 6 is a verse");
        };
        verse.text = "something else happened".to_owned();
    };

    let mut cases = Vec::new();

    // The text changed in place, hash left as it was.
    let mut record = honest[at].clone();
    text_changed(&mut record);
    cases.push(("text changed", with(record), "stored hash"));

    // The text changed and the hash made to match: the writer's
    // signature is over the old text.
    let mut record = honest[at].clone();
    text_changed(&mut record);
    record.hash = entry_hash(&record.entry);
    cases.push((
        "text changed, hash recomputed",
        with(record),
        "writer signature",
    ));

    // The time moved and the hash made to match: the appender's
    // signature is over the old hash.
    let mut record = honest[at].clone();
    record.entry.at += 1;
    record.hash = entry_hash(&record.entry);
    cases.push((
        "time moved, hash recomputed",
        with(record),
        "appender signature",
    ));

    // The entry as it was, sealed again by a key that is not the log's
    // appender.
    let resealed = Record::seal(
        honest[at].entry.clone(),
        &SigningKey::from_bytes(&[0x99; 32]),
    );
    cases.push((
        "sealed by another appender",
        with(resealed),
        "appender signature",
    ));

    // The real appender's key, used to put words in the writer's mouth.
    let mut forged = before.verse_submission("something the writer never said");
    forged.counter = 3;
    let mut record = before.seal(forged, &writer());
    record.entry.sig_writer = honest[at].entry.sig_writer;
    let record = Record::seal(record.entry, &appender());
    cases.push(("writer signature forged", with(record), "writer signature"));

    // A verse the gate refuses, sealed and served as an entry.
    let mut unfounded = before.verse_submission("trust me");
    unfounded.counter = 3;
    if let Body::Verse(verse) = &mut unfounded.body {
        verse.evidence = u8::MAX;
    }
    cases.push((
        "gate refusal smuggled in",
        with(before.seal(unfounded, &writer())),
        "VerseNamesEvidence",
    ));

    // A key with no grant.
    let stranger = SigningKey::from_bytes(&[0x66; 32]);
    let mut theirs = before.verse_submission("no grant for this");
    theirs.writer = public_key(&stranger);
    theirs.counter = 1;
    cases.push(("no grant", with(before.seal(theirs, &stranger)), "grant"));

    // Entry 6 left out: 7 served in its place.
    let mut gap = honest.clone();
    gap.remove(at);
    cases.push(("an entry left out", gap, "in its place"));

    // Entries 6 and 7 swapped.
    let mut swapped = honest.clone();
    swapped.swap(at, at + 1);
    cases.push(("two entries swapped", swapped, "in its place"));

    cases
}

#[test]
fn a_tampered_record_is_refused_and_nothing_is_added() {
    let log = Log::started().verses(6, 0);
    assert_eq!(log.records.len(), 10);
    for (name, served, why) in tampered(&log) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = replica_path(&dir);

        // The replica holds entries 0 to 4 already.
        let broker = Broker::start(log.records[..5].to_vec());
        broker.pull_into(&path, pin()).expect("pulled");
        let before = std::fs::read(&path).expect("read");

        // Entries 5 to 9 come in one batch with entry 6 tampered: the
        // whole batch is refused, entry 5 too.
        broker.serve(served);
        // A head that matches what is served, so only the record is wrong.
        let last = broker.served.lock().expect("lock").records.last().cloned();
        if let Some(last) = last {
            broker.lie_about_head(9, last.hash);
        }
        let error = broker.pull_into(&path, pin()).expect_err(name);
        let PullError::Refused(refused) = &error else {
            panic!("{name}: {error}");
        };
        assert_eq!(refused.seq, 6, "{name}: {refused}");
        assert!(refused.why.contains(why), "{name}: {refused}");
        assert!(error.to_string().starts_with("REFUSED: "), "{name}");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            before,
            "{name}: the replica changed"
        );

        // The honest log is taken afterwards, as if nothing had happened.
        broker.serve(log.records.clone());
        assert_eq!(broker.pull_into(&path, pin()).expect(name).added, 5);
    }
}

#[test]
fn a_broker_that_goes_back_or_rewrites_is_refused() {
    let log = Log::started().verses(4, 0);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = replica_path(&dir);
    let broker = Broker::start(log.records.clone());
    broker.pull_into(&path, pin()).expect("pulled");
    let before = std::fs::read(&path).expect("read");
    let refused = |broker: &Broker| {
        let error = broker.pull_into(&path, pin()).expect_err("refused");
        assert!(
            matches!(error, PullError::Diverged(_) | PullError::Refused(_)),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).expect("read"), before);
        error.to_string()
    };

    // Rolled back: the broker's log is shorter than the replica.
    broker.serve(log.records[..6].to_vec());
    assert!(refused(&broker).contains("ends at entry 5, before the replica's head at entry 7"));

    // Rewritten at the head: as long, but its last entry is another.
    let mut other = Log {
        chain: island_log::replay(pin(), &log.records[..7]).expect("valid"),
        records: log.records[..7].to_vec(),
        counter: 3,
    };
    other.verse("a different last verse");
    broker.serve(other.records.clone());
    assert!(refused(&broker).contains("entry 7 is not the replica's"));

    // Rewritten further back, and longer: the first new entry does not
    // link to the replica's head.
    let other = other.verses(3, 10);
    broker.serve(other.records.clone());
    let error = refused(&broker);
    assert!(
        error.contains("entry 8") && error.contains("prev"),
        "{error}"
    );
}

#[test]
fn a_head_that_lies_is_refused() {
    let log = Log::started().verses(4, 0);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = replica_path(&dir);
    let broker = Broker::start(log.records.clone());

    // A head beyond what /log serves.
    broker.lie_about_head(20, [7; 32]);
    let error = broker.pull_into(&path, pin()).expect_err("refused");
    assert!(
        error.to_string().contains("stops before entry 20"),
        "{error}"
    );

    // A head at a position /log serves, with another hash.
    broker.lie_about_head(5, [7; 32]);
    let error = broker.pull_into(&path, pin()).expect_err("refused");
    assert!(
        error.to_string().contains("not the one its /log serves"),
        "{error}"
    );
    assert!(seqs(&path).is_empty());
}

#[test]
fn a_log_under_another_root_is_refused_from_genesis() {
    let log = Log::started();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = replica_path(&dir);
    let broker = Broker::start(log.records.clone());
    let other_pin = public_key(&SigningKey::from_bytes(&[0x77; 32]));
    let error = broker.pull_into(&path, other_pin).expect_err("refused");
    let PullError::Refused(refused) = error else {
        panic!("{error}");
    };
    assert_eq!(refused.seq, 0);
    assert!(
        Replica::open(&path, other_pin)
            .expect("opens")
            .records()
            .is_empty()
    );
}

// ── The file ────────────────────────────────────────────────────────

#[test]
fn the_replica_file_is_checked_held_and_repaired() {
    let log = Log::started().verses(3, 0);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = replica_path(&dir);
    let broker = Broker::start(log.records.clone());
    broker.pull_into(&path, pin()).expect("pulled");
    let whole = std::fs::read(&path).expect("read");

    // One holder at a time; a reader may look while it holds.
    let held = Replica::open(&path, pin()).expect("opens");
    assert!(matches!(
        Replica::open(&path, pin()),
        Err(ReplicaError::Busy(_))
    ));
    let snapshot = flare_link::replica::read(&path, pin()).expect("readable");
    assert_eq!(snapshot.records.len(), 7);
    drop(held);

    // A pull killed halfway leaves a record cut short: dropped on
    // opening, and pulled again.
    std::fs::write(&path, &whole[..whole.len() - 40]).expect("write");
    let replica = Replica::open(&path, pin()).expect("opens");
    assert_eq!(replica.records().len(), 6);
    drop(replica);
    assert_eq!(broker.pull_into(&path, pin()).expect("pulled").added, 1);
    assert_eq!(std::fs::read(&path).expect("read"), whole);

    // A byte changed in the file: the replica does not open.
    let mut damaged = whole.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 1;
    std::fs::write(&path, &damaged).expect("write");
    let error = Replica::open(&path, pin()).err().expect("refused");
    assert!(matches!(error, ReplicaError::Invalid(..)), "{error}");
    assert!(
        error
            .to_string()
            .contains("does not verify against the pinned root")
    );
}

// ── The program ─────────────────────────────────────────────────────

fn link(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_flare-link"))
        .args(args)
        .output()
        .expect("run flare-link");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).expect("UTF-8"),
        String::from_utf8(output.stderr).expect("UTF-8"),
    )
}

#[test]
fn pull_and_see_from_the_command_line() {
    let mut log = Log::started().verses(2, 0);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = replica_path(&dir);
    let path = path.to_str().expect("UTF-8");
    let root = flare_core::hex(&pin());
    let broker = Broker::start(log.records.clone());
    let source = |url: &str| -> Vec<String> {
        ["--broker", url, "--root", &root, "--replica", path]
            .map(str::to_owned)
            .to_vec()
    };
    let run = |command: &str, url: &str, more: &[&str]| {
        let mut args = vec![command.to_owned()];
        args.extend(source(url));
        args.extend(more.iter().map(|arg| (*arg).to_owned()));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        link(&args)
    };

    let (code, out, _) = run("pull", &broker.url, &[]);
    assert_eq!(code, 0);
    assert!(
        out.contains(&format!(
            "pulled 6 entries, verified: head entry 5, {}",
            flare_core::short(&log.records[5].hash)
        )),
        "{out}"
    );
    let (_, out, _) = run("pull", &broker.url, &[]);
    assert!(out.contains("up to date: head entry 5"), "{out}");

    // `see` pulls first: a verse written since shows.
    log.verse("written after the pull");
    broker.serve(log.records.clone());
    let (code, overview, _) = run("see", &broker.url, &[]);
    assert_eq!(code, 0);
    assert!(
        overview.starts_with("7 entries, verified from genesis against root"),
        "{overview}"
    );
    assert!(
        overview.contains("policy    entry 2: VerseNamesEvidence, VerseTextMax(4096)"),
        "{overview}"
    );
    assert!(
        overview.contains("gate      0 verdicts, 0 override requests"),
        "{overview}"
    );
    assert!(
        overview.contains("island       3 entries, newest:"),
        "{overview}"
    );
    assert!(
        overview.contains("\"written after the pull\""),
        "{overview}"
    );

    let (code, stream, _) = run("see", &broker.url, &["island", "-n", "2"]);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stream.lines().collect();
    assert_eq!(lines[0], "stream island: 3 entries, the newest 2");
    assert!(
        lines[1].contains("verse") && lines[1].contains("\"verse 1\""),
        "{stream}"
    );
    assert!(lines[2].contains("\"written after the pull\""), "{stream}");

    // The broker is down: the replica is shown as it stands, with a note.
    let (code, offline, error) = run("see", "http://127.0.0.1:1", &[]);
    assert_eq!(code, 0);
    assert!(error.contains("not pulled"), "{error}");
    assert!(offline.starts_with("7 entries"), "{offline}");

    // The broker serves a tampered record: the verified replica is shown,
    // the refusal is said, and the exit code says something is wrong.
    let mut records = log.records.clone();
    log.verse("one more");
    records.push(log.records[7].clone());
    if let Body::Verse(verse) = &mut records[7].entry.submission.body {
        verse.text = "tampered".to_owned();
    }
    broker.serve(records);
    let (code, out, error) = run("see", &broker.url, &[]);
    assert_eq!(code, 1);
    assert!(out.starts_with("7 entries"), "{out}");
    assert!(
        error.contains("REFUSED") && error.contains("entry 7"),
        "{error}"
    );
    assert!(error.contains("the broker is not to be trusted"), "{error}");
}
