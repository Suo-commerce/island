//! Replay: every way a record can fail to follow the records before it,
//! named by position, and the state a valid log replays to.

mod common;

use std::collections::BTreeSet;

use common::{appender_key, pin, records, root, seal, verse, writer};
use island_core::{
    Answer, Body, Bootstrap, Genesis, Grant, Invariant, Kind, Policy, Preference, PublicKey,
    Record, Refusal, SigningKey, StreamId, Submission, Verdict, public_key,
};
use island_log::{Chain, Fault, ReplayError, genesis, replay};

fn fault(records: &[Record]) -> ReplayError {
    replay(pin(), records).expect_err("the log is refused")
}

fn at(seq: u64, fault: Fault) -> ReplayError {
    ReplayError { seq, fault }
}

/// Where the next record sits; a test changes one field to break it.
struct Position {
    seq: u64,
    at: u64,
    policy: u64,
    prev: [u8; 32],
}

/// A valid log, a submission to add, and who signs it: the record that
/// would come next, with every field right unless `change` says otherwise.
fn next(
    log: &[Record],
    submission: Submission,
    key: &SigningKey,
    change: impl FnOnce(&mut Position),
) -> Record {
    let chain = replay(pin(), log).expect("a valid log");
    let head = chain.head().expect("not empty");
    let mut position = Position {
        seq: head.seq + 1,
        at: head.at,
        policy: chain.policy_seq(),
        prev: head.hash,
    };
    change(&mut position);
    seal(
        position.seq,
        position.at,
        position.policy,
        position.prev,
        submission,
        key,
        &appender_key(),
    )
}

fn with(mut log: Vec<Record>, record: Record) -> Vec<Record> {
    log.push(record);
    log
}

fn policy(counter: u64, invariants: Vec<Invariant>) -> Submission {
    Submission {
        stream: StreamId::from(StreamId::POLICY),
        writer: pin(),
        counter,
        body: Body::Policy(Policy {
            evaluator: "0000000".into(),
            invariants,
        }),
    }
}

// ── A valid log ─────────────────────────────────────────────────────

#[test]
fn a_valid_log_replays_to_its_state() {
    let log = records(3);
    let chain = replay(pin(), &log).expect("valid");

    assert_eq!(chain.next_seq(), 5);
    assert_eq!(chain.head().expect("head").hash, log[4].hash);
    assert_eq!(chain.appender(), Some(public_key(&appender_key())));
    assert_eq!(chain.policy_seq(), 0);

    let state = chain.state();
    let authority = state.streams[&StreamId::from(StreamId::AUTHORITY)];
    assert_eq!((authority.entries, authority.last), (2, 1));
    let island = state.streams[&StreamId::from("island")];
    assert_eq!((island.entries, island.last), (3, 4));

    let last = state.writers[&public_key(&writer())];
    assert_eq!(last.counter, 3);
    assert_eq!(last.sub_hash, log[4].entry.submission.hash());
    assert_eq!(
        last.answer,
        Answer::Accepted {
            seq: 4,
            hash: log[4].hash
        }
    );
    assert_eq!(state.writers[&pin()].counter, 2);
    assert_eq!(chain.next_counter(&public_key(&writer())), Some(4));
    assert_eq!(chain.next_counter(&[9; 32]), Some(1));
}

#[test]
fn the_state_hash_is_the_same_for_the_same_records_and_differs_otherwise() {
    let log = records(3);
    let once = replay(pin(), &log).expect("valid").state_hash();
    let twice = replay(pin(), &log).expect("valid").state_hash();
    assert_eq!(once, twice);

    let shorter = replay(pin(), &log[..4]).expect("valid").state_hash();
    assert_ne!(once, shorter);
}

