//! The chain: what makes a sequence of records a valid Island log, and the
//! state it replays to.
//!
//! [`Chain::check`] is the single place that decides whether a record may
//! follow the records before it. Replay runs it over stored records; the
//! appender runs it on the record it is about to store. So what the
//! appender writes and what replay accepts cannot drift apart. Pure: no
//! I/O, no clock.
//!
//! Checked here, for every record: position (`seq`, `prev`, `at`), the
//! hash, both signatures, the genesis and bootstrap rules, the stream each
//! kind belongs to, the writer's counter, the grant that lets the writer
//! write it (`island-auth`), the gate (`island-gate`), and the `policy`
//! field.
//!
//! The gate cuts both ways. An ordinary entry must be one the gate
//! accepts under the policy in force. A `verdict` entry must carry a
//! submission that would have been an entry but for the gate, and the gate
//! must refuse it now for the reason recorded then.

use std::fmt;

use island_auth::{BootstrapError, Denied};
use island_core::{
    Answer, Answered, Body, Given, Hash, Kind, Last, PublicKey, Record, Refusal, Role, Signature,
    State, StreamId, Submission, VerifyError, state_hash, verify_hash,
};
use island_gate::judge;

/// Why a record may not follow the records before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// `seq` is not the next position.
    Seq {
        /// The next position.
        expected: u64,
        /// The record's `seq`.
        found: u64,
    },
    /// `prev` is not the hash of the entry before (zeros at genesis).
    Prev,
    /// `at` is earlier than the entry before.
    TimeWentBack,
    /// The stored hash is not the entry's hash.
    Hash,
    /// The writer's signature does not verify.
    WriterSignature,
    /// The appender's signature does not verify against the key declared
    /// in the bootstrap entry.
    AppenderSignature,
    /// Entry 0 is not a `genesis` entry signed by the key it declares.
    NotGenesis,
    /// Entry 0 declares a root key other than the pinned one.
    RootNotPinned,
    /// Entry 1 is not a `bootstrap` entry signed by the root key.
    NotBootstrap,
    /// The bootstrap entry does not name exactly one appender: one grant
    /// with the `verdict` right, held by a key other than root.
    BootstrapAppender,
    /// The bootstrap entry does not hold exactly one root capability: one
    /// grant held by root with the right to write grants and revokes on
    /// the authority stream.
    BootstrapRoot,
    /// Authority does not allow the write.
    Denied(Denied),
    /// A `genesis` or `bootstrap` after entries 0 and 1, or a verdict
    /// where only an ordinary submission can stand.
    Misplaced {
        /// Its kind.
        kind: Kind,
    },
    /// The kind does not belong on this stream.
    Stream {
        /// The kind.
        kind: Kind,
        /// The stream it was sent to.
        stream: StreamId,
    },
    /// The writer's counter is not its next number.
    Counter {
        /// The writer's next number, if it has one left.
        expected: Option<u64>,
        /// The submission's counter.
        found: u64,
    },
    /// `policy` is not the `seq` of the policy entry in force.
    Policy {
        /// The policy entry in force (0 before the first).
        expected: u64,
        /// The entry's `policy`.
        found: u64,
    },
    /// The appender's key wrote something other than a verdict. That key
    /// orders and records refusals; it submits nothing else.
    AppenderWrites,
    /// The gate refuses this write under the policy in force. It belongs
    /// in a `verdict` entry, not in the log as it stands.
    Gate(Refusal),
    /// A `verdict` entry that does not hold up.
    Verdict(VerdictFault),
}

