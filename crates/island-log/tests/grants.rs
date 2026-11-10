//! Grants in the write path (docs/spec.md, definition of done): a grant
//! for a key, that grant narrowed once, a widening refused, and a revoke
//! that takes effect on the next submission. And replay refusing a log in
//! which something was written without a grant.

mod common;

use common::{appender_key, pin, root, seal, sign, started, submit, verse, writer};
use island_auth::Denied;
use island_core::{
    Body, Ended, Grant, GrantId, Kind, Revoke, Role, SigningKey, StreamId, Submission, public_key,
};
use island_log::{Fault, Rejected, ReplayError, read, replay};

fn daily() -> SigningKey {
    SigningKey::from_bytes(&[0x44; 32])
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

fn on(stream: &str, key: &SigningKey, counter: u64, text: &str) -> Submission {
    let mut submission = verse(key, counter, text);
    submission.stream = StreamId::from(stream);
    submission
}

/// A grant or revoke entry, written by root with the given counter.
fn by_root(counter: u64, body: Body) -> Submission {
    Submission {
        stream: StreamId::from(StreamId::AUTHORITY),
        writer: pin(),
        counter,
        body,
    }
}

fn grant(scope: &[&str], rights: &[Kind], narrows: Option<GrantId>) -> Body {
    Body::Grant(Grant {
        holder: public_key(&daily()),
        scope: scope.iter().map(|name| StreamId::from(*name)).collect(),
        rights: rights.iter().copied().collect(),
        narrows,
    })
}

#[test]
fn grant_narrow_refuse_a_widening_and_revoke() {
    let (mut appender, storage, _) = started();
    let daily = daily();

    // No grant yet: refused, nothing written, the counter does not move.
    let len = storage.bytes().len();
    assert_eq!(
        refused(&mut appender, on("island", &daily, 1, "too early"), &daily),
        Rejected::Denied(Denied::NoGrant)
    );
    assert_eq!(storage.bytes().len(), len);

    // Root grants the daily key verses on two streams. The grant's id is
    // the entry that holds it.
    let ack = submit(
        &mut appender,
        by_root(3, grant(&["island", "flare"], &[Kind::Verse], None)),
        &root(),
    );
    let wide = GrantId::new(ack.seq, 0);
    submit(&mut appender, on("island", &daily, 1, "first"), &daily);
    submit(&mut appender, on("flare", &daily, 2, "second"), &daily);

    // The grant is narrowed in place to one stream.
    let ack = submit(
        &mut appender,
        by_root(4, grant(&["island"], &[Kind::Verse], Some(wide))),
        &root(),
    );
    let narrow = GrantId::new(ack.seq, 0);
    assert_eq!(
        refused(&mut appender, on("flare", &daily, 3, "no longer"), &daily),
        Rejected::Denied(Denied::NoGrant)
    );
    submit(&mut appender, on("island", &daily, 3, "still"), &daily);

    // A widening by way of `narrows` is refused, and leaves no entry.
    let len = storage.bytes().len();
    assert_eq!(
        refused(
            &mut appender,
            by_root(5, grant(&["island", "flare"], &[Kind::Verse], Some(narrow))),
            &root()
        ),
        Rejected::Denied(Denied::Wider)
    );
    assert_eq!(storage.bytes().len(), len);

    // A revoke takes effect on the very next submission.
    submit(
        &mut appender,
        on("island", &daily, 4, "last one in"),
        &daily,
    );
    submit(
        &mut appender,
        by_root(5, Body::Revoke(Revoke { grant: narrow })),
        &root(),
    );
    assert_eq!(
        refused(&mut appender, on("island", &daily, 5, "revoked"), &daily),
        Rejected::Denied(Denied::NoGrant)
    );

    // The log replays to the same grant state, with the history in it.
    let log = read(&storage.bytes(), pin()).expect("the log verifies");
    assert_eq!(&log.chain, appender.chain());
    let grants = &log.chain.state().grants;
    assert_eq!(grants[&GrantId::new(1, 0)].role, Role::Root);
    assert_eq!(grants[&GrantId::new(1, 1)].role, Role::Appender);
    assert_eq!(
        grants[&wide].ended,
        Some(Ended {
            at: narrow.entry,
            by: Some(narrow)
        })
    );
    assert_eq!(
        grants[&narrow].ended.map(|ended| ended.by),
        Some(None),
        "the narrowed grant ended by revoke"
    );
    // Four verses by the daily key are in the log, none after the revoke.
    assert_eq!(log.chain.next_counter(&public_key(&daily)), Some(5));
}

#[test]
fn the_protected_grants_are_protected_in_the_write_path() {
    let (mut appender, _, _) = started();
    let (root_id, appender_id) = (GrantId::new(1, 0), GrantId::new(1, 1));
    for (id, body) in [
        (root_id, Body::Revoke(Revoke { grant: root_id })),
        (appender_id, Body::Revoke(Revoke { grant: appender_id })),
    ] {
        assert_eq!(
            refused(&mut appender, by_root(3, body), &root()),
            Rejected::Denied(Denied::Guarded(id))
        );
    }
    // Only root writes grants: the test writer holds a verse grant, and
    // that is not the right to grant.
    let mut by_writer = by_root(1, grant(&["island"], &[Kind::Verse], None));
    by_writer.writer = public_key(&writer());
    assert_eq!(
        refused(&mut appender, by_writer, &writer()),
        Rejected::Denied(Denied::NoGrant)
    );
}

#[test]
fn replay_refuses_an_entry_no_grant_covers() {
    // An appender that seals a verse for a key nobody granted anything:
    // every hash and signature is right, and replay still refuses it.
    let log = common::records(1);
    let chain = replay(pin(), &log).expect("valid");
    let head = chain.head().expect("head");
    let stranger = SigningKey::from_bytes(&[0x66; 32]);

    let forged = seal(
        head.seq + 1,
        head.at,
        chain.policy_seq(),
        head.hash,
        verse(&stranger, 1, "nobody let me in"),
        &stranger,
        &appender_key(),
    );
    let mut with_forged = log.clone();
    with_forged.push(forged);
    assert_eq!(
        replay(pin(), &with_forged),
        Err(ReplayError {
            seq: head.seq + 1,
            fault: Fault::Denied(Denied::NoGrant)
        })
    );

    // The same for a write outside its writer's scope.
    let outside = seal(
        head.seq + 1,
        head.at,
        chain.policy_seq(),
        head.hash,
        on("flare", &writer(), 2, "not my stream"),
        &writer(),
        &appender_key(),
    );
    let mut with_outside = log;
    with_outside.push(outside);
    assert!(matches!(
        replay(pin(), &with_outside),
        Err(ReplayError {
            fault: Fault::Denied(Denied::NoGrant),
            ..
        })
    ));
}
