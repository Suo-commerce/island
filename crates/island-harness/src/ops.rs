//! The operations a case is made of, and how proptest generates them.
//!
//! An operation is plain data: small numbers that the world (see
//! `world.rs`) turns into submissions against the state it is in. A
//! counter is an offset from the writer's next number, a writer is an
//! index into four fixed keys, a stream an index into six fixed names. So
//! every operation means something in every state, the generator needs no
//! knowledge of what should happen, and a case can be written to a file
//! and read back exactly.
//!
//! What should happen is the model's to say.

use proptest::prelude::*;
use serde::{Deserialize, Serialize};

/// Writers 0, 1 and 2 are ordinary keys; writer 3 is the pinned root key.
pub const WRITERS: u8 = 4;

/// The root key's writer index.
pub const ROOT: u8 = 3;

/// The streams an operation can name, by index.
pub const STREAMS: [&str; 6] = ["island", "flare", "plan", "policy", "authority", "gate"];

/// One step of a case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// A writer submits something.
    Write(Write),
    /// A writer sends its latest accepted submission again, byte for byte.
    Resend {
        /// Which writer.
        writer: u8,
    },
    /// A writer sends an older accepted submission again: a replay.
    ReplayOld {
        /// Which writer.
        writer: u8,
        /// How many submissions before its latest.
        back: u8,
    },
    /// The clock moves forward.
    Tick {
        /// By how much.
        micros: u32,
    },
    /// The clock jumps back.
    ClockBack {
        /// By how much.
        micros: u32,
    },
    /// The next sync fails, and the crash that follows keeps `keep` of
    /// the bytes that were not yet durable.
    FailSync {
        /// Bytes of the unsynced record that survive the crash.
        keep: u16,
    },
    /// The next append stops after `at` bytes and fails, and the crash
    /// that follows keeps `keep` of what it wrote.
    TearAppend {
        /// Bytes written before the failure.
        at: u16,
        /// Bytes of those that survive the crash.
        keep: u16,
    },
    /// The process dies between operations and the log is opened again.
    Crash,
}

/// A submission, described relative to the state it meets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Write {
    /// Which writer (index, modulo [`WRITERS`]).
    pub writer: u8,
    /// The counter, as an offset from the writer's next number: 0 is the
    /// right one, negative is old, positive skips ahead.
    pub counter: i8,
    /// Makes otherwise equal submissions differ.
    pub salt: u8,
    /// Which stream (index into [`STREAMS`], modulo its length).
    pub stream: u8,
    /// What is submitted.
    pub what: What,
    /// How it is signed.
    pub sign: Sign,
}

/// The kind and body of a submission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum What {
    /// A verse.
    Verse {
        /// The raw evidence byte; 0 to 3 name a kind.
        evidence: u8,
        /// Length of the text's filler.
        len: u8,
        /// How many refs (modulo 3).
        refs: u8,
    },
    /// A preference.
    Preference,
    /// A policy entry.
    Policy {
        /// Which invariants, as a bit mask over the three tags.
        invariants: u8,
        /// The `VerseTextMax` bound, if that bit is set.
        max: u16,
    },
    /// A grant.
    Grant {
        /// Holder (writer index).
        holder: u8,
        /// Scope, as a bit mask over [`STREAMS`].
        scope: u8,
        /// Rights, as a bit mask over the kinds.
        rights: u8,
        /// The grant it narrows.
        narrows: Option<u8>,
    },
    /// A revoke.
    Revoke {
        /// Which grant.
        grant: u8,
    },
    /// A genesis declaring some writer's key as root.
    Genesis {
        /// Whose key is declared (writer index).
        root: u8,
    },
    /// A bootstrap.
    Bootstrap {
        /// Who it names as appender.
        appenders: Appenders,
    },
    /// A verdict, which only the appender may write, and not yet.
    Verdict,
}

/// Who a bootstrap entry names as appender.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appenders {
    /// The appender that runs the log.
    Right,
    /// Nobody.
    None,
    /// The right one and another.
    Two,
    /// The root key.
    Root,
    /// Only another key.
    Other,
}

/// How a submission is signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sign {
    /// By its writer.
    Good,
    /// By its writer, then one bit of the signature flipped.
    Flipped {
        /// Which bit (modulo 512).
        bit: u16,
    },
    /// By another writer's key.
    By {
        /// Which writer.
        writer: u8,
    },
}

fn authority_write(writer: u8, what: What) -> Op {
    Op::Write(Write {
        writer,
        counter: 0,
        salt: 0,
        stream: 4, // authority
        what,
        sign: Sign::Good,
    })
}