/// Why a `verdict` entry does not hold up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerdictFault {
    /// The refused submission is not signed by its writer.
    Signature,
    /// The refused submission would have been refused without an entry:
    /// its counter, its stream or its grant is wrong. A verdict records
    /// only what the gate refused.
    NoEntry(Box<Fault>),
    /// Judged again, the gate does not give the refusal recorded.
    Rejudged {
        /// The refusal in the entry.
        recorded: Refusal,
        /// What the gate says now; `None` if it accepts.
        now: Option<Refusal>,
    },
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seq { expected, found } => write!(f, "seq is {found}, expected {expected}"),
            Self::Prev => f.write_str("prev is not the hash of the entry before"),
            Self::TimeWentBack => f.write_str("at is earlier than the entry before"),
            Self::Hash => f.write_str("stored hash is not the entry's hash"),
            Self::WriterSignature => f.write_str("writer signature does not verify"),
            Self::AppenderSignature => f.write_str("appender signature does not verify"),
            Self::NotGenesis => {
                f.write_str("entry 0 is not a genesis entry signed by the root key it declares")
            }
            Self::RootNotPinned => f.write_str("the declared root key is not the pinned one"),
            Self::NotBootstrap => {
                f.write_str("entry 1 is not a bootstrap entry signed by the root key")
            }
            Self::BootstrapAppender => f.write_str(
                "the bootstrap entry must name exactly one appender: \
                 one grant with the verdict right, held by a key other than root",
            ),
            Self::BootstrapRoot => f.write_str(
                "the bootstrap entry must hold exactly one root capability: one grant held \
                 by root with the right to write grants and revokes on the authority stream",
            ),
            Self::Denied(denied) => denied.fmt(f),
            Self::Misplaced { kind } => write!(f, "a {kind:?} entry does not belong here"),
            Self::Stream { kind, stream } => write!(
                f,
                "a {kind:?} entry does not belong on stream {:?}",
                String::from_utf8_lossy(stream.as_bytes())
            ),
            Self::Counter {
                expected: Some(expected),
                found,
            } => write!(f, "writer counter is {found}, expected {expected}"),
            Self::Counter {
                expected: None,
                found,
            } => write!(
                f,
                "writer counter is {found}, but the writer has no number left"
            ),
            Self::Policy { expected, found } => {
                write!(
                    f,
                    "policy is {found}, but the policy in force is {expected}"
                )
            }
            Self::AppenderWrites => {
                f.write_str("the appender's key writes verdicts and nothing else")
            }
            Self::Gate(refusal) => write!(f, "the gate refuses it: {refusal:?}"),
            Self::Verdict(VerdictFault::Signature) => {
                f.write_str("the verdict's refused submission is not signed by its writer")
            }
            Self::Verdict(VerdictFault::NoEntry(fault)) => write!(
                f,
                "the verdict's refused submission would have left no entry: {fault}"
            ),
            Self::Verdict(VerdictFault::Rejudged { recorded, now }) => write!(
                f,
                "the verdict records {recorded:?}, but judged again the gate says {}",
                now.map_or("it is acceptable".to_owned(), |now| format!("{now:?}"))
            ),
        }
    }
}

/// A record that does not belong in the log, by position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayError {
    /// The position of the offending record.
    pub seq: u64,
    /// What is wrong with it.
    pub fault: Fault,
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "entry {}: {}", self.seq, self.fault)
    }
}

impl std::error::Error for ReplayError {}

/// The newest entry of a chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    /// Its position.
    pub seq: u64,
    /// Its hash.
    pub hash: Hash,
    /// Its time.
    pub at: u64,
}

/// A valid log so far: the state it replays to, and what the next record
/// must connect to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    pin: PublicKey,
    state: State,
    head: Option<Head>,
    policy_seq: u64,
    appender: Option<PublicKey>,
    /// Entry 0's hash and appender signature, held until entry 1 names the
    /// appender key they are checked against.
    genesis_seal: Option<(Hash, Signature)>,
}

/// What a checked record will change: the proof that it was checked.
#[derive(Debug)]
#[must_use = "a checked record changes nothing until Chain::advance"]
pub struct Step {
    head: Head,
    appender: Option<PublicKey>,
    genesis_seal: Option<(Hash, Signature)>,
}

