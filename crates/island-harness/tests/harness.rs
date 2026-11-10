//! The harness run: archived failures first, then seeds.
//!
//! `ISLAND_HARNESS_SEEDS` sets how many seeds (`just SEEDS=10000
//! harness`). Seeds are split over eight tests so that the test runner
//! spreads them over the cores.

use std::collections::BTreeMap;

use island_harness::ratchet::{self, Held};
use island_harness::{add_coverage, archive, check, generate, run_seed, seeds};

/// Every archived failure passes. Run before the seeds by `just harness`.
#[test]
fn archived_seeds_pass() {
    let archived = archive::load(&archive::dir()).expect("read the seed archive");
    for (path, case) in &archived {
        if let Err(failure) = check(&case.ops) {
            panic!(
                "{} fails again (it once broke oracle {}): {failure}",
                path.display(),
                case.oracle
            );
        }
    }
    println!("{} archived cases pass", archived.len());
}

const SHARDS: u64 = 8;

fn shard(shard: u64) {
    for seed in (0..seeds()).filter(|seed| seed % SHARDS == shard) {
        if let Err(found) = run_seed(seed) {
            let saved = match archive::save(&archive::dir(), &found) {
                Ok(path) => format!("archived as {}; commit it with the fix", path.display()),
                Err(error) => format!("could not be archived: {error}"),
            };
            panic!(
                "seed {seed} breaks oracle {} at operation {} of its shrunk case ({} operations): {}\n{saved}",
                found.oracle,
                found.step,
                found.ops.len(),
                found.message
            );
        }
    }
}

macro_rules! shards {
    ($($name:ident = $index:expr),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                shard($index);
            }
        )*
    };
}

shards! {
    seeds_shard_0 = 0, seeds_shard_1 = 1, seeds_shard_2 = 2, seeds_shard_3 = 3,
    seeds_shard_4 = 4, seeds_shard_5 = 5, seeds_shard_6 = 6, seeds_shard_7 = 7,
}

/// A harness that passes because its cases never do anything interesting
/// proves nothing. Over a fixed set of seeds, every outcome the model can
/// give and every kind of storage fault must actually have happened.
#[test]
fn the_generators_reach_every_outcome() {
    let mut coverage = BTreeMap::new();
    for seed in 0..600 {
        let report =
            check(&generate(seed)).unwrap_or_else(|failure| panic!("seed {seed}: {failure}"));
        add_coverage(&mut coverage, &report);
    }
    println!("{coverage:#?}");

    let required = [
        "accepted",
        "retry",
        "crash",
        "clock-back",
        "fault:lost",
        "fault:landed",
        "torn-tail-dropped",
        "refused:BadSignature",
        "refused:CounterOld",
        "refused:CounterGap",
        "refused:CounterConflict",
        "refused:NotGenesis",
        "refused:RootNotPinned",
        "refused:NotBootstrap",
        "refused:BootstrapAppender",
        "refused:BootstrapRoot",
        "refused:NoGrant",
        "refused:UnknownGrant",
        "refused:EndedGrant",
        "refused:OtherHolder",
        "refused:Wider",
        "refused:Ungrantable",
        "refused:Guarded",
        "grant:new",
        "grant:narrowed",
        "grant:revoked",
        "refused:WrongAppender",
        "refused:Misplaced",
        "refused:Verdict",
        "refused:Stream",
        "refused:AppenderWrites",
        "policy:first",
        "policy:tightened",
        "verse:passed-the-gate",
        "verdict:VerseNamesEvidence",
        "verdict:VerseTextMax",
        "verdict:ArtefactHasRef",
        "verdict:WeakerPolicy",
        "verdict-retry",
        "fault:verdict-landed",
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|label| !coverage.contains_key(*label))
        .collect();
    assert!(missing.is_empty(), "never reached: {missing:?}");
}

/// Oracle 6 on generated pairs of any two policies, ten pairs a seed:
/// wherever `strengthens(old, new)` holds, the new policy accepts nothing
/// the old one refuses.
#[test]
fn stronger_means_subset_on_generated_pairs() {
    let mut count = BTreeMap::new();
    for seed in 0..seeds() * 10 {
        let pair = ratchet::generate(seed);
        assert_eq!(
            pair,
            ratchet::generate(seed),
            "pair {seed} is not the same twice"
        );
        match ratchet::check(&pair) {
            Ok(held) => *count.entry(format!("{held:?}")).or_insert(0u64) += 1,
            Err(broken) => panic!("pair {seed} breaks oracle 6-ratchet: {broken}"),
        }
    }
    println!("{count:#?}");
    // Both sides of `strengthens` were reached, and the loosenings among
    // the pairs were real ones.
    for held in [Held::Stronger, Held::NotStronger, Held::Loosens] {
        assert!(
            count.contains_key(&format!("{held:?}")),
            "never reached: {held:?}"
        );
    }
}

#[test]
fn a_seed_is_the_same_case_every_time() {
    for seed in [0, 1, 2, 99, 12_345] {
        assert_eq!(generate(seed), generate(seed));
    }
    assert_ne!(generate(1), generate(2));
}

#[test]
fn an_archived_case_reads_back_exactly() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case = island_harness::Archived {
        oracle: "3-model".to_owned(),
        seed: 42,
        step: 3,
        message: "an example".to_owned(),
        ops: generate(42),
    };
    let path = archive::save(dir.path(), &case).expect("save");
    assert!(path.ends_with("3-model-seed-42.json"));
    let loaded = archive::load(dir.path()).expect("load");
    assert_eq!(loaded, [(path, case)]);
    assert!(
        archive::load(&dir.path().join("missing"))
            .expect("no directory")
            .is_empty()
    );
}
