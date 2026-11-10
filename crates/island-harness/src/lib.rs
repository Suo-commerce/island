//! Island harness: generated cases against the real appender, with the
//! reference model beside it (docs/spec.md, "Harness for the slice").
//!
//! - [`sim`]: storage that fails and crashes on command, a clock set by
//!   hand, a seeded generator.
//! - [`ops`]: the operations a case is made of, and their generators.
//! - [`world`]: one case, and the oracles checked after every operation.
//! - [`ratchet`]: oracle 6 on generated pairs of any two policies.
//! - [`archive`]: every case that ever failed, kept and re-run.
//!
//! A seed is a number. [`generate`] turns it into a case, the same case
//! every time. [`check`] runs a case twice and compares the two traces
//! byte for byte, so anything that depends on the clock, on the heap or on
//! luck shows up as a difference. [`run_seed`] does both and, when an
//! oracle breaks, shrinks the case to a small one that still breaks it.

#![warn(missing_docs)]

pub mod archive;
pub mod ops;
pub mod ratchet;
pub mod sim;
pub mod world;

use std::collections::BTreeMap;

use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

pub use archive::Archived;
pub use ops::Op;
pub use world::{Failure, Report};

/// Seeds per run when `ISLAND_HARNESS_SEEDS` is not set: enough for
/// `just test` to stay quick. `just harness` asks for 10,000.
pub const DEFAULT_SEEDS: u64 = 256;

/// How many seeds this run covers.
pub fn seeds() -> u64 {
    std::env::var("ISLAND_HARNESS_SEEDS")
        .ok()
        .and_then(|seeds| seeds.parse().ok())
        .unwrap_or(DEFAULT_SEEDS)
}

/// A proptest runner whose every choice follows from `seed`.
fn runner(seed: u64) -> TestRunner {
    // The seed, spread over the 32 bytes ChaCha takes.
    let mut bytes = [0u8; 32];
    for (index, chunk) in bytes.chunks_mut(8).enumerate() {
        chunk.copy_from_slice(
            &(seed ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_le_bytes(),
        );
    }
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &bytes);
    TestRunner::new_with_rng(Config::default(), rng)
}

fn tree(seed: u64) -> impl ValueTree<Value = Vec<Op>> {
    ops::case()
        .new_tree(&mut runner(seed))
        .expect("the case strategy never rejects")
}

/// The case a seed stands for.
pub fn generate(seed: u64) -> Vec<Op> {
    tree(seed).current()
}

/// Run a case twice. `Err` if an oracle breaks, or if the two runs differ
/// in a single byte of their traces.
pub fn check(ops: &[Op]) -> Result<Report, Failure> {
    let first = world::run(ops)?;
    let second = world::run(ops)?;
    if first.trace != second.trace {
        let line = first
            .trace
            .iter()
            .zip(&second.trace)
            .position(|(a, b)| a != b)
            .unwrap_or(first.trace.len().min(second.trace.len()));
        let show = |trace: &[String]| trace.get(line).cloned().unwrap_or("(nothing)".to_owned());
        return Err(Failure {
            oracle: "determinism",
            step: line,
            message: format!(
                "two runs differ at event {line}: {:?} then {:?}",
                show(&first.trace),
                show(&second.trace)
            ),
        });
    }
    Ok(first)
}

/// Run one seed. On failure, the case is shrunk to a small one that still
/// breaks an oracle and returned ready for the archive.
pub fn run_seed(seed: u64) -> Result<Report, Box<Archived>> {
    let mut tree = tree(seed);
    let first = match check(&tree.current()) {
        Ok(report) => return Ok(report),
        Err(failure) => failure,
    };

    // Proptest's shrinking, by hand: simplify while it still fails, step
    // back when it stops failing.
    let mut smallest = (tree.current(), first);
    if tree.simplify() {
        loop {
            let ops = tree.current();
            let more = match check(&ops) {
                Err(failure) => {
                    smallest = (ops, failure);
                    tree.simplify()
                }
                Ok(_) => tree.complicate(),
            };
            if !more {
                break;
            }
        }
    }

    let (ops, failure) = smallest;
    Err(Box::new(Archived {
        oracle: failure.oracle.to_owned(),
        seed,
        step: failure.step,
        message: failure.message,
        ops,
    }))
}

/// Add one report's coverage to a total.
pub fn add_coverage(total: &mut BTreeMap<String, u64>, report: &Report) {
    for (label, count) in &report.coverage {
        *total.entry(label.clone()).or_default() += count;
    }
}
