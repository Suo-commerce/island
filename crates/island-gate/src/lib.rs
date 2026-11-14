//! Island gate: the judge every new write passes, and the ratchet on the
//! policy it judges by (docs/spec.md, "Write path and gate", step 4).
//!
//! [`judge`] says whether a write holds every invariant of the policy in
//! force. A refusal is not thrown away: the appender records it as a
//! `verdict` entry carrying the whole submission, and replay calls `judge`
//! again on every one and requires the same answer. So a judge that
//! changed its mind, or an appender that recorded a refusal the judge
//! never gave, shows up as a log that no longer verifies.
//!
//! [`strengthens`] is the ratchet. A new policy is accepted only if every
//! invariant of the old one is still there, each at least as strict. What
//! that buys is exact: whenever `strengthens(old, new)` holds, every write
//! `new` accepts, `old` accepts too. The harness tests that sentence.
//!
//! Invariants judge new writes only, so tightening never invalidates what
//! is already in the log.
//!
//! One rule is not a policy and is never loosened: a `dispatch` lets out
//! only an action an approver approved, once ([`State::dispatch_refusal`],
//! docs/slice-2.md, "Actions and the sender"). It reads the state the log
//! has replayed to, so replay judges every dispatch again.
//!
//! Pure, and depends on `island-core` alone: values in, values out.

#![warn(missing_docs)]

use island_core::{Body, Evidence, Invariant, Policy, Refusal, State, Submission, Verse};