impl Chain {
    /// An empty chain that will accept only a genesis declaring `pin`, the
    /// root public key every replica holds in its config.
    pub fn new(pin: PublicKey) -> Self {
        Self {
            pin,
            state: State::default(),
            head: None,
            policy_seq: 0,
            appender: None,
            genesis_seal: None,
        }
    }

    /// The state the records so far replay to.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// `state_hash` of that state.
    pub fn state_hash(&self) -> Hash {
        state_hash(&self.state)
    }

    /// The newest entry, if any.
    pub fn head(&self) -> Option<Head> {
        self.head
    }

    /// The position the next record takes.
    pub fn next_seq(&self) -> u64 {
        self.head.map_or(0, |head| head.seq + 1)
    }

    /// What the next record's `prev` must be.
    pub fn prev(&self) -> Hash {
        self.head.map_or([0; 32], |head| head.hash)
    }

    /// The `seq` of the policy entry in force; 0 before the first one.
    /// Entry 0 is the genesis, so 0 never names a real policy.
    pub fn policy_seq(&self) -> u64 {
        self.policy_seq
    }

    /// The pinned root key.
    pub fn root(&self) -> PublicKey {
        self.pin
    }

    /// The appender key, once the bootstrap entry has declared it.
    pub fn appender(&self) -> Option<PublicKey> {
        self.appender
    }

    /// The counter `writer`'s next submission must carry: 1, then one more
    /// than its latest. `None` only for a writer that has used every
    /// number, which cannot happen while gaps are refused.
    pub fn next_counter(&self, writer: &PublicKey) -> Option<u64> {
        match self.state.writers.get(writer) {
            None => Some(1),
            Some(last) => last.counter.checked_add(1),
        }
    }

