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
//! kind belongs to, the writer's counter, and the `policy` field.
//!
//! Not checked yet: grants (build step 5) and the gate (build step 6).
//! Until the gate exists a `verdict` entry cannot be re-judged, so replay
//! refuses one instead of half-checking it.

use std::fmt;

use island_core::{
    Answer, Body, Hash, Kind, Last, PublicKey, Record, Signature, State, StreamId, VerifyError,
    state_hash, verify_hash,
};

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
    /// A `genesis` or `bootstrap` entry after entries 0 and 1.
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
    /// A `verdict` entry, which cannot be re-judged before the gate exists
    /// (build step 6).
    VerdictWithoutGate,
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
            Self::Misplaced { kind } => write!(f, "a {kind:?} entry is valid only at the start"),
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
            Self::VerdictWithoutGate => {
                f.write_str("a verdict entry cannot be re-judged before the gate exists")
            }
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

        // Genesis, bootstrap, and nothing like them later.
        let kind = submission.kind();
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
                let mut holders = bootstrap
                    .grants
                    .iter()
                    .filter(|grant| grant.rights.contains(&Kind::Verdict))
                    .map(|grant| grant.holder);
                match (holders.next(), holders.next()) {
                    (Some(holder), None) if holder != self.pin => appender = Some(holder),
                    _ => return Err(Fault::BootstrapAppender),
                }
            }
            (1, _) => return Err(Fault::NotBootstrap),
            (_, Body::Genesis(_) | Body::Bootstrap(_)) => return Err(Fault::Misplaced { kind }),
            (_, Body::Verdict(_)) => return Err(Fault::VerdictWithoutGate),
            _ => {}
        }

        // The stream each kind belongs to.
        if !belongs(kind, &submission.stream) {
            return Err(Fault::Stream {
                kind,
                stream: submission.stream.clone(),
            });
        }

        // The writer's counter: exactly its next number.
        let expected = self.next_counter(&submission.writer);
        if expected != Some(submission.counter) {
            return Err(Fault::Counter {
                expected,
                found: submission.counter,
            });
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

        if let Body::Policy(policy) = &submission.body {
            self.state.policy = policy.clone();
            self.policy_seq = seq;
        }

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
    let system = is(StreamId::POLICY) || is(StreamId::AUTHORITY) || is(StreamId::GATE);
    match kind {
        Kind::Policy => is(StreamId::POLICY),
        Kind::Grant | Kind::Revoke | Kind::Genesis | Kind::Bootstrap => is(StreamId::AUTHORITY),
        Kind::Verdict => is(StreamId::GATE),
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
