//! The harness run: archived failures first, then seeds.
//!
//! `ISLAND_HARNESS_SEEDS` sets how many seeds (`just SEEDS=10000
//! harness`). Seeds are split over eight tests so that the test runner
//! spreads them over the cores.

use std::collections::BTreeMap;

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
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|label| !coverage.contains_key(*label))
        .collect();
    assert!(missing.is_empty(), "never reached: {missing:?}");
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
