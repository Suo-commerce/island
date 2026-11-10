//! The write path: what is acknowledged, what is refused without an entry,
//! and what a storage failure or a crash leaves behind.

mod common;

use common::{
    ManualClock, MemStorage, appender_key, open, pin, root, sign, started, submit, verse, writer,
};
use island_core::{SigningKey, StreamId, Submission, public_key};
use island_log::frame::{self, HEADER_LEN};
use island_log::{
    Ack, Appender, Fault, FrameError, LogError, OpenError, Recovery, Rejected, genesis, read,
};

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

// ── Accepting ───────────────────────────────────────────────────────

#[test]
fn a_new_log_takes_genesis_bootstrap_and_then_verses() {
    let (mut appender, storage, _) = started();
    let ack = submit(&mut appender, verse(&writer(), 1, "first"), &writer());
    assert_eq!(ack.seq, 2);

    let log = read(&storage.bytes(), pin()).expect("the log reads");
    assert_eq!(log.records.len(), 3);
    assert_eq!(log.torn, 0);
    assert_eq!(log.records[2].hash, ack.hash);
    // What the appender judges from is what replay arrives at.
    assert_eq!(&log.chain, appender.chain());
    assert_eq!(log.chain.state_hash(), appender.chain().state_hash());
}

#[test]
fn nothing_is_acknowledged_before_it_is_synced() {
    let (mut appender, storage, _) = started();
    submit(&mut appender, verse(&writer(), 1, "first"), &writer());
    let mem = storage.0.borrow();
    assert_eq!(mem.synced, mem.bytes.len());
}

#[test]
fn at_never_goes_back_when_the_clock_does() {
    let (mut appender, storage, clock) = started();
    clock.set(5_000);
    submit(&mut appender, verse(&writer(), 1, "a"), &writer());
    clock.set(10);
    submit(&mut appender, verse(&writer(), 2, "b"), &writer());
    clock.set(6_000);
    submit(&mut appender, verse(&writer(), 3, "c"), &writer());

    let log = read(&storage.bytes(), pin()).expect("the log reads");
    let times: Vec<u64> = log.records.iter().map(|record| record.entry.at).collect();
    assert_eq!(times, [1_000, 1_000, 5_000, 5_000, 6_000]);
}

// ── The counter ─────────────────────────────────────────────────────

#[test]
fn an_exact_resend_gets_the_first_answer_and_adds_nothing() {
    let (mut appender, storage, clock) = started();
    let first = submit(&mut appender, verse(&writer(), 1, "once"), &writer());
    let len = storage.len();

    clock.set(9_999);
    let again = submit(&mut appender, verse(&writer(), 1, "once"), &writer());
    assert_eq!(again, first);
    assert_eq!(storage.len(), len);
    assert_eq!(appender.chain().next_seq(), 3);
}

#[test]
fn a_replayed_old_submission_is_refused() {
    let (mut appender, storage, _) = started();
    submit(&mut appender, verse(&writer(), 1, "one"), &writer());
    submit(&mut appender, verse(&writer(), 2, "two"), &writer());
    let len = storage.len();

    assert_eq!(
        refused(&mut appender, verse(&writer(), 1, "one"), &writer()),
        Rejected::CounterOld {
            expected: Some(3),
            found: 1
        }
    );
    assert_eq!(storage.len(), len);
}

#[test]
fn a_skipped_counter_is_refused() {
    let (mut appender, storage, _) = started();
    let len = storage.len();
    assert_eq!(
        refused(&mut appender, verse(&writer(), 2, "early"), &writer()),
        Rejected::CounterGap {
            expected: Some(1),
            found: 2
        }
    );
    assert_eq!(
        refused(&mut appender, verse(&writer(), 0, "zero"), &writer()),
        Rejected::CounterOld {
            expected: Some(1),
            found: 0
        }
    );
    assert_eq!(storage.len(), len);
}

#[test]
fn the_same_counter_with_other_content_is_refused() {
    let (mut appender, storage, _) = started();
    submit(&mut appender, verse(&writer(), 1, "one"), &writer());
    let len = storage.len();
    assert_eq!(
        refused(&mut appender, verse(&writer(), 1, "another"), &writer()),
        Rejected::CounterConflict { counter: 1 }
    );
    assert_eq!(storage.len(), len);
}

// ── Refused without an entry ────────────────────────────────────────

