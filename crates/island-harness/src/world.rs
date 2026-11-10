//! One case: the real appender over simulated storage and clock, the
//! reference model beside it, and the oracles checked after every
//! operation (docs/spec.md, "Harness for the slice").
//!
//! Oracles, by the spec's numbers:
//!
//! 1. The stored chain verifies: `seq` gap-free, `prev`, the hash and both
//!    signatures, checked here from `island-core` alone, without the log's
//!    own replay.
//! 2. Replaying what is stored gives exactly the state the appender judges
//!    from.
//! 3. The model agrees on every accept, retry and refusal, and on the
//!    state. (The gate's part joins with build step 6.)
//! 4. No acknowledged entry is ever missing or changed; nothing is
//!    acknowledged before it is durable; after a crash the log holds what
//!    the model holds, plus at most the one submission that was in flight.
//! 7. An exact resend returns the first answer and adds nothing.
//!
//! Oracles 5 (grants) and 6 (stronger means subset) join with build steps
//! 5 and 6.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use island_core::{
    Body, Bootstrap, Evidence, Genesis, Grant, Hash, Invariant, Kind, Policy, Preference,
    PublicKey, Record, Refusal, Revoke, Signature, SigningKey, StreamId, Submission, Verdict,
    Verse, public_key, sign_hash,
};
use island_log::{Ack, Appender, Chain, Fault, Rejected, frame};
use island_model::{Model, Outcome, Why};

use crate::ops::{Appenders, Op, STREAMS, Sign, WRITERS, What, Write};
use crate::sim::{SimClock, SimStorage};

/// An oracle that does not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Which oracle: `1-chain`, `2-replay`, `3-model`, `4-durability`,
    /// `7-retry` or `determinism`.
    pub oracle: &'static str,
    /// The index of the operation after which it broke.
    pub step: usize,
    /// What was seen.
    pub message: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "oracle {} broke at operation {}: {}",
            self.oracle, self.step, self.message
        )
    }
}

impl std::error::Error for Failure {}

/// What a case did: one line per event, and how often each kind of thing
/// happened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The full event trace. Two runs of one case must give the same.
    pub trace: Vec<String>,
    /// How many times each outcome was reached, by label.
    pub coverage: BTreeMap<String, u64>,
}

type Check = Result<(), Failure>;

fn key(writer: u8) -> SigningKey {
    match writer % WRITERS {
        3 => SigningKey::from_bytes(&[0x52; 32]),
        index => SigningKey::from_bytes(&[0x10 + index; 32]),
    }
}

fn appender_key() -> SigningKey {
    SigningKey::from_bytes(&[0x41; 32])
}

fn other_appender() -> PublicKey {
    public_key(&SigningKey::from_bytes(&[0x67; 32]))
}

fn pin() -> PublicKey {
    public_key(&key(crate::ops::ROOT))
}

fn stream(index: u8) -> StreamId {
    StreamId::from(STREAMS[usize::from(index) % STREAMS.len()])
}

