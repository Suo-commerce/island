//! The gate in the write path and in replay (docs/spec.md, "Write path and
//! gate" and "The ratchet in slice 1").

mod common;

use common::{appender_key, pin, root, seal, sign, started, submit, verse, writer};
use island_auth::Denied;
use island_core::{
    Answer, Body, Evidence, Invariant, Kind, Policy, Record, Refusal, SigningKey, StreamId,
    Submission, Verdict, Verse, public_key,
};
use island_log::{Fault, Rejected, ReplayError, VerdictFault, read, replay};

const NAMES: Invariant = Invariant::VerseNamesEvidence;
const HAS_REF: Invariant = Invariant::ArtefactHasRef;

const fn max(bytes: u32) -> Invariant {
    Invariant::VerseTextMax { bytes }
}

fn policy(counter: u64, invariants: &[Invariant]) -> Submission {
    Submission {
        stream: StreamId::from(StreamId::POLICY),
        writer: pin(),
        counter,
        body: Body::Policy(Policy {
            evaluator: "test".to_owned(),
            invariants: invariants.to_vec(),
        }),
    }
}

/// A verse with any evidence byte and any number of refs.
fn raw(counter: u64, evidence: u8, text: &str, refs: usize) -> Submission {
    let mut submission = verse(&writer(), counter, text);
    submission.body = Body::Verse(Verse {
        evidence,
        text: text.to_owned(),
        refs: vec![[9; 32]; refs],
    });
    submission
}

fn refused(
    appender: &mut common::TestAppender,
    submission: Submission,
    key: &SigningKey,
) -> Rejected {
    let signature = sign(&submission, key);
    appender
        .submit(submission, signature)
        .expect_err("the submission is refused")
}

/// A started log with the first policy in force at entry 2.
fn with_policy(invariants: &[Invariant]) -> (common::TestAppender, common::MemStorage) {
    let (mut appender, storage, _) = started();
    let ack = submit(&mut appender, policy(3, invariants), &root());
    assert_eq!(ack.seq, 2);
    (appender, storage)
}

// ── The write path ──────────────────────────────────────────────────

#[test]
fn a_refused_verse_leaves_a_verdict_and_uses_its_counter() {
    let (mut appender, storage) = with_policy(&[NAMES, max(10)]);
    let no_evidence = raw(1, 200, "short", 0);

    let Rejected::Refused { seq, hash, reason } =
        refused(&mut appender, no_evidence.clone(), &writer())
    else {
        panic!("expected a gate refusal");
    };
    assert_eq!((seq, reason), (3, Refusal::Invariant(NAMES)));

    // The verdict is entry 3: on the gate stream, written and sealed by the
    // appender, judged under policy 2, carrying the whole submission.
    let log = read(&storage.bytes(), pin()).expect("the log verifies");
    let entry = &log.records[3].entry;
    assert_eq!(log.records[3].hash, hash);
    assert_eq!(entry.policy, 2);
    assert_eq!(entry.submission.stream, StreamId::from(StreamId::GATE));
    assert_eq!(entry.submission.writer, public_key(&appender_key()));
    let Body::Verdict(verdict) = &entry.submission.body else {
        panic!("entry 3 is not a verdict");
    };
    assert_eq!(*verdict.rejected, no_evidence);
    assert_eq!(verdict.reason, Refusal::Invariant(NAMES));
    assert_eq!(verdict.rejected.verify(&verdict.sig_writer), Ok(()));

    // The writer's counter advanced: its answer is the refusal.
    let last = log.chain.state().writers[&public_key(&writer())];
    assert_eq!(last.counter, 1);
    assert_eq!(last.answer, Answer::Refused { seq, hash, reason });
    assert_eq!(&log.chain, appender.chain());

    // An exact resend gets the same refusal and adds nothing.
    let len = storage.len();
    assert_eq!(
        refused(&mut appender, no_evidence, &writer()),
        Rejected::Refused { seq, hash, reason }
    );
    assert_eq!(storage.len(), len);

    // The next verse carries counter 2, and a good one is accepted.
    assert_eq!(
        refused(&mut appender, raw(1, 0, "again", 0), &writer()),
        Rejected::CounterConflict { counter: 1 }
    );
    assert_eq!(
        submit(&mut appender, raw(2, 0, "fine", 0), &writer()).seq,
        4
    );
}