/// The two operations that start a log properly.
pub fn start() -> [Op; 2] {
    [
        authority_write(ROOT, What::Genesis { root: ROOT }),
        authority_write(
            ROOT,
            What::Bootstrap {
                appenders: Appenders::Right,
            },
        ),
    ]
}

/// The stream a kind belongs on, as an index into [`STREAMS`].
fn home(what: &What) -> u8 {
    match what {
        What::Verse { .. } | What::Preference => 0,
        What::Policy { .. } => 3,
        What::Grant { .. }
        | What::Revoke { .. }
        | What::Genesis { .. }
        | What::Bootstrap { .. } => 4,
        What::Verdict => 5,
    }
}

fn appenders() -> impl Strategy<Value = Appenders> {
    prop::sample::select(vec![
        Appenders::Right,
        Appenders::None,
        Appenders::Two,
        Appenders::Root,
        Appenders::Other,
    ])
}

fn what() -> impl Strategy<Value = What> {
    prop_oneof![
        10 => (0u8..6, 0u8..40, 0u8..3)
            .prop_map(|(evidence, len, refs)| What::Verse { evidence, len, refs }),
        2 => Just(What::Preference),
        2 => (0u8..8, 0u16..5000).prop_map(|(invariants, max)| What::Policy { invariants, max }),
        2 => (0..WRITERS, any::<u8>(), any::<u8>(), prop::option::of(0u8..8)).prop_map(
            |(holder, scope, rights, narrows)| What::Grant { holder, scope, rights, narrows }
        ),
        1 => (0u8..8).prop_map(|grant| What::Revoke { grant }),
        1 => (0..WRITERS).prop_map(|root| What::Genesis { root }),
        1 => appenders().prop_map(|appenders| What::Bootstrap { appenders }),
        1 => Just(What::Verdict),
    ]
}

fn write() -> impl Strategy<Value = Write> {
    (
        0..WRITERS,
        prop_oneof![8 => Just(0i8), 2 => -3i8..=3],
        0u8..4,
        what(),
        // Mostly the stream the kind belongs on, sometimes any stream.
        prop_oneof![6 => Just(None), 1 => (0u8..6).prop_map(Some)],
        prop_oneof![
            12 => Just(Sign::Good),
            1 => (0u16..512).prop_map(|bit| Sign::Flipped { bit }),
            1 => (0..WRITERS).prop_map(|writer| Sign::By { writer }),
        ],
    )
        .prop_map(|(writer, counter, salt, what, stream, sign)| Write {
            writer,
            counter,
            salt,
            stream: stream.unwrap_or_else(|| home(&what)),
            what,
            sign,
        })
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        14 => write().prop_map(Op::Write),
        1 => (0..WRITERS).prop_map(|writer| Op::Resend { writer }),
        1 => (0..WRITERS, 0u8..3).prop_map(|(writer, back)| Op::ReplayOld { writer, back }),
        1 => (1u32..5_000_000).prop_map(|micros| Op::Tick { micros }),
        1 => (1u32..5_000_000).prop_map(|micros| Op::ClockBack { micros }),
        1 => (0u16..600).prop_map(|keep| Op::FailSync { keep }),
        1 => (0u16..600, 0u16..600).prop_map(|(at, keep)| Op::TearAppend { at, keep }),
        1 => Just(Op::Crash),
    ]
}

/// How a case begins. Most begin with a properly started log, so that the
/// rest of the case has something to work on; the others begin at the
/// places where a log can go wrong before it has started.
fn beginning() -> impl Strategy<Value = Vec<Op>> {
    let [genesis, bootstrap] = start();
    prop_oneof![
        14 => Just(vec![genesis.clone(), bootstrap]),
        2 => Just(vec![genesis.clone()]),
        2 => Just(Vec::new()),
        // A genesis, then a bootstrap naming anyone as appender.
        3 => appenders().prop_map(move |appenders| {
            vec![
                genesis.clone(),
                authority_write(ROOT, What::Bootstrap { appenders }),
            ]
        }),
        // Any writer declaring itself root.
        2 => (0..WRITERS).prop_map(|writer| {
            vec![authority_write(writer, What::Genesis { root: writer })]
        }),
    ]
}

/// A case: a beginning, then up to 48 operations.
pub fn case() -> impl Strategy<Value = Vec<Op>> {
    (beginning(), prop::collection::vec(op(), 0..48)).prop_map(|(mut case, ops)| {
        case.extend(ops);
        case
    })
}