#[test]
fn a_bad_signature_is_refused_before_anything_else() {
    let (mut appender, storage, _) = started();
    let len = storage.len();
    // Signed by the writer, then changed: the counter is even wrong, but
    // the signature is what is reported.
    let signature = sign(&verse(&writer(), 1, "signed"), &writer());
    assert_eq!(
        appender.submit(verse(&writer(), 7, "changed"), signature),
        Err(Rejected::BadSignature)
    );
    assert_eq!(storage.len(), len);
}

#[test]
fn the_structure_rules_refuse_without_an_entry() {
    // A verse before any genesis.
    let storage = MemStorage::default();
    let clock = ManualClock::at(1_000);
    let mut appender = open(&storage, &clock);
    assert_eq!(
        refused(&mut appender, verse(&writer(), 1, "x"), &writer()),
        Rejected::Invalid(Fault::NotGenesis)
    );

    // A genesis for a root that is not the pinned one.
    let other = SigningKey::from_bytes(&[0x66; 32]);
    assert_eq!(
        refused(&mut appender, genesis::genesis(public_key(&other)), &other),
        Rejected::Invalid(Fault::RootNotPinned)
    );
    assert_eq!(storage.bytes(), frame::header());

    // A bootstrap that names another appender key: this appender's seals
    // would not verify against it.
    submit(&mut appender, genesis::genesis(pin()), &root());
    let other_appender = public_key(&SigningKey::from_bytes(&[0x67; 32]));
    assert_eq!(
        refused(
            &mut appender,
            genesis::bootstrap(pin(), other_appender),
            &root()
        ),
        Rejected::Invalid(Fault::AppenderSignature)
    );

    // A verse on a system stream.
    let (mut appender, storage, _) = started();
    let len = storage.len();
    let mut misplaced = verse(&writer(), 1, "x");
    misplaced.stream = StreamId::from(StreamId::POLICY);
    assert!(matches!(
        refused(&mut appender, misplaced, &writer()),
        Rejected::Invalid(Fault::Stream { .. })
    ));
    assert_eq!(storage.len(), len);
    // None of it moved the writer's counter.
    submit(&mut appender, verse(&writer(), 1, "x"), &writer());
}

// ── Storage failures ────────────────────────────────────────────────

#[test]
fn a_failed_sync_stops_the_appender_and_acknowledges_nothing() {
    let (mut appender, storage, clock) = started();
    submit(&mut appender, verse(&writer(), 1, "durable"), &writer());
    let before = appender.chain().clone();

    storage.0.borrow_mut().fail_next_sync = true;
    assert!(matches!(
        refused(&mut appender, verse(&writer(), 2, "lost?"), &writer()),
        Rejected::Storage(_)
    ));
    assert!(appender.is_stopped());
    // The state did not run ahead of what is durable.
    assert_eq!(appender.chain(), &before);
    // And nothing more is taken, not even a retry.
    assert_eq!(
        refused(&mut appender, verse(&writer(), 2, "lost?"), &writer()),
        Rejected::Stopped
    );

    // Case 1: the crash loses the unsynced record. The writer resends and
    // the record lands at the same position.
    let lost = MemStorage::with_bytes(storage.bytes());
    lost.0.borrow_mut().synced = storage.0.borrow().synced;
    lost.crash(0);
    let mut reopened = open(&lost, &clock);
    assert_eq!(reopened.chain(), &before);
    let ack = submit(&mut reopened, verse(&writer(), 2, "lost?"), &writer());
    assert_eq!(ack.seq, 3);

    // Case 2: the record reached the disk after all. The resend is then a
    // retry and gets that record's position; nothing is written twice.
    let kept = MemStorage::with_bytes(storage.bytes());
    let mut reopened = open(&kept, &clock);
    let len = kept.len();
    let ack = submit(&mut reopened, verse(&writer(), 2, "lost?"), &writer());
    assert_eq!(ack.seq, 3);
    assert_eq!(kept.len(), len);
}

#[test]
fn a_torn_append_is_dropped_on_reopen() {
    let (mut appender, storage, clock) = started();
    submit(&mut appender, verse(&writer(), 1, "durable"), &writer());
    let good = storage.len();

    storage.0.borrow_mut().tear_next_append = Some(HEADER_LEN + 5);
    assert!(matches!(
        refused(&mut appender, verse(&writer(), 2, "torn"), &writer()),
        Rejected::Storage(_)
    ));
    assert_eq!(storage.len(), good + HEADER_LEN + 5);

    // A reader reports the tail and leaves it alone.
    let log = read(&storage.bytes(), pin()).expect("still a log");
    assert_eq!((log.records.len(), log.torn), (3, HEADER_LEN + 5));

    let (mut reopened, recovery) =
        Appender::open(storage.clone(), clock, appender_key(), pin()).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            records: 3,
            dropped: (HEADER_LEN + 5) as u64
        }
    );
    assert_eq!(storage.len(), good);
    let ack = submit(&mut reopened, verse(&writer(), 2, "torn"), &writer());
    assert_eq!(ack.seq, 3);
    read(&storage.bytes(), pin()).expect("a clean log again");
}