    /// Decide whether `record` may follow the records so far. Changes
    /// nothing; hand the result to [`Chain::advance`].
    pub fn check(&self, record: &Record) -> Result<Step, Fault> {
        let entry = &record.entry;
        let submission = &entry.submission;
        let seq = self.next_seq();

        // Position.
        if entry.seq != seq {
            return Err(Fault::Seq {
                expected: seq,
                found: entry.seq,
            });
        }
        if entry.prev != self.prev() {
            return Err(Fault::Prev);
        }
        if self.head.is_some_and(|head| entry.at < head.at) {
            return Err(Fault::TimeWentBack);
        }

        // The hash, and who wrote. The appender's signature comes last: on
        // entry 1 the key it is checked against is in the entry itself.
        match record_faults(record) {
            Ok(()) => {}
            Err(VerifyError::HashMismatch) => return Err(Fault::Hash),
            Err(VerifyError::BadWriterSignature) => return Err(Fault::WriterSignature),
            Err(VerifyError::BadAppenderSignature) => return Err(Fault::AppenderSignature),
        }

        // Genesis and bootstrap: the first two entries.
        let mut appender = self.appender;
        match (seq, &submission.body) {
            (0, Body::Genesis(genesis)) => {
                if genesis.root != submission.writer {
                    return Err(Fault::NotGenesis);
                }
                if genesis.root != self.pin {
                    return Err(Fault::RootNotPinned);
                }
            }
            (0, _) => return Err(Fault::NotGenesis),
            (1, Body::Bootstrap(bootstrap)) => {
                if submission.writer != self.pin {
                    return Err(Fault::NotBootstrap);
                }
                let roles =
                    island_auth::bootstrap(&bootstrap.grants, &self.pin).map_err(|error| {
                        match error {
                            BootstrapError::Appender => Fault::BootstrapAppender,
                            BootstrapError::Root => Fault::BootstrapRoot,
                            BootstrapError::Ungrantable(kind) => {
                                Fault::Denied(Denied::Ungrantable(kind))
                            }
                        }
                    })?;
                // The appender is the holder of the one grant with that role.
                appender = bootstrap
                    .grants
                    .iter()
                    .zip(roles)
                    .find(|(_, role)| *role == Role::Appender)
                    .map(|(grant, _)| grant.holder);
            }
            (1, _) => return Err(Fault::NotBootstrap),
            _ => {}
        }

        match &submission.body {
            // The first two entries come before any grant and any policy;
            // their rules are above.
            _ if seq < 2 => self.placed(submission)?,
            // A verdict: the appender's record of a refusal. It must hold
            // the right to write it, and the refusal must hold up.
            Body::Verdict(verdict) => {
                self.placed(submission)?;
                island_auth::authorize(&self.state, submission).map_err(Fault::Denied)?;
                let rejected = &*verdict.rejected;
                rejected
                    .verify(&verdict.sig_writer)
                    .map_err(|_| Fault::Verdict(VerdictFault::Signature))?;
                self.admit(rejected)
                    .map_err(|fault| Fault::Verdict(VerdictFault::NoEntry(Box::new(fault))))?;
                let now = judge(&self.state, &self.state.policy, rejected).err();
                if now != Some(verdict.reason) {
                    return Err(Fault::Verdict(VerdictFault::Rejudged {
                        recorded: verdict.reason,
                        now,
                    }));
                }
            }
            // Anything else: authority lets it in, and the gate accepts it.
            _ => {
                self.admit(submission)?;
                judge(&self.state, &self.state.policy, submission).map_err(Fault::Gate)?;
            }
        }

        // The policy the entry says it was judged under.
        if entry.policy != self.policy_seq {
            return Err(Fault::Policy {
                expected: self.policy_seq,
                found: entry.policy,
            });
        }

        // Who ordered. Entry 0 waits for entry 1 to name the key; entry 1
        // then answers for both.
        let mut genesis_seal = self.genesis_seal;
        match appender {
            None => genesis_seal = Some((record.hash, record.sig_appender)),
            Some(appender) => {
                if let Some((hash, signature)) = genesis_seal.take() {
                    verify_hash(&appender, &hash, &signature)
                        .map_err(|_| Fault::AppenderSignature)?;
                }
                verify_hash(&appender, &record.hash, &record.sig_appender)
                    .map_err(|_| Fault::AppenderSignature)?;
            }
        }

        Ok(Step {
            head: Head {
                seq,
                hash: record.hash,
                at: entry.at,
            },
            appender,
            genesis_seal,
        })
    }

    /// The stream the kind belongs to, and the writer's counter: exactly
    /// its next number.
    fn placed(&self, submission: &Submission) -> Result<(), Fault> {
        let kind = submission.kind();
        if !belongs(kind, &submission.stream) {
            return Err(Fault::Stream {
                kind,
                stream: submission.stream.clone(),
            });
        }
        let expected = self.next_counter(&submission.writer);
        if expected != Some(submission.counter) {
            return Err(Fault::Counter {
                expected,
                found: submission.counter,
            });
        }
        Ok(())
    }

    /// Everything short of the gate that a submission must pass to become
    /// an entry after the first two: its kind, its writer, its stream, its
    /// counter and its grant. What fails here leaves no entry at all.
    fn admit(&self, submission: &Submission) -> Result<(), Fault> {
        let kind = submission.kind();
        if matches!(kind, Kind::Genesis | Kind::Bootstrap | Kind::Verdict) {
            return Err(Fault::Misplaced { kind });
        }
        if self.appender == Some(submission.writer) {
            return Err(Fault::AppenderWrites);
        }
        self.placed(submission)?;
        island_auth::authorize(&self.state, submission).map_err(Fault::Denied)
    }

