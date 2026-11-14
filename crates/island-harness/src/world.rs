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
//!    from, and replay judges every verdict's refused submission again and
//!    requires the same refusal.
//! 3. The model agrees on every accept, retry and refusal, on every gate
//!    refusal and its reason, on every verdict entry byte for byte, and on
//!    the state.
//! 4. No acknowledged entry is ever missing or changed; nothing is
//!    acknowledged before it is durable; after a crash the log holds what
//!    the model holds, plus at most the one submission that was in flight.
//! 5. Nothing outside a writer's grants was ever accepted: for every
//!    stored entry, a grant issued earlier and not ended earlier covers
//!    its writer, stream and kind; the same for the submission a verdict
//!    holds. Worked out here from the stored entries, without
//!    `island-auth` and without the model.
//! 6. Stronger means subset: of the policies that were in force in a case,
//!    each strengthens every one before it, and no write the case
//!    generated is accepted by a later one and refused by an earlier one.
//!    (`ratchet.rs` checks the same on generated pairs of any policies.)
//! 7. An exact resend returns the first answer and adds nothing: the
//!    acknowledgement, or the refusal.
//! 8. Every action let out was approved, once: each stored `dispatch`
//!    names an earlier `approve` of its request that no earlier dispatch
//!    used and no decline after it overrides, and no Vessel's action key
//!    is dispatched twice. Worked out here from the stored entries,
//!    without the gate, the state or the model.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use island_core::{
    Answered, Approval, Body, Bootstrap, Dispatch, Evidence, Genesis, Grant, GrantId, Hash,
    Invariant, Kind, Policy, Preference, PublicKey, Record, Refusal, Revoke, Role, Signature,
    SigningKey, StreamId, Submission, Verdict, Verse, VesselId, public_key, sign_hash,
};
use island_log::{Ack, Appender, Chain, Denied, Fault, Rejected, frame};
use island_model::{Guard, Model, Outcome, Why};

use crate::ops::{Appenders, KEYS, Op, STREAMS, Sign, Twist, What, Write};
use crate::sim::{SimClock, SimStorage};