#[test]
fn a_policy_entry_takes_force_from_the_next_entry_on() {
    let log = records(1);
    // The policy entry itself is judged under what was in force before.
    let entry = next(
        &log,
        policy(3, vec![Invariant::VerseNamesEvidence]),
        &root(),
        |_| {},
    );
    assert_eq!(entry.entry.policy, 0);
    let log = with(log, entry);

    let chain = replay(pin(), &log).expect("valid");
    assert_eq!(chain.policy_seq(), 3);
    assert_eq!(
        chain.state().policy.invariants,
        [Invariant::VerseNamesEvidence]
    );

    // The next entry must say it was judged under entry 3.
    let stale = next(&log, verse(&writer(), 2, "after"), &writer(), |position| {
        position.policy = 0
    });
    assert_eq!(
        fault(&with(log.clone(), stale)),
        at(
            4,
            Fault::Policy {
                expected: 3,
                found: 0
            }
        )
    );
    let current = next(&log, verse(&writer(), 2, "after"), &writer(), |_| {});
    assert_eq!(current.entry.policy, 3);
    replay(pin(), &with(log, current)).expect("valid");
}

#[test]
fn a_log_with_only_its_genesis_is_valid_but_names_no_appender_yet() {
    let log = records(0);
    let chain = replay(pin(), &log[..1]).expect("valid");
    assert_eq!(chain.appender(), None);
    assert_eq!(chain.next_seq(), 1);
}

// ── Position ────────────────────────────────────────────────────────

#[test]
fn a_gap_or_a_repeat_in_seq_is_refused() {
    let log = records(2);
    let missing: Vec<Record> = [&log[..2], &log[3..]].concat();
    assert_eq!(
        fault(&missing),
        at(
            2,
            Fault::Seq {
                expected: 2,
                found: 3
            }
        )
    );
    let repeated: Vec<Record> = [&log[..3], &log[2..]].concat();
    assert_eq!(
        fault(&repeated),
        at(
            3,
            Fault::Seq {
                expected: 3,
                found: 2
            }
        )
    );
}

#[test]
fn a_wrong_prev_is_refused() {
    let log = records(1);
    let record = next(&log, verse(&writer(), 2, "x"), &writer(), |position| {
        position.prev[0] ^= 1
    });
    assert_eq!(fault(&with(log, record)), at(3, Fault::Prev));
}

#[test]
fn time_may_stand_still_but_not_go_back() {
    let log = records(1);
    let same = next(&log, verse(&writer(), 2, "x"), &writer(), |_| {});
    replay(pin(), &with(log.clone(), same)).expect("equal times are fine");

    let earlier = next(&log, verse(&writer(), 2, "x"), &writer(), |position| {
        position.at -= 1
    });
    assert_eq!(fault(&with(log, earlier)), at(3, Fault::TimeWentBack));
}

// ── Hash and signatures ─────────────────────────────────────────────

#[test]
fn a_changed_entry_is_refused_by_its_hash() {
    let mut log = records(2);
    log[3].entry.at += 1;
    assert_eq!(fault(&log), at(3, Fault::Hash));
}

#[test]
fn a_submission_its_writer_did_not_sign_is_refused() {
    let log = records(1);
    let mut record = next(&log, verse(&writer(), 2, "x"), &writer(), |_| {});
    // Change the text and re-seal: the appender signs, the writer did not.
    record.entry.submission.body = verse(&writer(), 2, "y").body;
    let forged = Record::seal(record.entry, &appender_key());
    assert_eq!(fault(&with(log, forged)), at(3, Fault::WriterSignature));
}

#[test]
fn a_record_sealed_by_another_key_is_refused() {
    let log = records(1);
    let chain = replay(pin(), &log).expect("valid");
    let head = chain.head().expect("head");
    let stranger = SigningKey::from_bytes(&[0x66; 32]);
    let record = seal(
        3,
        head.at,
        0,
        head.hash,
        verse(&writer(), 2, "x"),
        &writer(),
        &stranger,
    );
    assert_eq!(fault(&with(log, record)), at(3, Fault::AppenderSignature));
}

#[test]
fn the_genesis_seal_is_checked_once_entry_1_names_the_appender() {
    let mut log = records(0);
    let stranger = SigningKey::from_bytes(&[0x66; 32]);
    // Entry 0 sealed by a stranger; entry 1 rebuilt on top of it, honestly.
    log[0] = Record::seal(log[0].entry.clone(), &stranger);
    log[1] = seal(
        1,
        log[1].entry.at,
        0,
        log[0].hash,
        log[1].entry.submission.clone(),
        &root(),
        &appender_key(),
    );
    // Alone, entry 0 cannot be told apart yet; with entry 1 it is caught.
    replay(pin(), &log[..1]).expect("no appender named yet");
    assert_eq!(fault(&log), at(1, Fault::AppenderSignature));
}

