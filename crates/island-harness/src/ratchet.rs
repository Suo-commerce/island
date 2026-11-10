//! Oracle 6 on its own: stronger means subset, on generated pairs of
//! policies (docs/spec.md, "The ratchet in slice 1").
//!
//! The promise of the ratchet is one sentence: whenever
//! `strengthens(old, new)` holds, every write `new` accepts, `old` accepts
//! too. A case in `world.rs` checks it on the policies that came into
//! force there, which the gate already let through. Here the pairs are
//! any two policies: a generated one, and the same one after a few
//! generated changes (a bound lowered, an invariant added, one removed, a
//! bound raised, the order turned round), so both sides of `strengthens`
//! are reached, and the invariant lists may repeat and contradict
//! themselves in ways no sensible policy would.

use island_core::{Body, Invariant, Policy, State, StreamId, Submission, Verse};
use island_gate::{judge, strengthens};
use proptest::prelude::*;
use proptest::strategy::ValueTree;

/// Two policies and the writes to set them against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    /// The policy in force.
    pub old: Policy,
    /// The policy that would replace it.
    pub new: Policy,
    /// Verses: any evidence byte, texts on both sides of the bounds, with
    /// and without refs.
    pub verses: Vec<Verse>,
}

/// What became of a pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// `new` strengthens `old`, and it accepted nothing `old` refuses.
    Stronger,
    /// `new` does not strengthen `old`, and none of these verses shows a
    /// difference.
    NotStronger,
    /// `new` does not strengthen `old`, and it accepts a verse `old`
    /// refuses: the loosening is real.
    Loosens,
}

/// One change to a policy.
#[derive(Clone, Debug)]
enum Change {
    /// Lower a text bound.
    Tighten { which: usize, by: u32 },
    /// Raise a text bound.
    Loosen { which: usize, by: u32 },
    /// Add an invariant, anywhere.
    Add { at: usize, invariant: Invariant },
    /// Remove one.
    Remove { which: usize },
    /// Turn the order round.
    Reverse,
}

fn invariant() -> impl Strategy<Value = Invariant> {
    prop_oneof![
        Just(Invariant::VerseNamesEvidence),
        (0u32..64).prop_map(|bytes| Invariant::VerseTextMax { bytes }),
        Just(Invariant::ArtefactHasRef),
    ]
}

fn policy() -> impl Strategy<Value = Policy> {
    prop::collection::vec(invariant(), 0..5).prop_map(|invariants| Policy {
        evaluator: "ratchet".to_owned(),
        invariants,
    })
}

fn change() -> impl Strategy<Value = Change> {
    prop_oneof![
        3 => (any::<usize>(), 0u32..16).prop_map(|(which, by)| Change::Tighten { which, by }),
        2 => (any::<usize>(), 1u32..16).prop_map(|(which, by)| Change::Loosen { which, by }),
        3 => (any::<usize>(), invariant()).prop_map(|(at, invariant)| Change::Add { at, invariant }),
        2 => any::<usize>().prop_map(|which| Change::Remove { which }),
        1 => Just(Change::Reverse),
    ]
}

fn changed(mut policy: Policy, changes: &[Change]) -> Policy {
    let invariants = &mut policy.invariants;
    for change in changes {
        // The text bounds, by position.
        let bounds: Vec<usize> = (0..invariants.len())
            .filter(|index| matches!(invariants[*index], Invariant::VerseTextMax { .. }))
            .collect();
        let bound = |which: usize| bounds.get(which % bounds.len().max(1)).copied();
        match change {
            Change::Tighten { which, by } => {
                if let Some(Invariant::VerseTextMax { bytes }) =
                    bound(*which).map(|index| &mut invariants[index])
                {
                    *bytes = bytes.saturating_sub(*by);
                }
            }
            Change::Loosen { which, by } => {
                if let Some(Invariant::VerseTextMax { bytes }) =
                    bound(*which).map(|index| &mut invariants[index])
                {
                    *bytes += by;
                }
            }
            Change::Add { at, invariant } => {
                invariants.insert(at % (invariants.len() + 1), *invariant);
            }
            Change::Remove { which } => {
                if !invariants.is_empty() {
                    invariants.remove(which % invariants.len());
                }
            }
            Change::Reverse => invariants.reverse(),
        }
    }
    policy
}

fn verse() -> impl Strategy<Value = Verse> {
    (
        prop_oneof![6 => 0u8..4, 1 => 4u8..=255],
        0usize..90,
        0usize..3,
    )
        .prop_map(|(evidence, len, refs)| Verse {
            evidence,
            text: "v".repeat(len),
            refs: vec![[7; 32]; refs],
        })
}

fn pair() -> impl Strategy<Value = Pair> {
    (
        policy(),
        prop::collection::vec(change(), 0..4),
        prop::collection::vec(verse(), 24),
    )
        .prop_map(|(old, changes, verses)| Pair {
            new: changed(old.clone(), &changes),
            old,
            verses,
        })
}

/// The pair a seed stands for: the same pair every time.
pub fn generate(seed: u64) -> Pair {
    pair()
        .new_tree(&mut crate::runner(seed))
        .expect("the pair strategy never rejects")
        .current()
}

/// Check one pair. `Err` names a verse the new policy accepts and the old
/// one refuses although `strengthens(old, new)` holds.
pub fn check(pair: &Pair) -> Result<Held, String> {
    let state = State::default();
    let accepts = |policy: &Policy, verse: &Verse| {
        let write = Submission {
            stream: StreamId::from("island"),
            writer: [0; 32],
            counter: 1,
            body: Body::Verse(verse.clone()),
        };
        judge(&state, policy, &write).is_ok()
    };
    let widened = pair
        .verses
        .iter()
        .find(|verse| accepts(&pair.new, verse) && !accepts(&pair.old, verse));
    match (strengthens(&pair.old, &pair.new), widened) {
        (true, Some(verse)) => Err(format!(
            "strengthens({:?}, {:?}) holds, but the new policy accepts a verse the old one refuses: evidence {}, {} bytes, {} refs",
            pair.old.invariants,
            pair.new.invariants,
            verse.evidence,
            verse.text.len(),
            verse.refs.len()
        )),
        (true, None) => Ok(Held::Stronger),
        (false, Some(_)) => Ok(Held::Loosens),
        (false, None) => Ok(Held::NotStronger),
    }
}