    /// Take a checked record into the chain. `step` must come from
    /// [`Chain::check`] on this chain and this record.
    pub fn advance(&mut self, record: &Record, step: Step) {
        let submission = &record.entry.submission;
        let seq = step.head.seq;

        let stream = self
            .state
            .streams
            .entry(submission.stream.clone())
            .or_default();
        stream.entries += 1;
        stream.last = seq;

        self.state.writers.insert(
            submission.writer,
            Last {
                counter: submission.counter,
                sub_hash: submission.hash(),
                answer: Answer::Accepted {
                    seq,
                    hash: step.head.hash,
                },
            },
        );

        // A refused submission still used its writer's counter. The answer
        // it got is this verdict.
        if let Body::Verdict(verdict) = &submission.body {
            self.state.writers.insert(
                verdict.rejected.writer,
                Last {
                    counter: verdict.rejected.counter,
                    sub_hash: verdict.rejected.hash(),
                    answer: Answer::Refused {
                        seq,
                        hash: step.head.hash,
                        reason: verdict.reason,
                    },
                },
            );
        }

        if let Body::Policy(policy) = &submission.body {
            self.state.policy = policy.clone();
            self.policy_seq = seq;
        }
        if let Body::Approval(approval) = &submission.body {
            self.state.approvals.insert(
                seq,
                Given {
                    request: approval.request,
                    verdict: approval.verdict,
                    used_by: None,
                },
            );
            if approval.verdict == Answered::Decline {
                self.state.declined.insert(approval.request, seq);
            }
        }
        if let Body::Dispatch(dispatch) = &submission.body {
            if let Some(given) = self.state.approvals.get_mut(&dispatch.approval) {
                given.used_by = Some(seq);
            }
            self.state
                .dispatched
                .insert((dispatch.vessel, dispatch.key));
        }
        island_auth::apply(&mut self.state, seq, submission);

        self.head = Some(step.head);
        self.appender = step.appender;
        self.genesis_seal = step.genesis_seal;
    }

    /// [`Chain::check`] then [`Chain::advance`], naming the position on
    /// failure.
    pub fn apply(&mut self, record: &Record) -> Result<(), ReplayError> {
        let step = self.check(record).map_err(|fault| ReplayError {
            seq: self.next_seq(),
            fault,
        })?;
        self.advance(record, step);
        Ok(())
    }
}

/// The hash and the writer's signature. The appender's signature is
/// checked by the caller, against the key the chain declares.
fn record_faults(record: &Record) -> Result<(), VerifyError> {
    if record.entry.hash() != record.hash {
        return Err(VerifyError::HashMismatch);
    }
    record
        .entry
        .verify_writer()
        .map_err(|_| VerifyError::BadWriterSignature)
}

/// Whether a kind belongs on a stream (docs/spec.md, the "Stream" column
/// of the kinds table).
fn belongs(kind: Kind, stream: &StreamId) -> bool {
    let stream = stream.as_bytes();
    let is = |name: &str| stream == name.as_bytes();
    let system = is(StreamId::POLICY)
        || is(StreamId::AUTHORITY)
        || is(StreamId::GATE)
        || is(StreamId::REPOS)
        || is(StreamId::ACCESS)
        || is(StreamId::APPROVALS);
    match kind {
        Kind::Policy => is(StreamId::POLICY),
        Kind::Grant | Kind::Revoke | Kind::Genesis | Kind::Bootstrap => is(StreamId::AUTHORITY),
        Kind::Verdict => is(StreamId::GATE),
        Kind::Receipt => is(StreamId::REPOS),
        Kind::Access => is(StreamId::ACCESS),
        Kind::Approval | Kind::Dispatch => is(StreamId::APPROVALS),
        // A project.
        Kind::Verse => !system && !is(StreamId::PLAN),
        // A project, or the plan.
        Kind::Preference => !system,
    }
}

/// Replay records from genesis: every record must follow the ones before
/// it. Returns the chain, with its state and state hash, or the first
/// record that does not belong.
pub fn replay<'a>(
    pin: PublicKey,
    records: impl IntoIterator<Item = &'a Record>,
) -> Result<Chain, ReplayError> {
    let mut chain = Chain::new(pin);
    for record in records {
        chain.apply(record)?;
    }
    Ok(chain)
}
