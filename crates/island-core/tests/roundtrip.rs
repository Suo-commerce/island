//! The encoding is one-to-one: every value decodes back to itself, and
//! bytes that decode at all re-encode to exactly themselves.

use std::collections::BTreeSet;

use island_core::{
    Body, Bootstrap, DecodeError, Entry, Genesis, Grant, Invariant, Kind, Policy, Preference,
    Record, Refusal, Revoke, StreamId, Submission, Verdict, Verse,
};
use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use taso_core::canon::Canonical;

fn stream_id() -> impl Strategy<Value = StreamId> {
    vec(any::<u8>(), 0..12).prop_map(StreamId::new)
}

fn kind() -> impl Strategy<Value = Kind> {
    prop::sample::select(vec![
        Kind::Policy,
        Kind::Grant,
        Kind::Revoke,
        Kind::Verse,
        Kind::Preference,
        Kind::Verdict,
        Kind::Genesis,
        Kind::Bootstrap,
    ])
}

fn invariant() -> impl Strategy<Value = Invariant> {
    prop_oneof![
        Just(Invariant::VerseNamesEvidence),
        any::<u32>().prop_map(|bytes| Invariant::VerseTextMax { bytes }),
        Just(Invariant::ArtefactHasRef),
    ]
}

fn grant() -> impl Strategy<Value = Grant> {
    (
        any::<[u8; 32]>(),
        btree_set(stream_id(), 0..5),
        btree_set(kind(), 0..5),
        any::<Option<u64>>(),
    )
        .prop_map(|(holder, scope, rights, narrows)| Grant {
            holder,
            scope,
            rights,
            narrows,
        })
}

/// Every body except a verdict.
fn plain_body() -> impl Strategy<Value = Body> {
    prop_oneof![
        (".{0,16}", vec(invariant(), 0..5)).prop_map(|(evaluator, invariants)| {
            Body::Policy(Policy {
                evaluator,
                invariants,
            })
        }),
        grant().prop_map(Body::Grant),
        any::<u64>().prop_map(|grant| Body::Revoke(Revoke { grant })),
        // Any evidence byte, named or not: an unnamed one must still decode.
        (any::<u8>(), ".{0,40}", vec(any::<[u8; 32]>(), 0..4)).prop_map(
            |(evidence, text, refs)| Body::Verse(Verse {
                evidence,
                text,
                refs,
            })
        ),
        (".{0,16}", ".{0,16}").prop_map(|(key, value)| Body::Preference(Preference { key, value })),
        any::<[u8; 32]>().prop_map(|root| Body::Genesis(Genesis { root })),
        vec(grant(), 0..4).prop_map(|grants| Body::Bootstrap(Bootstrap { grants })),
    ]
}

fn submission_with(body: impl Strategy<Value = Body>) -> impl Strategy<Value = Submission> {
    (stream_id(), any::<[u8; 32]>(), any::<u64>(), body).prop_map(
        |(stream, writer, counter, body)| Submission {
            stream,
            writer,
            counter,
            body,
        },
    )
}

fn signature() -> impl Strategy<Value = [u8; 64]> {
    vec(any::<u8>(), 64).prop_map(|bytes| bytes.try_into().expect("64 bytes"))
}

fn verdict() -> impl Strategy<Value = Body> {
    (submission_with(plain_body()), signature(), invariant()).prop_map(
        |(rejected, sig_writer, invariant)| {
            Body::Verdict(Verdict {
                rejected: Box::new(rejected),
                sig_writer,
                reason: Refusal::Invariant(invariant),
            })
        },
    )
}

fn submission() -> impl Strategy<Value = Submission> {
    submission_with(prop_oneof![4 => plain_body(), 1 => verdict()])
}

fn record() -> impl Strategy<Value = Record> {
    (
        (any::<u64>(), any::<u64>(), any::<u64>(), any::<[u8; 32]>()),
        submission(),
        signature(),
        any::<[u8; 32]>(),
        signature(),
    )
        .prop_map(
            |((seq, at, policy, prev), submission, sig_writer, hash, sig_appender)| Record {
                entry: Entry {
                    seq,
                    at,
                    policy,
                    prev,
                    submission,
                    sig_writer,
                },
                hash,
                sig_appender,
            },
        )
}

proptest! {
    #[test]
    fn a_submission_decodes_back_to_itself(submission in submission()) {
        let bytes = submission.canon_bytes();
        prop_assert_eq!(Submission::decode(&bytes), Ok(submission));
    }

    #[test]
    fn a_record_decodes_back_to_itself(record in record()) {
        let bytes = record.encode();
        prop_assert_eq!(Record::decode(&bytes), Ok(record));
    }

    /// Change one byte of a valid record: it is refused, or it is another
    /// record whose encoding is exactly the changed bytes. Never a second
    /// encoding of anything.
    #[test]
    fn a_changed_record_is_refused_or_still_canonical(
        record in record(),
        index in any::<prop::sample::Index>(),
        byte in any::<u8>(),
    ) {
        let mut bytes = record.encode();
        let at = index.index(bytes.len());
        bytes[at] = byte;
        if let Ok(decoded) = Record::decode(&bytes) {
            prop_assert_eq!(decoded.encode(), bytes);
        }
    }

    #[test]
    fn a_shortened_record_is_refused(record in record(), index in any::<prop::sample::Index>()) {
        let bytes = record.encode();
        let cut = index.index(bytes.len());
        prop_assert!(Record::decode(&bytes[..cut]).is_err());
    }

    #[test]
    fn a_lengthened_record_is_refused(record in record(), extra in vec(any::<u8>(), 1..8)) {
        let mut bytes = record.encode();
        let at = bytes.len();
        bytes.extend(&extra);
        prop_assert_eq!(
            Record::decode(&bytes),
            Err(DecodeError::Trailing { at, extra: extra.len() })
        );
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in vec(any::<u8>(), 0..512)) {
        if let Ok(decoded) = Submission::decode(&bytes) {
            prop_assert_eq!(decoded.canon_bytes(), bytes);
        }
    }
}