/// Judge one write under the policy in force.
///
/// - A verse must hold every invariant. The first one that does not, in
///   the policy's own order, is the refusal.
/// - A `policy` entry must strengthen the policy in force.
/// - A `dispatch` must name an unused approval of its request that no
///   later decline overrides, for an action not dispatched before.
/// - Every other kind passes: authority has already decided who may write
///   it.
///
/// `state` is what the log has replayed to before this write.
pub fn judge(state: &State, policy: &Policy, submission: &Submission) -> Result<(), Refusal> {
    match &submission.body {
        Body::Verse(verse) => match policy
            .invariants
            .iter()
            .find(|invariant| !holds(invariant, verse))
        {
            Some(broken) => Err(Refusal::Invariant(*broken)),
            None => Ok(()),
        },
        Body::Policy(new) => {
            if strengthens(policy, new) {
                Ok(())
            } else {
                Err(Refusal::WeakerPolicy)
            }
        }
        Body::Dispatch(dispatch) => match state.dispatch_refusal(
            &dispatch.vessel,
            &dispatch.key,
            &dispatch.request,
            dispatch.approval,
        ) {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// Whether one invariant holds for a verse.
fn holds(invariant: &Invariant, verse: &Verse) -> bool {
    match invariant {
        Invariant::VerseNamesEvidence => verse.evidence().is_some(),
        Invariant::VerseTextMax { bytes } => verse.text.len() as u64 <= u64::from(*bytes),
        Invariant::ArtefactHasRef => {
            verse.evidence() != Some(Evidence::Artefact) || !verse.refs.is_empty()
        }
    }
}

/// Whether `new` may replace `old`: every invariant of `old` is still in
/// `new`, at least as strict. Additions are allowed; nothing is removed
/// and no parameter is loosened.
pub fn strengthens(old: &Policy, new: &Policy) -> bool {
    old.invariants
        .iter()
        .all(|kept| new.invariants.iter().any(|by| at_least(by, kept)))
}

/// Whether `new` is the same invariant as `old`, at least as strict.
/// `VerseTextMax` is the one with a direction: its bound may only go down.
fn at_least(new: &Invariant, old: &Invariant) -> bool {
    match (new, old) {
        (Invariant::VerseNamesEvidence, Invariant::VerseNamesEvidence)
        | (Invariant::ArtefactHasRef, Invariant::ArtefactHasRef) => true,
        (Invariant::VerseTextMax { bytes: new }, Invariant::VerseTextMax { bytes: old }) => {
            new <= old
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use island_core::{Preference, StreamId};

    use super::*;

    const NAMES: Invariant = Invariant::VerseNamesEvidence;
    const HAS_REF: Invariant = Invariant::ArtefactHasRef;

    const fn max(bytes: u32) -> Invariant {
        Invariant::VerseTextMax { bytes }
    }

    fn policy(invariants: &[Invariant]) -> Policy {
        Policy {
            evaluator: "test".to_owned(),
            invariants: invariants.to_vec(),
        }
    }

    fn submission(body: Body) -> Submission {
        Submission {
            stream: StreamId::from("island"),
            writer: [1; 32],
            counter: 1,
            body,
        }
    }

    fn verse(evidence: u8, text: &str, refs: usize) -> Submission {
        submission(Body::Verse(Verse {
            evidence,
            text: text.to_owned(),
            refs: vec![[7; 32]; refs],
        }))
    }

    fn judged(invariants: &[Invariant], submission: &Submission) -> Result<(), Refusal> {
        judge(&State::default(), &policy(invariants), submission)
    }

    #[test]
    fn a_verse_must_name_its_evidence() {
        for evidence in 0..=3 {
            assert_eq!(judged(&[NAMES], &verse(evidence, "x", 0)), Ok(()));
        }
        for evidence in [4, 200, 255] {
            assert_eq!(
                judged(&[NAMES], &verse(evidence, "x", 0)),
                Err(Refusal::Invariant(NAMES))
            );
            // Without the invariant, such a verse passes.
            assert_eq!(judged(&[], &verse(evidence, "x", 0)), Ok(()));
        }
    }

    #[test]
    fn a_verse_fits_the_text_bound_in_bytes() {
        assert_eq!(judged(&[max(5)], &verse(0, "12345", 0)), Ok(()));
        assert_eq!(
            judged(&[max(5)], &verse(0, "123456", 0)),
            Err(Refusal::Invariant(max(5)))
        );
        // Bytes of UTF-8, not characters: "ää" is four bytes.
        assert_eq!(judged(&[max(4)], &verse(0, "ää", 0)), Ok(()));
        assert_eq!(
            judged(&[max(3)], &verse(0, "ää", 0)),
            Err(Refusal::Invariant(max(3)))
        );
        assert_eq!(judged(&[max(0)], &verse(0, "", 0)), Ok(()));
    }

    #[test]
    fn an_artefact_verse_carries_a_ref() {
        let artefact = Evidence::Artefact.as_u8();
        assert_eq!(
            judged(&[HAS_REF], &verse(artefact, "x", 0)),
            Err(Refusal::Invariant(HAS_REF))
        );
        assert_eq!(judged(&[HAS_REF], &verse(artefact, "x", 1)), Ok(()));
        // Other evidence needs none.
        assert_eq!(
            judged(&[HAS_REF], &verse(Evidence::Decision.as_u8(), "x", 0)),
            Ok(())
        );
        // A verse that names no evidence is not an artefact verse.
        assert_eq!(judged(&[HAS_REF], &verse(9, "x", 0)), Ok(()));
    }

    #[test]
    fn the_first_broken_invariant_in_the_policys_order_is_the_refusal() {
        let both = verse(9, "too long", 0);
        assert_eq!(
            judged(&[NAMES, max(3)], &both),
            Err(Refusal::Invariant(NAMES))
        );
        assert_eq!(
            judged(&[max(3), NAMES], &both),
            Err(Refusal::Invariant(max(3)))
        );
    }

    #[test]
    fn other_kinds_pass() {
        let preference = submission(Body::Preference(Preference {
            key: "k".into(),
            value: "a value far longer than any bound".into(),
        }));
        assert_eq!(judged(&[NAMES, max(1), HAS_REF], &preference), Ok(()));
    }

    #[test]
    fn a_policy_may_only_get_stronger() {
        let strong = |old: &[Invariant], new: &[Invariant]| strengthens(&policy(old), &policy(new));

        // The same, an addition, a lower bound.
        assert!(strong(&[NAMES, max(4096)], &[NAMES, max(4096)]));
        assert!(strong(&[NAMES, max(4096)], &[NAMES, max(4096), HAS_REF]));
        assert!(strong(&[NAMES, max(4096)], &[NAMES, max(100)]));
        assert!(strong(&[], &[NAMES]));
        assert!(strong(&[], &[]));
        // Order does not matter.
        assert!(strong(&[NAMES, max(10)], &[max(10), NAMES]));

        // A removal, a higher bound, a swap of one invariant for another.
        assert!(!strong(&[NAMES, max(4096)], &[max(4096)]));
        assert!(!strong(&[NAMES, max(4096)], &[NAMES]));
        assert!(!strong(&[NAMES, max(4096)], &[NAMES, max(4097)]));
        assert!(!strong(&[NAMES], &[HAS_REF]));
        assert!(!strong(&[NAMES], &[]));
    }

    #[test]
    fn a_policy_entry_is_judged_against_the_policy_in_force() {
        let entry = |invariants: &[Invariant]| submission(Body::Policy(policy(invariants)));
        assert_eq!(
            judged(&[NAMES, max(10)], &entry(&[NAMES, max(5), HAS_REF])),
            Ok(())
        );
        assert_eq!(
            judged(&[NAMES, max(10)], &entry(&[NAMES, max(11)])),
            Err(Refusal::WeakerPolicy)
        );
        assert_eq!(judged(&[NAMES], &entry(&[])), Err(Refusal::WeakerPolicy));
        // The evaluator named by a policy is free to change.
        let mut renamed = policy(&[NAMES]);
        renamed.evaluator = "another build".to_owned();
        assert_eq!(judged(&[NAMES], &submission(Body::Policy(renamed))), Ok(()));
    }

    /// Stronger means subset, checked over every policy and every verse of
    /// a small world: whenever `strengthens(old, new)`, every verse `new`
    /// accepts, `old` accepts.
    #[test]
    fn stronger_means_subset_exhaustively() {
        let pieces = [NAMES, max(0), max(1), max(2), HAS_REF];
        let policies: Vec<Vec<Invariant>> = (0u32..1 << pieces.len())
            .map(|mask| {
                (0..pieces.len())
                    .filter(|bit| mask & (1 << bit) != 0)
                    .map(|bit| pieces[bit])
                    .collect()
            })
            .collect();
        let mut verses = Vec::new();
        for evidence in 0..=4 {
            for text in ["", "a", "ab", "abc"] {
                for refs in 0..=1 {
                    verses.push(verse(evidence, text, refs));
                }
            }
        }

        let (mut pairs, mut stronger) = (0, 0);
        for old in &policies {
            for new in &policies {
                pairs += 1;
                if !strengthens(&policy(old), &policy(new)) {
                    continue;
                }
                stronger += 1;
                for verse in &verses {
                    if judged(new, verse).is_ok() {
                        assert_eq!(
                            judged(old, verse),
                            Ok(()),
                            "{new:?} strengthens {old:?} but accepts a verse it refuses"
                        );
                    }
                }
            }
        }
        // The check is not empty either way.
        assert_eq!(pairs, 1024);
        assert!(stronger > 100 && stronger < pairs, "{stronger} of {pairs}");
    }
}