// ── Genesis and bootstrap ───────────────────────────────────────────

fn first(submission: Submission, key: &SigningKey) -> Record {
    seal(0, 1_000, 0, [0; 32], submission, key, &appender_key())
}

#[test]
fn entry_0_must_be_a_genesis_for_the_pinned_root() {
    // Not a genesis at all.
    let record = first(verse(&writer(), 1, "x"), &writer());
    assert_eq!(fault(&[record]), at(0, Fault::NotGenesis));

    // A genesis for another root, signed by that root.
    let other = SigningKey::from_bytes(&[0x66; 32]);
    let record = first(genesis::genesis(public_key(&other)), &other);
    assert_eq!(fault(&[record]), at(0, Fault::RootNotPinned));

    // A genesis that declares the pinned root but is signed by someone
    // else.
    let mut submission = genesis::genesis(public_key(&other));
    submission.body = Body::Genesis(Genesis { root: pin() });
    let record = first(submission, &other);
    assert_eq!(fault(&[record]), at(0, Fault::NotGenesis));
}

fn second(log: &[Record], submission: Submission, key: &SigningKey) -> Vec<Record> {
    let record = seal(1, 1_000, 0, log[0].hash, submission, key, &appender_key());
    vec![log[0].clone(), record]
}

fn bootstrap_with(grants: Vec<Grant>) -> Submission {
    let mut submission = genesis::bootstrap(pin(), public_key(&appender_key()));
    submission.body = Body::Bootstrap(Bootstrap { grants });
    submission
}

fn verdict_grant(holder: PublicKey) -> Grant {
    Grant {
        holder,
        scope: BTreeSet::from([StreamId::from(StreamId::GATE)]),
        rights: BTreeSet::from([Kind::Verdict]),
        narrows: None,
    }
}

#[test]
fn entry_1_must_be_a_bootstrap_signed_by_root() {
    let log = records(0);

    let mut not_bootstrap = verse(&root(), 2, "x");
    not_bootstrap.stream = StreamId::from(StreamId::AUTHORITY);
    assert_eq!(
        fault(&second(&log, not_bootstrap, &root())),
        at(1, Fault::NotBootstrap)
    );

    let mut by_writer = genesis::bootstrap(pin(), public_key(&appender_key()));
    by_writer.writer = public_key(&writer());
    by_writer.counter = 1;
    assert_eq!(
        fault(&second(&log, by_writer, &writer())),
        at(1, Fault::NotBootstrap)
    );
}

#[test]
fn the_bootstrap_must_name_exactly_one_appender_that_is_not_root() {
    let log = records(0);
    let appender = public_key(&appender_key());
    for grants in [
        vec![],
        vec![verdict_grant(appender), verdict_grant([7; 32])],
        vec![verdict_grant(pin())],
    ] {
        assert_eq!(
            fault(&second(&log, bootstrap_with(grants), &root())),
            at(1, Fault::BootstrapAppender)
        );
    }
}

#[test]
fn the_bootstrap_must_hold_exactly_one_root_capability() {
    let log = records(0);
    let appender = verdict_grant(public_key(&appender_key()));
    let root_grant = |scope: &[&str], rights: &[Kind]| Grant {
        holder: pin(),
        scope: scope.iter().map(|name| StreamId::from(*name)).collect(),
        rights: rights.iter().copied().collect(),
        narrows: None,
    };
    let capability = root_grant(&["authority", "policy"], &[Kind::Grant, Kind::Revoke]);
    for grants in [
        // None at all, or two.
        vec![appender.clone()],
        vec![capability.clone(), capability.clone(), appender.clone()],
        // Root without the right to revoke, or without the authority stream.
        vec![root_grant(&["authority"], &[Kind::Grant]), appender.clone()],
        vec![
            root_grant(&["policy"], &[Kind::Grant, Kind::Revoke]),
            appender.clone(),
        ],
    ] {
        assert_eq!(
            fault(&second(&log, bootstrap_with(grants), &root())),
            at(1, Fault::BootstrapRoot)
        );
    }
    replay(
        pin(),
        &second(&log, bootstrap_with(vec![capability, appender]), &root()),
    )
    .expect("one root capability and one appender");
}