fn verse_submission(body: Body) -> Submission {
    Submission {
        stream: StreamId::from("island"),
        writer: [7; 32],
        counter: 1,
        body,
    }
}

fn grant_of(scope: &[&str], rights: &[Kind]) -> Grant {
    Grant {
        holder: [9; 32],
        scope: scope.iter().map(|name| StreamId::from(*name)).collect(),
        rights: rights.iter().copied().collect(),
        narrows: None,
    }
}

/// Offset of the kind tag in a submission on stream "island".
const KIND_AT: usize = 8 + 6 + 32 + 8;

#[test]
fn reserved_and_unknown_kinds_are_refused() {
    let mut bytes = verse_submission(Body::Revoke(Revoke { grant: 1 })).canon_bytes();
    for tag in [6, 7, 10, 255] {
        bytes[KIND_AT] = tag;
        assert_eq!(
            Submission::decode(&bytes),
            Err(DecodeError::BadTag {
                at: KIND_AT,
                what: "kind",
                tag
            })
        );
    }
}

#[test]
fn a_verse_that_names_no_evidence_still_decodes() {
    let verse = Verse {
        evidence: 200,
        text: "no evidence named".into(),
        refs: vec![],
    };
    assert_eq!(verse.evidence(), None);
    let submission = verse_submission(Body::Verse(verse));
    assert_eq!(
        Submission::decode(&submission.canon_bytes()),
        Ok(submission)
    );
}

#[test]
fn a_set_out_of_order_or_repeated_is_refused() {
    // scope {"a", "b"}: count, then each as length + byte, ascending.
    let good = verse_submission(Body::Grant(grant_of(&["a", "b"], &[Kind::Verse])));
    let bytes = good.canon_bytes();
    assert_eq!(Submission::decode(&bytes), Ok(good));

    let scope_at = KIND_AT + 1 + 32; // after the kind tag and the holder
    let first = scope_at + 8; // after the count
    let second = first + 9;
    assert_eq!(&bytes[first + 8..first + 9], b"a");
    assert_eq!(&bytes[second + 8..second + 9], b"b");

    let mut swapped = bytes.clone();
    swapped[first + 8] = b'b';
    swapped[second + 8] = b'a';
    assert_eq!(
        Submission::decode(&swapped),
        Err(DecodeError::UnorderedSet {
            at: second,
            what: "scope"
        })
    );

    let mut repeated = bytes.clone();
    repeated[second + 8] = b'a';
    assert_eq!(
        Submission::decode(&repeated),
        Err(DecodeError::UnorderedSet {
            at: second,
            what: "scope"
        })
    );
}

#[test]
fn a_set_encodes_in_the_order_of_its_elements_encodings() {
    // "b" (length 1) sorts before "aa" (length 2): the length prefix comes
    // first in the encoding, so this is not the order of the names.
    let scope: BTreeSet<StreamId> = [StreamId::from("aa"), StreamId::from("b")].into();
    let bytes = scope.canon_bytes();
    assert_eq!(&bytes[8 + 8..8 + 9], b"b");
    let submission = verse_submission(Body::Grant(grant_of(&["aa", "b"], &[])));
    assert_eq!(
        Submission::decode(&submission.canon_bytes()),
        Ok(submission)
    );
}

#[test]
fn a_verdict_inside_a_verdict_is_refused() {
    let inner = verse_submission(Body::Verdict(Verdict {
        rejected: Box::new(verse_submission(Body::Revoke(Revoke { grant: 1 }))),
        sig_writer: [1; 64],
        reason: Refusal::Invariant(Invariant::VerseNamesEvidence),
    }));
    let outer = verse_submission(Body::Verdict(Verdict {
        rejected: Box::new(inner),
        sig_writer: [2; 64],
        reason: Refusal::Invariant(Invariant::VerseNamesEvidence),
    }));
    assert_eq!(
        Submission::decode(&outer.canon_bytes()),
        Err(DecodeError::BadTag {
            at: KIND_AT + 1 + KIND_AT,
            what: "rejected kind",
            tag: 5
        })
    );
}

#[test]
fn a_huge_count_is_refused_before_allocating() {
    // A bootstrap that claims u64::MAX grants in a few bytes.
    let mut bytes = verse_submission(Body::Bootstrap(Bootstrap { grants: vec![] })).canon_bytes();
    let count_at = KIND_AT + 1;
    bytes[count_at..count_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        Submission::decode(&bytes),
        Err(DecodeError::Truncated {
            at: count_at,
            need: u64::MAX
        })
    );
}

#[test]
fn text_must_be_utf8() {
    let mut bytes = verse_submission(Body::Verse(Verse {
        evidence: 0,
        text: "ab".into(),
        refs: vec![],
    }))
    .canon_bytes();
    let text_at = KIND_AT + 1 + 1 + 8;
    bytes[text_at] = 0xFF;
    assert_eq!(
        Submission::decode(&bytes),
        Err(DecodeError::BadUtf8 { at: text_at })
    );
}