fn short(bytes: &[u8]) -> String {
    bytes[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What oracle 1 has verified so far, so each operation only pays for the
/// records it added.
#[derive(Debug, Default)]
struct Verified {
    bytes: Vec<u8>,
    hashes: Vec<Hash>,
    last_at: u64,
}

struct World {
    storage: SimStorage,
    clock: SimClock,
    appender: Appender<SimStorage, SimClock>,
    model: Model,
    /// Per writer: the submissions that are in the log, with signatures.
    sent: BTreeMap<PublicKey, Vec<(Submission, Signature)>>,
    /// Every acknowledged entry: position to hash.
    acked: BTreeMap<u64, Hash>,
    verified: Verified,
    /// Oracle 2's own replay of the stored records.
    shadow: Chain,
    /// Bytes of an unsynced record the next crash keeps.
    crash_keep: usize,
    step: usize,
    report: Report,
}

/// Run one case from an empty log. `Ok` carries the trace; `Err` the first
/// oracle that broke.
pub fn run(ops: &[Op]) -> Result<Report, Failure> {
    let storage = SimStorage::default();
    let clock = SimClock::at(1_000_000);
    let (appender, _) = Appender::open(storage.clone(), clock.clone(), appender_key(), pin())
        .map_err(|error| Failure {
            oracle: "4-durability",
            step: 0,
            message: format!("an empty log does not open: {error}"),
        })?;
    let mut world = World {
        storage,
        clock,
        appender,
        model: Model::new(pin(), public_key(&appender_key())),
        sent: BTreeMap::new(),
        acked: BTreeMap::new(),
        verified: Verified::default(),
        shadow: Chain::new(pin()),
        crash_keep: 0,
        step: 0,
        report: Report::default(),
    };
    for (step, op) in ops.iter().enumerate() {
        world.step = step;
        world.apply(op)?;
        world.check()?;
    }
    // Once more from nothing, in case the incremental checks hid anything.
    world.step = ops.len();
    world.verified = Verified::default();
    world.shadow = Chain::new(pin());
    world.check()?;
    Ok(world.report)
}

impl World {
    fn fail(&self, oracle: &'static str, message: impl Into<String>) -> Failure {
        Failure {
            oracle,
            step: self.step,
            message: message.into(),
        }
    }

    fn hit(&mut self, label: impl Into<String>) {
        *self.report.coverage.entry(label.into()).or_default() += 1;
    }

    fn note(&mut self, event: String) {
        self.report.trace.push(format!("{:>3} {event}", self.step));
    }

    // ── Operations ──────────────────────────────────────────────────

    fn apply(&mut self, op: &Op) -> Check {
        match op {
            Op::Write(write) => {
                let (submission, signature) = self.build(write);
                self.submit(submission, signature)
            }
            Op::Resend { writer } => {
                let writer = public_key(&key(*writer));
                match self.sent.get(&writer).and_then(|sent| sent.last()).cloned() {
                    Some((submission, signature)) => self.submit(submission, signature),
                    None => {
                        self.note("resend: nothing to resend".to_owned());
                        Ok(())
                    }
                }
            }
            Op::ReplayOld { writer, back } => {
                let writer = public_key(&key(*writer));
                let old = self.sent.get(&writer).and_then(|sent| {
                    sent.len()
                        .checked_sub(2 + usize::from(*back))
                        .map(|index| sent[index].clone())
                });
                match old {
                    Some((submission, signature)) => self.submit(submission, signature),
                    None => {
                        self.note("replay: nothing old enough".to_owned());
                        Ok(())
                    }
                }
            }
            Op::Tick { micros } => {
                self.clock.advance(u64::from(*micros));
                self.note(format!("clock +{micros}"));
                Ok(())
            }
            Op::ClockBack { micros } => {
                self.clock.back(u64::from(*micros));
                self.hit("clock-back");
                self.note(format!("clock -{micros}"));
                Ok(())
            }
            Op::FailSync { keep } => {
                self.storage.fail_next_sync();
                self.crash_keep = usize::from(*keep);
                self.note(format!("armed: next sync fails, crash keeps {keep}"));
                Ok(())
            }
            Op::TearAppend { at, keep } => {
                self.storage.tear_next_append(usize::from(*at));
                self.crash_keep = usize::from(*keep);
                self.note(format!(
                    "armed: next append tears at {at}, crash keeps {keep}"
                ));
                Ok(())
            }
            Op::Crash => {
                self.hit("crash");
                self.crash(None)
            }
        }
    }

    /// Turn a described write into a submission against the current state.
    fn build(&self, write: &Write) -> (Submission, Signature) {
        let writer_key = key(write.writer);
        let writer = public_key(&writer_key);
        let next = self.model.counter_of(&writer) + 1;
        let counter = next.saturating_add_signed(i64::from(write.counter));

        let body = match &write.what {
            What::Verse {
                evidence,
                len,
                refs,
            } => Body::Verse(Verse {
                evidence: *evidence,
                text: format!("{}:{}", write.salt, "v".repeat(usize::from(*len))),
                refs: (0..*refs % 3).map(|i| [i; 32]).collect(),
            }),
            What::Preference => Body::Preference(Preference {
                key: "key".to_owned(),
                value: format!("{}", write.salt),
            }),
            What::Policy { invariants, max } => {
                let all = [
                    Invariant::VerseNamesEvidence,
                    Invariant::VerseTextMax {
                        bytes: u32::from(*max),
                    },
                    Invariant::ArtefactHasRef,
                ];
                Body::Policy(Policy {
                    evaluator: format!("sim-{}", write.salt),
                    invariants: (0..3)
                        .filter(|bit| invariants & (1 << bit) != 0)
                        .map(|bit| all[bit])
                        .collect(),
                })
            }
            What::Grant {
                holder,
                scope,
                rights,
                narrows,
            } => {
                let kinds = [
                    Kind::Policy,
                    Kind::Grant,
                    Kind::Revoke,
                    Kind::Verse,
                    Kind::Preference,
                    Kind::Verdict,
                ];
                Body::Grant(Grant {
                    holder: public_key(&key(*holder)),
                    scope: (0u8..6)
                        .filter(|bit| scope & (1 << bit) != 0)
                        .map(stream)
                        .collect(),
                    rights: (0..6)
                        .filter(|bit| rights & (1 << bit) != 0)
                        .map(|bit| kinds[bit])
                        .collect(),
                    narrows: narrows.map(u64::from),
                })
            }
            What::Revoke { grant } => Body::Revoke(Revoke {
                grant: u64::from(*grant),
            }),
            What::Genesis { root } => Body::Genesis(Genesis {
                root: public_key(&key(*root)),
            }),
            What::Bootstrap { appenders } => {
                let appender = |holder| Grant {
                    holder,
                    scope: BTreeSet::from([StreamId::from(StreamId::GATE)]),
                    rights: BTreeSet::from([Kind::Verdict]),
                    narrows: None,
                };
                let mine = public_key(&appender_key());
                let mut grants = vec![Grant {
                    holder: pin(),
                    scope: [StreamId::AUTHORITY, StreamId::POLICY]
                        .map(StreamId::from)
                        .into(),
                    rights: [Kind::Policy, Kind::Grant, Kind::Revoke].into(),
                    narrows: None,
                }];
                match appenders {
                    Appenders::Right => grants.push(appender(mine)),
                    Appenders::None => {}
                    Appenders::Two => grants.extend([appender(mine), appender(other_appender())]),
                    Appenders::Root => grants.push(appender(pin())),
                    Appenders::Other => grants.push(appender(other_appender())),
                }
                Body::Bootstrap(Bootstrap { grants })
            }
            What::Verdict => {
                let rejected = Submission {
                    stream: stream(0),
                    writer: public_key(&key(0)),
                    counter: 1,
                    body: Body::Verse(Verse::new(Evidence::Decision, "refused", vec![])),
                };
                Body::Verdict(Verdict {
                    sig_writer: sign_hash(&key(0), &rejected.hash()),
                    rejected: Box::new(rejected),
                    reason: Refusal::Invariant(Invariant::VerseNamesEvidence),
                })
            }
        };

        let submission = Submission {
            stream: stream(write.stream),
            writer,
            counter,
            body,
        };
        let hash = submission.hash();
        let signature = match write.sign {
            Sign::Good => sign_hash(&writer_key, &hash),
            Sign::Flipped { bit } => {
                let mut signature = sign_hash(&writer_key, &hash);
                signature[usize::from(bit % 512) / 8] ^= 1 << (bit % 8);
                signature
            }
            Sign::By { writer } => sign_hash(&key(writer), &hash),
        };
        (submission, signature)
    }

    /// Send one submission through the appender and the model, and compare.
    fn submit(&mut self, submission: Submission, signature: Signature) -> Check {
        let expected = self.model.judge(&submission, &signature);
        let before = self.appender.chain().clone();
        let len = self.storage.len();

        let result = self.appender.submit(submission.clone(), signature);
        let fired = self.storage.take_fired();
        self.note(format!(
            "submit {:?} counter {} on {:?} by {} -> {result:?}",
            submission.kind(),
            submission.counter,
            String::from_utf8_lossy(submission.stream.as_bytes()),
            short(&submission.writer),
        ));

        // A storage fault: the answer is "unknown", and only a crash and a
        // reopen can say which way it went. It must be reported as one.
        if fired {
            if !matches!(result, Err(Rejected::Storage(_))) {
                return Err(self.fail(
                    "4-durability",
                    format!("storage failed but the appender answered {result:?}"),
                ));
            }
            if !matches!(expected, Outcome::Accepted { .. }) {
                return Err(self.fail(
                    "3-model",
                    format!("the appender wrote a submission the model answers {expected:?}"),
                ));
            }
            if self.appender.chain() != &before {
                return Err(self.fail(
                    "4-durability",
                    "the appender's state moved for a write that was not durable",
                ));
            }
            return self.crash(Some((submission, signature)));
        }

        let actual = match self.classify(&result, before.next_seq()) {
            Ok(actual) => actual,
            Err(message) => return Err(self.fail("3-model", message)),
        };
        if actual != expected {
            return Err(self.fail(
                "3-model",
                format!("the appender answered {actual:?}, the model {expected:?}"),
            ));
        }

        match (actual, result) {
            (Outcome::Accepted { seq }, Ok(ack)) => {
                self.hit("accepted");
                if self.storage.synced_len() != self.storage.len() {
                    return Err(self.fail("4-durability", "acknowledged before it was synced"));
                }
                self.model.submit(&submission, &signature);
                self.acked.insert(seq, ack.hash);
                self.sent
                    .entry(submission.writer)
                    .or_default()
                    .push((submission.clone(), signature));

                // Oracle 7, probed after every accepted write: the same
                // submission again changes nothing and gets the same answer.
                let after = self.appender.chain().clone();
                let len = self.storage.len();
                let again = self.appender.submit(submission, signature);
                if again != Ok(ack) || self.storage.len() != len || self.appender.chain() != &after
                {
                    return Err(self.fail(
                        "7-retry",
                        format!("a resend right after the ack answered {again:?}"),
                    ));
                }
            }
            (Outcome::Retry { seq }, Ok(ack)) => {
                self.hit("retry");
                let first = self.acked.get(&seq).copied();
                if first != Some(ack.hash)
                    || self.storage.len() != len
                    || self.appender.chain() != &before
                {
                    return Err(self.fail(
                        "7-retry",
                        format!("a resend of entry {seq} answered {ack:?}; first answer {first:?}"),
                    ));
                }
            }
            (Outcome::Refused(why), _) => {
                self.hit(format!("refused:{why:?}"));
                if self.storage.len() != len || self.appender.chain() != &before {
                    return Err(self.fail("3-model", "a refused submission left a trace"));
                }
            }
            (outcome, result) => {
                return Err(self.fail("3-model", format!("{outcome:?} with result {result:?}")));
            }
        }
        Ok(())
    }

    /// The appender's answer in the model's terms.
    fn classify(&self, result: &Result<Ack, Rejected>, next_seq: u64) -> Result<Outcome, String> {
        let why = match result {
            Ok(ack) if ack.seq == next_seq => return Ok(Outcome::Accepted { seq: ack.seq }),
            Ok(ack) => return Ok(Outcome::Retry { seq: ack.seq }),
            Err(Rejected::BadSignature) => Why::BadSignature,
            Err(Rejected::CounterOld { .. }) => Why::CounterOld,
            Err(Rejected::CounterGap { .. }) => Why::CounterGap,
            Err(Rejected::CounterConflict { .. }) => Why::CounterConflict,
            Err(Rejected::Invalid(fault)) => match fault {
                Fault::NotGenesis => Why::NotGenesis,
                Fault::RootNotPinned => Why::RootNotPinned,
                Fault::NotBootstrap => Why::NotBootstrap,
                Fault::BootstrapAppender => Why::BootstrapAppender,
                // The appender's seal does not verify against the key the
                // bootstrap names: it names another appender.
                Fault::AppenderSignature => Why::WrongAppender,
                Fault::Misplaced { .. } => Why::Misplaced,
                Fault::VerdictWithoutGate => Why::Verdict,
                Fault::Stream { .. } => Why::Stream,
                // Position, hash, counter and policy are the appender's own
                // to get right; a fault there is its bug.
                other => return Err(format!("the appender built an invalid record: {other}")),
            },
            Err(other) => return Err(format!("unexpected answer: {other}")),
        };
        Ok(Outcome::Refused(why))
    }

    /// Crash and reopen. `in_flight` is the submission whose write failed,
    /// if that is why: it may have landed, or not.
    fn crash(&mut self, in_flight: Option<(Submission, Signature)>) -> Check {
        let lost = self.storage.crash(self.crash_keep);
        self.storage.disarm();
        self.crash_keep = 0;

        let opened = Appender::open(
            self.storage.clone(),
            self.clock.clone(),
            appender_key(),
            pin(),
        );
        let (appender, recovery) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                return Err(self.fail(
                    "4-durability",
                    format!("the log does not open after a crash: {error}"),
                ));
            }
        };
        self.appender = appender;
        if recovery.dropped > 0 {
            self.hit("torn-tail-dropped");
        }
        self.note(format!(
            "crash: lost {lost} unsynced bytes; reopened with {} records, dropped {} torn bytes",
            recovery.records, recovery.dropped
        ));

        // Everything is verified again from nothing.
        self.verified = Verified::default();
        self.shadow = Chain::new(pin());

        // The log holds what the model holds, or that plus the one
        // submission that was in flight.
        let stored = self.appender.chain().next_seq();
        let modelled = self.model.log().len() as u64;
        match in_flight {
            Some((submission, signature)) if stored == modelled + 1 => {
                let landed = self.model.submit(&submission, &signature);
                if landed != (Outcome::Accepted { seq: modelled }) {
                    return Err(self.fail(
                        "4-durability",
                        format!("an entry landed that the model answers {landed:?}"),
                    ));
                }
                // Durable now, though its writer never heard so.
                let hash = self.appender.chain().head().map(|head| head.hash);
                self.acked.insert(modelled, hash.unwrap_or_default());
                self.sent
                    .entry(submission.writer)
                    .or_default()
                    .push((submission, signature));
                self.hit("fault:landed");
            }
            Some(_) if stored == modelled => self.hit("fault:lost"),
            None if stored == modelled => {}
            _ => {
                return Err(self.fail(
                    "4-durability",
                    format!("after a crash the log has {stored} entries, the model {modelled}"),
                ));
            }
        }
        Ok(())
    }

    // ── Oracles ─────────────────────────────────────────────────────

    fn check(&mut self) -> Check {
        let bytes = self.storage.bytes();

        // 4: after a finished operation nothing is left unsynced.
        if self.storage.synced_len() != bytes.len() {
            return Err(self.fail("4-durability", "bytes are left unsynced after an operation"));
        }

        // 1: the stored chain verifies. What was verified before must
        // still be there, byte for byte.
        if !bytes.starts_with(&self.verified.bytes) {
            return Err(self.fail("1-chain", "stored bytes changed after they were verified"));
        }
        let scan = match frame::scan(&bytes) {
            Ok(scan) => scan,
            Err(error) => return Err(self.fail("1-chain", error.to_string())),
        };
        if scan.torn != 0 {
            return Err(self.fail(
                "1-chain",
                format!("{} bytes of an unfinished record are stored", scan.torn),
            ));
        }
        let appender = public_key(&appender_key());
        let mut added = Vec::new();
        for (seq, raw) in scan
            .records
            .iter()
            .enumerate()
            .skip(self.verified.hashes.len())
        {
            let record = match Record::decode(raw) {
                Ok(record) => record,
                Err(error) => {
                    return Err(self.fail("1-chain", format!("entry {seq}: {error}")));
                }
            };
            let prev = self.verified.hashes.last().copied().unwrap_or_default();
            let problem = if record.entry.seq != seq as u64 {
                Some(format!("seq is {}", record.entry.seq))
            } else if record.entry.prev != prev {
                Some("prev is not the hash of the entry before".to_owned())
            } else if record.entry.at < self.verified.last_at {
                Some("at went back".to_owned())
            } else {
                record
                    .verify(&appender)
                    .err()
                    .map(|error| error.to_string())
            };
            if let Some(problem) = problem {
                return Err(self.fail("1-chain", format!("entry {seq}: {problem}")));
            }
            self.verified.hashes.push(record.hash);
            self.verified.last_at = record.entry.at;
            added.push(record);
        }
        self.verified.bytes = bytes;

        // 2: replaying the stored records gives the appender's own state.
        for record in &added {
            if let Err(error) = self.shadow.apply(record) {
                return Err(self.fail("2-replay", format!("replay refuses {error}")));
            }
        }
        if &self.shadow != self.appender.chain() {
            return Err(self.fail(
                "2-replay",
                format!(
                    "replay arrives at state {}, the appender judges from {}",
                    short(&self.shadow.state_hash()),
                    short(&self.appender.chain().state_hash())
                ),
            ));
        }

        // 3: the model's log is the stored log, and its state the state.
        let chain = self.appender.chain();
        let state = chain.state();
        if chain.next_seq() != self.model.log().len() as u64 {
            return Err(self.fail(
                "3-model",
                format!(
                    "the log has {} entries, the model {}",
                    chain.next_seq(),
                    self.model.log().len()
                ),
            ));
        }
        for record in &added {
            let seq = record.entry.seq as usize;
            if self.model.log().get(seq) != Some(&record.entry.submission) {
                return Err(self.fail("3-model", format!("entry {seq} is not the model's")));
            }
        }
        for (writer, last) in &state.writers {
            if last.counter != self.model.counter_of(writer) {
                return Err(self.fail(
                    "3-model",
                    format!("writer {} is at counter {}", short(writer), last.counter),
                ));
            }
        }
        let writers: BTreeSet<PublicKey> = self.model.log().iter().map(|s| s.writer).collect();
        if writers.len() != state.writers.len() {
            return Err(self.fail("3-model", "the state knows other writers than the model"));
        }
        let streams: BTreeSet<&StreamId> = self.model.log().iter().map(|s| &s.stream).collect();
        if streams.len() != state.streams.len() {
            return Err(self.fail("3-model", "the state knows other streams than the model"));
        }
        for (id, stream) in &state.streams {
            if self.model.stream(id) != Some((stream.entries, stream.last)) {
                return Err(self.fail(
                    "3-model",
                    format!(
                        "stream {:?} has {} entries, newest {}",
                        String::from_utf8_lossy(id.as_bytes()),
                        stream.entries,
                        stream.last
                    ),
                ));
            }
        }
        let policy = self.model.policy();
        if chain.policy_seq() != policy.map_or(0, |(seq, _)| seq)
            || policy.is_some_and(|(_, body)| body != &state.policy)
            || (policy.is_none() && state.policy != Policy::default())
        {
            return Err(self.fail("3-model", "the policy in force is not the model's"));
        }

        // 4: every acknowledged entry is there, unchanged.
        for (seq, hash) in &self.acked {
            if self.verified.hashes.get(*seq as usize) != Some(hash) {
                return Err(self.fail(
                    "4-durability",
                    format!("acknowledged entry {seq} is missing or changed"),
                ));
            }
        }

        let head = chain
            .head()
            .map_or("none".to_owned(), |head| short(&head.hash));
        self.note(format!(
            "    {} entries, head {head}, state {}, {} bytes, clock {}",
            chain.next_seq(),
            short(&chain.state_hash()),
            self.verified.bytes.len(),
            island_env::Clock::now_micros(&self.clock),
        ));
        Ok(())
    }
}
