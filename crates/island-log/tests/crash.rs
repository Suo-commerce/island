//! Crash test with a real file: a writer process is killed with SIGKILL at
//! arbitrary points, again and again, and no acknowledged entry is ever
//! lost (docs/spec.md, definition of done: "The appender survives
//! `kill -9` at random points with no acknowledged entry lost; replay from
//! genesis reproduces the state hash and every verdict").
//!
//! SIGKILL takes the process, not the machine: what it had written stays
//! in the page cache. Power loss is a different fault; the harness covers
//! it with simulated storage (build step 4), and `tests/appender.rs`
//! covers a cut at every byte.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use common::{appender_key, pin, root, sign};
use island_core::{Body, Hash};
use island_env::{FileStorage, SystemClock};
use island_log::{Appender, genesis, read};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `ack <seq> <hash>` or `refused <seq> <hash>` to its parts: the entry
/// the writer was answered with, its hash, and whether it is a verdict.
/// Anything else is not an answer.
fn parse_answer(line: &str) -> Option<(u64, (String, bool))> {
    let mut words = line.strip_suffix('\n')?.split(' ');
    match (words.next()?, words.next()?, words.next()?, words.next()) {
        (answer @ ("ack" | "refused"), seq, hash, None) => {
            Some((seq.parse().ok()?, (hash.to_owned(), answer == "refused")))
        }
        _ => None,
    }
}

#[test]
fn no_acknowledged_entry_is_lost_to_kill_9() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("island.log");

    // A new log with its three first entries, written in this process:
    // the third is the first policy, which refuses a verse that names no
    // evidence.
    {
        let storage = FileStorage::create(&path).expect("create the log");
        let (mut appender, _) =
            Appender::open(storage, SystemClock, appender_key(), pin()).expect("open");
        for submission in [
            genesis::genesis(pin()),
            common::bootstrap(),
            genesis::first_policy(pin(), "test"),
        ] {
            let signature = sign(&submission, &root());
            appender.submit(submission, signature).expect("accepted");
        }
    } // dropping the appender releases the file lock

    // How many acks to wait for before each kill. 0 kills the writer while
    // it is still opening the log.
    let schedule = [0, 1, 2, 3, 5, 8, 13, 1, 0, 21, 4, 7, 0, 2, 34, 1];
    let mut acked: BTreeMap<u64, (String, bool)> = BTreeMap::new();

    for (round, wait_for) in schedule.into_iter().enumerate() {
        let mut child = Command::new(env!("CARGO_BIN_EXE_island-log-crash-writer"))
            .arg(&path)
            .arg(hex(&pin()))
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the writer");
        let mut lines = BufReader::new(child.stdout.take().expect("piped stdout"));

        let mut seen = 0;
        let mut line = String::new();
        let take = |line: &str, acked: &mut BTreeMap<u64, (String, bool)>| {
            if let Some((seq, answer)) = parse_answer(line) {
                let earlier = acked.insert(seq, answer.clone());
                assert!(
                    earlier.is_none_or(|earlier| earlier == answer),
                    "round {round}: entry {seq} was answered twice, differently"
                );
                1
            } else {
                0
            }
        };
        while seen < wait_for {
            line.clear();
            if lines.read_line(&mut line).expect("read the writer") == 0 {
                panic!("round {round}: the writer exited on its own after {seen} acks");
            }
            seen += take(&line, &mut acked);
        }

        child.kill().expect("kill -9");
        // Whatever it printed before dying was acknowledged too. A last
        // line without its newline was cut by the kill and does not count.
        loop {
            line.clear();
            if lines.read_line(&mut line).expect("drain the writer") == 0 {
                break;
            }
            take(&line, &mut acked);
        }
        let status = child.wait().expect("reap the writer");
        assert!(
            !status.success(),
            "round {round}: the writer was not killed"
        );

        // After every kill: the file is a valid log from genesis, which
        // means every verdict in it was judged again and held; and every
        // entry a writer was answered with is in it, at its position,
        // unchanged, a verdict where the answer was a refusal.
        let bytes = std::fs::read(&path).expect("read the log");
        let log = read(&bytes, pin())
            .unwrap_or_else(|error| panic!("round {round}: the log is not valid: {error}"));
        for (seq, (hash, refused)) in &acked {
            let record = log
                .records
                .get(*seq as usize)
                .unwrap_or_else(|| panic!("round {round}: acknowledged entry {seq} is gone"));
            assert_eq!(
                &hex(&record.hash),
                hash,
                "round {round}: entry {seq} changed"
            );
            assert_eq!(
                matches!(record.entry.submission.body, Body::Verdict(_)),
                *refused,
                "round {round}: entry {seq} is not what the writer was told"
            );
        }
    }
    let refusals = acked.values().filter(|(_, refused)| *refused).count();
    assert!(refusals >= 20, "only {refusals} refusals were answered");

    let waited: usize = schedule.iter().sum();
    assert!(
        acked.len() >= waited,
        "the writer acknowledged {}",
        acked.len()
    );

    // Opening for appending drops at most a torn tail, and arrives at the
    // state a plain replay of the file arrives at.
    let before = read(&std::fs::read(&path).expect("read"), pin()).expect("valid");
    let storage = FileStorage::open(&path).expect("open the log");
    let (appender, recovery) =
        Appender::open(storage, SystemClock, appender_key(), pin()).expect("open");
    assert_eq!(recovery.records as usize, before.records.len());
    assert_eq!(recovery.dropped as usize, before.torn);
    assert_eq!(appender.chain().state_hash(), before.chain.state_hash());
    let head: Hash = appender.chain().head().expect("head").hash;
    assert_eq!(head, before.records[before.records.len() - 1].hash);
}