/// An oracle that does not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Which oracle: `1-chain`, `2-replay`, `3-model`, `4-durability`,
    /// `5-grants`, `6-ratchet`, `7-retry`, `8-dispatch` or `determinism`.
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
    match writer % KEYS {
        3 => SigningKey::from_bytes(&[0x52; 32]),
        4 => appender_key(),
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

/// The kinds a rights mask can name, in the order of `ops::RIGHTS`.
const KINDS: [Kind; 8] = [
    Kind::Verse,
    Kind::Preference,
    Kind::Policy,
    Kind::Grant,
    Kind::Revoke,
    Kind::Verdict,
    Kind::Approval,
    Kind::Dispatch,
];

/// The three request hashes approvals and dispatches name.
fn request(index: u8) -> Hash {
    [0xA0 + index % 3; 32]
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
    /// Oracle 5's own reading of the stored entries: every grant issued,
    /// and whether a later entry ended it.
    issued: Vec<(GrantId, Grant, bool)>,
    /// Oracle 6's reading of them: every policy that has been in force,
    /// oldest first, starting with the empty one before any entry.
    policies: Vec<Policy>,
    /// How many policies and how many writes oracle 6 has already set
    /// against each other.
    ratcheted: (usize, usize),
    /// Oracle 8's reading: every approval and decline by position, the
    /// approvals a dispatch used, and every action dispatched.
    answers: BTreeMap<u64, (Hash, Answered)>,
    used: BTreeSet<u64>,
    dispatched: BTreeSet<(VesselId, Hash)>,
}

impl Verified {
    fn new() -> Self {
        Self {
            policies: vec![Policy::default()],
            ..Self::default()
        }
    }
}

struct World {
    storage: SimStorage,
    clock: SimClock,
    appender: Appender<SimStorage, SimClock>,
    model: Model,
    /// Per writer: the submissions that are in the log, with signatures:
    /// as entries, or held in the verdicts that refused them.
    sent: BTreeMap<PublicKey, Vec<(Submission, Signature)>>,
    /// Every entry a writer was answered with: position to hash.
    acked: BTreeMap<u64, Hash>,
    /// Every verse and policy the case has generated, whatever became of
    /// it: the writes oracle 6 judges under each policy.
    writes: Vec<Submission>,
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
        writes: Vec::new(),
        verified: Verified::new(),
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
    world.verified = Verified::new();
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
            What::Tighten { add, lower_by } => {
                // The policy in force, with its text bound lowered and the
                // missing invariants of `add` appended.
                let lower = u32::from(*lower_by);
                let mut invariants: Vec<Invariant> = self
                    .model
                    .policy()
                    .map(|(_, policy)| policy.invariants.clone())
                    .unwrap_or_default();
                for invariant in &mut invariants {
                    if let Invariant::VerseTextMax { bytes } = invariant {
                        *bytes = bytes.saturating_sub(lower);
                    }
                }
                let more = [
                    Invariant::VerseNamesEvidence,
                    Invariant::VerseTextMax { bytes: 8 + lower },
                    Invariant::ArtefactHasRef,
                ];
                for (bit, invariant) in more.into_iter().enumerate() {
                    let held = invariants.iter().any(|held| held.tag() == invariant.tag());
                    if add & (1 << bit) != 0 && !held {
                        invariants.push(invariant);
                    }
                }
                Body::Policy(Policy {
                    evaluator: format!("sim-{}", write.salt),
                    invariants,
                })
            }
            What::Grant {
                holder,
                scope,
                rights,
            } => Body::Grant(Grant {
                holder: public_key(&key(*holder)),
                scope: (0u8..7)
                    .filter(|bit| scope & (1 << bit) != 0)
                    .map(stream)
                    .collect(),
                rights: (0..8)
                    .filter(|bit| rights & (1 << bit) != 0)
                    .map(|bit| KINDS[bit])
                    .collect(),
                narrows: None,
            }),
            What::Narrow {
                target,
                keep_scope,
                keep_rights,
                twist,
            } => {
                // The target with some of its streams and kinds dropped: a
                // valid narrowing, unless the twist says otherwise.
                let (id, old) = self.target(*target);
                let kept = |mask: u8, index: usize| index >= 8 || mask & (1 << index) != 0;
                let mut grant = Grant {
                    holder: old.holder,
                    scope: old
                        .scope
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| kept(*keep_scope, *index))
                        .map(|(_, stream)| stream.clone())
                        .collect(),
                    rights: old
                        .rights
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| kept(*keep_rights, *index))
                        .map(|(_, kind)| *kind)
                        .collect(),
                    narrows: Some(id),
                };
                match twist {
                    Twist::None => {}
                    Twist::OtherHolder { writer } => grant.holder = public_key(&key(*writer)),
                    Twist::AddStream { stream: index } => {
                        grant.scope.insert(stream(*index));
                    }
                    Twist::AddRight { right } => {
                        grant
                            .rights
                            .insert(KINDS[usize::from(*right) % KINDS.len()]);
                    }
                }
                Body::Grant(grant)
            }
            What::Revoke { target } => Body::Revoke(Revoke {
                grant: self.target(*target).0,
            }),
            What::Genesis { root } => Body::Genesis(Genesis {
                root: public_key(&key(*root)),
            }),
            What::Bootstrap { appenders, roots } => {
                let appender = |holder| Grant {
                    holder,
                    scope: BTreeSet::from([StreamId::from(StreamId::GATE)]),
                    rights: BTreeSet::from([Kind::Verdict]),
                    narrows: None,
                };
                let mine = public_key(&appender_key());
                let root_capability = Grant {
                    holder: pin(),
                    scope: [StreamId::AUTHORITY, StreamId::POLICY]
                        .map(StreamId::from)
                        .into(),
                    rights: [Kind::Policy, Kind::Grant, Kind::Revoke].into(),
                    narrows: None,
                };
                let mut grants = vec![root_capability; usize::from(*roots % 3)];
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
            What::Approve {
                request: index,
                decline,
            } => Body::Approval(Approval {
                request: request(*index),
                verdict: if *decline {
                    Answered::Decline
                } else {
                    Answered::Approve
                },
                proof: None,
            }),
            What::Dispatch {
                approval,
                request: which,
                key,
                vessel,
            } => {
                // The approvals and declines in the log so far, by
                // position; one past the end is a position holding none.
                // 0 to 3 pick among the approvals, 4 among approvals and
                // declines alike, 5 names a position holding neither. Few
                // to pick from, so that dispatches meet on one.
                let answers: Vec<(u64, Hash)> = (0u64..)
                    .zip(self.model.log())
                    .filter_map(|(seq, entry)| match &entry.body {
                        Body::Approval(given)
                            if *approval == 4 || given.verdict == Answered::Approve =>
                        {
                            Some((seq, given.request))
                        }
                        _ => None,
                    })
                    .collect();
                let named = match (*approval, answers.len()) {
                    (5, _) | (_, 0) => None,
                    (index, len) => answers.get(usize::from(index) % len),
                };
                let (seq, answered) = named.copied().unwrap_or((1, request(0)));
                Body::Dispatch(Dispatch {
                    vessel: [*vessel % 2; 16],
                    op: u64::from(write.salt),
                    key: [*key % 3; 32],
                    request: if *which == 0 {
                        answered
                    } else {
                        request(*which - 1)
                    },
                    approval: seq,
                    counter: 0,
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

    /// The grant a narrowing or a revoke aims at: an index into every
    /// grant issued so far, or, one past the end, a grant that does not
    /// exist.
    fn target(&self, index: u8) -> (GrantId, Grant) {
        let leases = self.model.grants();
        match leases.get(usize::from(index) % (leases.len() + 1)) {
            Some(lease) => (lease.id, lease.grant.clone()),
            None => (
                GrantId::new(9_999, 0),
                Grant {
                    holder: public_key(&key(0)),
                    scope: BTreeSet::new(),
                    rights: BTreeSet::new(),
                    narrows: None,
                },
            ),
        }
    }

    /// Send one submission through the appender and the model, and compare.
    fn submit(&mut self, submission: Submission, signature: Signature) -> Check {
        let expected = self.model.judge(&submission, &signature);
        let before = self.appender.chain().clone();
        let len = self.storage.len();
        if matches!(submission.body, Body::Verse(_) | Body::Policy(_)) {
            self.writes.push(submission.clone());
        }

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
            if !matches!(expected, Outcome::Accepted { .. } | Outcome::Verdict { .. }) {
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
                match &submission.body {
                    Body::Grant(grant) if grant.narrows.is_some() => self.hit("grant:narrowed"),
                    Body::Grant(_) => self.hit("grant:new"),
                    Body::Revoke(_) => self.hit("grant:revoked"),
                    Body::Policy(policy) => match self.model.policy() {
                        None => self.hit("policy:first"),
                        Some((_, old)) if old.invariants == policy.invariants => {
                            self.hit("policy:restated");
                        }
                        Some(_) => self.hit("policy:tightened"),
                    },
                    Body::Verse(_) if self.model.policy().is_some() => {
                        self.hit("verse:passed-the-gate");
                    }
                    Body::Approval(approval) if approval.verdict == Answered::Decline => {
                        self.hit("approval:declined");
                    }
                    Body::Approval(_) => self.hit("approval:approved"),
                    Body::Dispatch(_) => self.hit("dispatch:let-out"),
                    _ => {}
                }
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
            // The gate refused it: a verdict took the position, and the
            // writer's counter is used.
            (Outcome::Verdict { seq, reason }, Err(refused)) => {
                self.hit(match reason {
                    Refusal::Invariant(Invariant::VerseNamesEvidence) => {
                        "verdict:VerseNamesEvidence"
                    }
                    Refusal::Invariant(Invariant::VerseTextMax { .. }) => "verdict:VerseTextMax",
                    Refusal::Invariant(Invariant::ArtefactHasRef) => "verdict:ArtefactHasRef",
                    Refusal::WeakerPolicy => "verdict:WeakerPolicy",
                    Refusal::Unapproved => "verdict:Unapproved",
                    Refusal::ApprovalUsed => "verdict:ApprovalUsed",
                    Refusal::Declined => "verdict:Declined",
                    Refusal::AlreadyDispatched => "verdict:AlreadyDispatched",
                });
                let Rejected::Refused { hash, .. } = refused else {
                    return Err(self.fail("3-model", format!("{refused:?} for a gate refusal")));
                };
                if self.storage.synced_len() != self.storage.len() {
                    return Err(self.fail("4-durability", "refused before the verdict was synced"));
                }
                self.model.submit(&submission, &signature);
                self.acked.insert(seq, hash);
                self.sent
                    .entry(submission.writer)
                    .or_default()
                    .push((submission.clone(), signature));

                // Oracle 7, for refusals: the same submission again gets
                // the same refusal and adds nothing.
                let after = self.appender.chain().clone();
                let len = self.storage.len();
                let again = self.appender.submit(submission, signature);
                if again != Err(refused.clone())
                    || self.storage.len() != len
                    || self.appender.chain() != &after
                {
                    return Err(self.fail(
                        "7-retry",
                        format!("a resend right after the refusal answered {again:?}"),
                    ));
                }
            }
            (Outcome::VerdictRetry { seq, .. }, Err(Rejected::Refused { hash, .. })) => {
                self.hit("verdict-retry");
                let first = self.acked.get(&seq).copied();
                if first != Some(hash)
                    || self.storage.len() != len
                    || self.appender.chain() != &before
                {
                    return Err(self.fail(
                        "7-retry",
                        format!("a resend refused at entry {seq} was answered with verdict {}; first answer {first:?}", short(&hash)),
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
            Err(Rejected::Refused { seq, reason, .. }) => {
                let (seq, reason) = (*seq, *reason);
                return Ok(if seq == next_seq {
                    Outcome::Verdict { seq, reason }
                } else {
                    Outcome::VerdictRetry { seq, reason }
                });
            }
            Err(Rejected::VerdictFromOutside) => Why::Verdict,
            Err(Rejected::Denied(denied)) => match denied {
                Denied::NoGrant => Why::NoGrant,
                Denied::UnknownGrant(_) => Why::UnknownGrant,
                Denied::Ended(_) => Why::EndedGrant,
                Denied::OtherHolder => Why::OtherHolder,
                Denied::Wider => Why::Wider,
                Denied::Ungrantable(_) => Why::Ungrantable,
                Denied::Guarded(_) => Why::Guarded,
            },
            Err(Rejected::Invalid(fault)) => match fault {
                Fault::NotGenesis => Why::NotGenesis,
                Fault::RootNotPinned => Why::RootNotPinned,
                Fault::NotBootstrap => Why::NotBootstrap,
                Fault::BootstrapAppender => Why::BootstrapAppender,
                Fault::BootstrapRoot => Why::BootstrapRoot,
                // The appender's seal does not verify against the key the
                // bootstrap names: it names another appender.
                Fault::AppenderSignature => Why::WrongAppender,
                Fault::Misplaced { .. } => Why::Misplaced,
                Fault::AppenderWrites => Why::AppenderWrites,
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
        self.verified = Verified::new();
        self.shadow = Chain::new(pin());

        // The log holds what the model holds, or that plus the one
        // submission that was in flight: as an entry, or in its verdict.
        let stored = self.appender.chain().next_seq();
        let modelled = self.model.log().len() as u64;
        match in_flight {
            Some((submission, signature)) if stored == modelled + 1 => {
                match self.model.submit(&submission, &signature) {
                    Outcome::Accepted { seq } if seq == modelled => self.hit("fault:landed"),
                    Outcome::Verdict { seq, .. } if seq == modelled => {
                        self.hit("fault:verdict-landed");
                    }
                    landed => {
                        return Err(self.fail(
                            "4-durability",
                            format!("an entry landed that the model answers {landed:?}"),
                        ));
                    }
                }
                // Durable now, though its writer never heard so.
                let hash = self.appender.chain().head().map(|head| head.hash);
                self.acked.insert(modelled, hash.unwrap_or_default());
                self.sent
                    .entry(submission.writer)
                    .or_default()
                    .push((submission, signature));
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
        // A writer is known for its entries, or for a submission a verdict
        // holds.
        let writers: BTreeSet<PublicKey> = self
            .model
            .log()
            .iter()
            .flat_map(|entry| match &entry.body {
                Body::Verdict(verdict) => vec![entry.writer, verdict.rejected.writer],
                _ => vec![entry.writer],
            })
            .collect();
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

        // 3, for grants: the same grants, each in force or not, each
        // protected or not.
        let leases = self.model.grants();
        if leases.len() != state.grants.len() {
            return Err(self.fail("3-model", "the state holds other grants than the model"));
        }
        for lease in &leases {
            let role = match lease.guard {
                Guard::None => Role::Ordinary,
                Guard::Root => Role::Root,
                Guard::Appender => Role::Appender,
            };
            let same = state.grants.get(&lease.id).is_some_and(|held| {
                held.grant == lease.grant && held.in_force() == lease.live && held.role == role
            });
            if !same {
                return Err(self.fail("3-model", format!("grant {} is not the model's", lease.id)));
            }
        }

        // 5: every stored entry was covered by a grant issued before it and
        // not ended before it.
        for record in &added {
            let seq = record.entry.seq;
            let submission = &record.entry.submission;
            // The entry's own writer, and for a verdict also the writer of
            // the submission it refused: the gate judges only what
            // authority let in.
            let mut held = vec![submission];
            if let Body::Verdict(verdict) = &submission.body {
                held.push(&verdict.rejected);
            }
            for submission in held {
                let covered = self.verified.issued.iter().any(|(_, grant, live)| {
                    *live
                        && grant.holder == submission.writer
                        && grant.scope.contains(&submission.stream)
                        && grant.rights.contains(&submission.kind())
                });
                if seq >= 2 && !covered {
                    return Err(self.fail(
                        "5-grants",
                        format!(
                            "entry {seq}: no grant in force lets {} write {:?} to {:?}",
                            short(&submission.writer),
                            submission.kind(),
                            String::from_utf8_lossy(submission.stream.as_bytes())
                        ),
                    ));
                }
            }
            let end = |issued: &mut Vec<(GrantId, Grant, bool)>, id: GrantId| {
                if let Some(grant) = issued.iter_mut().find(|(issued, _, _)| *issued == id) {
                    grant.2 = false;
                }
            };
            match &submission.body {
                Body::Bootstrap(bootstrap) => {
                    for (index, grant) in (0u32..).zip(&bootstrap.grants) {
                        let id = GrantId::new(seq, index);
                        self.verified.issued.push((id, grant.clone(), true));
                    }
                }
                Body::Grant(grant) => {
                    if let Some(old) = grant.narrows {
                        end(&mut self.verified.issued, old);
                    }
                    let id = GrantId::new(seq, 0);
                    self.verified.issued.push((id, grant.clone(), true));
                }
                Body::Revoke(revoke) => end(&mut self.verified.issued, revoke.grant),
                Body::Policy(policy) => self.verified.policies.push(policy.clone()),
                _ => {}
            }
        }

        // 6: stronger means subset. Every policy that came into force
        // strengthens every one before it, and nothing this case generated
        // is accepted by a later policy and refused by an earlier one.
        let accepts =
            |policy: &Policy, write: &Submission| island_gate::judge(state, policy, write).is_ok();
        let (policies, writes) = self.verified.ratcheted;
        for (later, new) in self.verified.policies.iter().enumerate() {
            let before = &self.verified.policies[..later];
            if later >= policies
                && let Some(earlier) = before
                    .iter()
                    .position(|old| !island_gate::strengthens(old, new))
            {
                return Err(self.fail(
                    "6-ratchet",
                    format!("policy {later} of this log came into force without strengthening policy {earlier}: {new:?}"),
                ));
            }
            for (index, write) in self.writes.iter().enumerate() {
                if (later < policies && index < writes) || !accepts(new, write) {
                    continue;
                }
                if let Some(earlier) = before.iter().position(|old| !accepts(old, write)) {
                    return Err(self.fail(
                        "6-ratchet",
                        format!("policy {later} of this log accepts a write that policy {earlier} refuses: {:?}", write.body),
                    ));
                }
            }
        }
        self.verified.ratcheted = (self.verified.policies.len(), self.writes.len());

        // 8: every action let out was approved, once.
        for record in &added {
            let seq = record.entry.seq;
            match &record.entry.submission.body {
                Body::Approval(approval) => {
                    self.verified
                        .answers
                        .insert(seq, (approval.request, approval.verdict));
                }
                Body::Dispatch(dispatch) => {
                    let named = self.verified.answers.get(&dispatch.approval);
                    let problem = if named != Some(&(dispatch.request, Answered::Approve)) {
                        Some("names no approval of its request")
                    } else if self.verified.used.contains(&dispatch.approval) {
                        Some("uses an approval another dispatch used")
                    } else if self
                        .verified
                        .answers
                        .iter()
                        .any(|(at, (request, verdict))| {
                            *at > dispatch.approval
                                && *request == dispatch.request
                                && *verdict == Answered::Decline
                        })
                    {
                        Some("comes after a decline of its request")
                    } else if self
                        .verified
                        .dispatched
                        .contains(&(dispatch.vessel, dispatch.key))
                    {
                        Some("lets out an action already let out")
                    } else {
                        None
                    };
                    if let Some(problem) = problem {
                        return Err(self.fail("8-dispatch", format!("entry {seq} {problem}")));
                    }
                    self.verified.used.insert(dispatch.approval);
                    self.verified
                        .dispatched
                        .insert((dispatch.vessel, dispatch.key));
                }
                _ => {}
            }
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