#[test]
fn a_crash_at_any_byte_recovers_to_the_complete_records() {
    let (mut appender, storage, clock) = started();
    let mut acks: Vec<(usize, Ack)> = Vec::new();
    for counter in 1..=3 {
        let ack = submit(
            &mut appender,
            verse(&writer(), counter, "a verse of some length"),
            &writer(),
        );
        acks.push((storage.len(), ack));
    }
    let bytes = storage.bytes();

    for cut in 0..=bytes.len() {
        let crashed = MemStorage::with_bytes(bytes[..cut].to_vec());
        let (mut reopened, recovery) =
            Appender::open(crashed.clone(), clock.clone(), appender_key(), pin())
                .unwrap_or_else(|error| panic!("cut at {cut}: {error}"));

        // Every acknowledged record that was whole on disk is still there.
        let log = read(&crashed.bytes(), pin()).expect("a clean log");
        assert_eq!(log.torn, 0, "cut at {cut}");
        assert_eq!(log.records.len() as u64, recovery.records, "cut at {cut}");
        for (end, ack) in &acks {
            if *end <= cut {
                assert_eq!(log.records[ack.seq as usize].hash, ack.hash, "cut at {cut}");
            }
        }

        // And the log goes on from there. A writer that lost its replies
        // resends its latest submission (a retry, if it had landed) and
        // carries on.
        if reopened.chain().next_seq() == 0 {
            submit(&mut reopened, genesis::genesis(pin()), &root());
        }
        if reopened.chain().next_seq() == 1 {
            submit(&mut reopened, common::bootstrap(), &root());
        }
        let landed = reopened
            .chain()
            .next_counter(&public_key(&writer()))
            .expect("a next counter")
            - 1;
        for counter in landed.max(1)..=3 {
            let ack = submit(
                &mut reopened,
                verse(&writer(), counter, "a verse of some length"),
                &writer(),
            );
            assert_eq!(ack.seq, counter + 1, "cut at {cut}");
        }
        let log = read(&crashed.bytes(), pin()).expect("a clean log");
        assert_eq!(log.records.len(), 5, "cut at {cut}");
    }
}

// ── Opening ─────────────────────────────────────────────────────────

#[test]
fn damage_is_an_error_and_nothing_is_dropped_to_hide_it() {
    let (mut appender, storage, clock) = started();
    for counter in 1..=3 {
        submit(&mut appender, verse(&writer(), counter, "x"), &writer());
    }
    let bytes = storage.bytes();

    // One flipped bit anywhere after the magic: the log does not open,
    // and the stored bytes are left exactly as they were.
    for at in (frame::MAGIC.len()..bytes.len()).step_by(7) {
        let mut damaged = bytes.clone();
        damaged[at] ^= 0x10;
        let store = MemStorage::with_bytes(damaged.clone());
        let result = Appender::open(store.clone(), clock.clone(), appender_key(), pin());
        assert!(
            matches!(result, Err(OpenError::Log(_))),
            "a flipped bit at {at} went unnoticed"
        );
        assert_eq!(store.bytes(), damaged, "bytes changed after damage at {at}");
        assert!(read(&damaged, pin()).is_err());
    }
}

#[test]
fn a_file_that_is_not_a_log_does_not_open() {
    let store = MemStorage::with_bytes(b"something else entirely".to_vec());
    let result = Appender::open(store.clone(), ManualClock::at(0), appender_key(), pin());
    assert!(matches!(
        result,
        Err(OpenError::Log(LogError::Frame(FrameError::NotALog)))
    ));
    assert_eq!(store.bytes(), b"something else entirely");
}

#[test]
fn another_key_cannot_append_to_the_log() {
    let (_, storage, clock) = started();
    let stranger = SigningKey::from_bytes(&[0x66; 32]);
    let result = Appender::open(storage, clock, stranger, pin());
    assert!(matches!(result, Err(OpenError::WrongAppenderKey)));
}

#[test]
fn a_log_does_not_open_under_another_root() {
    let (_, storage, clock) = started();
    let result = Appender::open(storage, clock, appender_key(), [9; 32]);
    assert!(matches!(
        result,
        Err(OpenError::Log(LogError::Replay(error))) if error.seq == 0 && error.fault == Fault::RootNotPinned
    ));
}