#[test]
fn a_genesis_or_bootstrap_later_in_the_log_is_refused() {
    let log = records(1);
    let mut again = genesis::genesis(pin());
    again.counter = 3;
    let record = next(&log, again, &root(), |_| {});
    assert_eq!(
        fault(&with(log.clone(), record)),
        at(
            3,
            Fault::Misplaced {
                kind: Kind::Genesis
            }
        )
    );

    let mut again = genesis::bootstrap(pin(), public_key(&appender_key()));
    again.counter = 3;
    let record = next(&log, again, &root(), |_| {});
    assert_eq!(
        fault(&with(log, record)),
        at(
            3,
            Fault::Misplaced {
                kind: Kind::Bootstrap
            }
        )
    );
}

// ── Streams, counters, verdicts ─────────────────────────────────────

#[test]
fn each_kind_belongs_on_its_stream() {
    let log = records(1);
    let on = |stream: &str, body: Body| Submission {
        stream: StreamId::from(stream),
        writer: public_key(&writer()),
        counter: 2,
        body,
    };
    let verse_body = verse(&writer(), 2, "x").body;
    let preference = Body::Preference(Preference {
        key: "k".into(),
        value: "v".into(),
    });
    let policy_body = policy(3, vec![]).body;

    let refused = [
        on("policy", verse_body.clone()),
        on("authority", verse_body.clone()),
        on("gate", verse_body.clone()),
        on("plan", verse_body.clone()),
        on("gate", preference.clone()),
        on("island", policy_body.clone()),
    ];
    for submission in refused {
        let (kind, stream) = (submission.kind(), submission.stream.clone());
        let record = next(&log, submission, &writer(), |_| {});
        assert_eq!(
            fault(&with(log.clone(), record)),
            at(3, Fault::Stream { kind, stream })
        );
    }

    for submission in [
        on("island", verse_body),
        on("island", preference.clone()),
        on("plan", preference),
    ] {
        let record = next(&log, submission, &writer(), |_| {});
        replay(pin(), &with(log.clone(), record)).expect("belongs");
    }
    // A policy entry belongs on the policy stream; root holds that right.
    let record = next(&log, policy(3, vec![]), &root(), |_| {});
    replay(pin(), &with(log, record)).expect("belongs");
}

#[test]
fn a_writer_counts_one_two_three() {
    let log = records(1);
    for (counter, expected) in [(1, 2), (3, 2), (0, 2)] {
        let record = next(&log, verse(&writer(), counter, "x"), &writer(), |_| {});
        assert_eq!(
            fault(&with(log.clone(), record)),
            at(
                3,
                Fault::Counter {
                    expected: Some(expected),
                    found: counter
                }
            )
        );
    }
    // A writer's first submission carries 1, nothing else.
    let newcomer = SigningKey::from_bytes(&[0x4e; 32]);
    let record = next(&log, verse(&newcomer, 2, "x"), &newcomer, |_| {});
    assert_eq!(
        fault(&with(log, record)),
        at(
            3,
            Fault::Counter {
                expected: Some(1),
                found: 2
            }
        )
    );
}

#[test]
fn a_verdict_is_refused_until_the_gate_can_rejudge_it() {
    let log = records(1);
    let rejected = verse(&writer(), 2, "x");
    let submission = Submission {
        stream: StreamId::from(StreamId::GATE),
        writer: public_key(&appender_key()),
        counter: 1,
        body: Body::Verdict(Verdict {
            sig_writer: common::sign(&rejected, &writer()),
            rejected: Box::new(rejected),
            reason: Refusal::Invariant(Invariant::VerseNamesEvidence),
        }),
    };
    let record = next(&log, submission, &appender_key(), |_| {});
    assert_eq!(fault(&with(log, record)), at(3, Fault::VerdictWithoutGate));
}

#[test]
fn check_changes_nothing_until_advance() {
    let log = records(1);
    let mut chain = replay(pin(), &log).expect("valid");
    let before: Chain = chain.clone();
    let record = next(&log, verse(&writer(), 2, "x"), &writer(), |_| {});

    let step = chain.check(&record).expect("valid next record");
    assert_eq!(chain, before);
    chain.advance(&record, step);
    assert_eq!(chain.next_seq(), before.next_seq() + 1);
}