#[test]
fn each_invariant_refuses_what_it_names() {
    let (mut appender, _) = with_policy(&[NAMES, max(10), HAS_REF]);
    let artefact = Evidence::Artefact.as_u8();
    let cases = [
        (raw(1, 9, "x", 0), Refusal::Invariant(NAMES)),
        (raw(2, 0, "eleven char", 0), Refusal::Invariant(max(10))),
        (raw(3, artefact, "x", 0), Refusal::Invariant(HAS_REF)),
    ];
    for (submission, expected) in cases {
        let Rejected::Refused { reason, .. } = refused(&mut appender, submission, &writer()) else {
            panic!("expected a gate refusal");
        };
        assert_eq!(reason, expected);
    }
    submit(&mut appender, raw(4, artefact, "ten chars!", 1), &writer());
}

#[test]
fn the_policy_ratchets_and_tightening_leaves_history_alone() {
    let (mut appender, storage) = with_policy(&[NAMES, max(4096)]);
    // Accepted under the first policy: a long verse.
    let long = "a verse of more than twenty bytes";
    submit(&mut appender, raw(1, 0, long, 0), &writer());

    // Loosening is refused, with a verdict: raising the bound, dropping an
    // invariant.
    for (counter, invariants) in [(4, &[NAMES, max(5000)][..]), (5, &[max(4096)][..])] {
        let Rejected::Refused { reason, .. } =
            refused(&mut appender, policy(counter, invariants), &root())
        else {
            panic!("expected a gate refusal");
        };
        assert_eq!(reason, Refusal::WeakerPolicy);
    }
    assert_eq!(appender.chain().policy_seq(), 2);

    // The slice's real tightening: a lower bound and ArtefactHasRef.
    let ack = submit(
        &mut appender,
        policy(6, &[NAMES, max(20), HAS_REF]),
        &root(),
    );
    assert_eq!(appender.chain().policy_seq(), ack.seq);

    // New writes are judged by it ...
    let Rejected::Refused { reason, .. } = refused(&mut appender, raw(2, 0, long, 0), &writer())
    else {
        panic!("expected a gate refusal");
    };
    assert_eq!(reason, Refusal::Invariant(max(20)));
    // ... and the long verse already in the log still verifies.
    let log = read(&storage.bytes(), pin()).expect("history is still valid");
    assert_eq!(&log.chain, appender.chain());
    assert_eq!(
        log.chain.state().policy.invariants,
        [NAMES, max(20), HAS_REF]
    );
}

#[test]
fn nobody_submits_a_verdict() {
    let (mut appender, storage) = with_policy(&[NAMES]);
    let rejected = raw(1, 9, "x", 0);
    let forged = |writer: &SigningKey, counter| Submission {
        stream: StreamId::from(StreamId::GATE),
        writer: public_key(writer),
        counter,
        body: Body::Verdict(Verdict {
            sig_writer: sign(&rejected, &common::writer()),
            rejected: Box::new(rejected.clone()),
            reason: Refusal::Invariant(NAMES),
        }),
    };
    let len = storage.len();
    // Not a writer, and not even the appender's own key from outside.
    assert_eq!(
        refused(&mut appender, forged(&writer(), 1), &writer()),
        Rejected::VerdictFromOutside
    );
    assert_eq!(
        refused(&mut appender, forged(&appender_key(), 1), &appender_key()),
        Rejected::VerdictFromOutside
    );
    assert_eq!(storage.len(), len);
}

// ── Replay ──────────────────────────────────────────────────────────

/// The records of a log with policy `[NAMES, max(10)]` at entry 2 and one
/// refused verse (a verdict) at entry 3.
fn log_with_verdict() -> Vec<Record> {
    let (mut appender, storage) = with_policy(&[NAMES, max(10)]);
    refused(&mut appender, raw(1, 9, "x", 0), &writer());
    read(&storage.bytes(), pin()).expect("valid").records
}

/// Entry 3 rebuilt with another body, sealed honestly by the appender.
fn reseal(log: &[Record], submission: Submission, key: &SigningKey) -> Vec<Record> {
    let chain = replay(pin(), &log[..3]).expect("valid");
    let head = chain.head().expect("head");
    let record = seal(
        3,
        head.at,
        chain.policy_seq(),
        head.hash,
        submission,
        key,
        &appender_key(),
    );
    [&log[..3], &[record]].concat()
}

fn verdict_for(rejected: Submission, signer: &SigningKey, reason: Refusal) -> Submission {
    Submission {
        stream: StreamId::from(StreamId::GATE),
        writer: public_key(&appender_key()),
        counter: 1,
        body: Body::Verdict(Verdict {
            sig_writer: island_core::sign_hash(signer, &rejected.hash()),
            rejected: Box::new(rejected),
            reason,
        }),
    }
}

fn fault_at_3(log: &[Record]) -> Fault {
    match replay(pin(), log) {
        Err(ReplayError { seq: 3, fault }) => fault,
        other => panic!("expected a fault at entry 3, got {other:?}"),
    }
}

#[test]
fn replay_rejudges_every_verdict() {
    let log = log_with_verdict();
    replay(pin(), &log).expect("an honest verdict replays");
    let appender = appender_key();

    // The gate would refuse it, but for another reason than recorded.
    let other_reason = verdict_for(raw(1, 9, "x", 0), &writer(), Refusal::Invariant(max(10)));
    assert_eq!(
        fault_at_3(&reseal(&log, other_reason, &appender)),
        Fault::Verdict(VerdictFault::Rejudged {
            recorded: Refusal::Invariant(max(10)),
            now: Some(Refusal::Invariant(NAMES)),
        })
    );

    // The gate would accept it: a refusal that was never due.
    let acceptable = verdict_for(raw(1, 0, "x", 0), &writer(), Refusal::Invariant(NAMES));
    assert_eq!(
        fault_at_3(&reseal(&log, acceptable, &appender)),
        Fault::Verdict(VerdictFault::Rejudged {
            recorded: Refusal::Invariant(NAMES),
            now: None,
        })
    );

    // The refused submission was never signed by its writer.
    let unsigned = verdict_for(raw(1, 9, "x", 0), &root(), Refusal::Invariant(NAMES));
    assert_eq!(
        fault_at_3(&reseal(&log, unsigned, &appender)),
        Fault::Verdict(VerdictFault::Signature)
    );

    // It would have been refused with no entry: no grant, or a bad counter.
    let stranger = SigningKey::from_bytes(&[0x66; 32]);
    let mut by_stranger = raw(1, 9, "x", 0);
    by_stranger.writer = public_key(&stranger);
    let no_grant = verdict_for(by_stranger, &stranger, Refusal::Invariant(NAMES));
    assert_eq!(
        fault_at_3(&reseal(&log, no_grant, &appender)),
        Fault::Verdict(VerdictFault::NoEntry(Box::new(Fault::Denied(
            Denied::NoGrant
        ))))
    );
    let skipped = verdict_for(raw(5, 9, "x", 0), &writer(), Refusal::Invariant(NAMES));
    assert!(matches!(
        fault_at_3(&reseal(&log, skipped, &appender)),
        Fault::Verdict(VerdictFault::NoEntry(fault)) if matches!(*fault, Fault::Counter { .. })
    ));

    // A verdict written by anyone but the appender: no grant for that.
    let mut by_writer = verdict_for(raw(1, 9, "x", 0), &writer(), Refusal::Invariant(NAMES));
    by_writer.writer = public_key(&writer());
    assert_eq!(
        fault_at_3(&reseal(&log, by_writer, &writer())),
        Fault::Denied(Denied::NoGrant)
    );
}

#[test]
fn replay_refuses_an_entry_the_gate_refuses() {
    // An appender that puts a refused verse straight into the log.
    let log = log_with_verdict();
    let smuggled = reseal(&log, raw(1, 9, "x", 0), &writer());
    assert_eq!(
        fault_at_3(&smuggled),
        Fault::Gate(Refusal::Invariant(NAMES))
    );

    // The same for a policy that loosens.
    let loosened = reseal(&log, policy(4, &[NAMES]), &root());
    assert_eq!(fault_at_3(&loosened), Fault::Gate(Refusal::WeakerPolicy));
}

#[test]
fn the_appender_key_writes_only_verdicts() {
    // Even with a grant in the bootstrap, a verse by the appender's key is
    // not a valid entry.
    let log = common::records(0);
    let chain = replay(pin(), &log).expect("valid");
    let head = chain.head().expect("head");
    let by_appender = verse(&appender_key(), 1, "I only keep the log");
    let record = seal(
        2,
        head.at,
        0,
        head.hash,
        by_appender,
        &appender_key(),
        &appender_key(),
    );
    assert_eq!(
        replay(pin(), &[&log[..], &[record]].concat()),
        Err(ReplayError {
            seq: 2,
            fault: Fault::AppenderWrites
        })
    );
    // Kind::Verdict is the one exception, and it is checked on its own.
    assert_eq!(Kind::Verdict.tag(), 5);
}
