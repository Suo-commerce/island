//! The appender: one writer of the log, one order (docs/spec.md, "Write
//! path and gate").
//!
//! For each submission, in this order:
//!
//! 1. the writer's signature;
//! 2. the writer's counter: the next number goes on, an exact resend gets
//!    its first answer again, anything else is refused;
//! 3. a grant in force that lets the writer write it (`island-auth`);
//! 4. the gate (`island-gate`): if it refuses, a `verdict` entry carrying
//!    the whole submission is appended instead, and the writer's counter
//!    advances either way;
//! 5. position: `seq`, `at`, `policy`, `prev`;
//! 6. hash and sign;
//! 7. `append`, then `sync`;
//! 8. acknowledge.
//!
//! Nothing is acknowledged before `sync` has succeeded, and the state the
//! appender judges from never runs ahead of what is durable. If storage
//! fails, the appender stops for good; opening the log again replays what
//! is really there.
//!
//! The clock and storage come from `island-env`. This is the only code in
//! the crate that touches either.

use std::fmt;
use std::io;

use island_auth::Denied;
use island_core::{
    Answer, Body, DecodeError, Entry, Hash, PublicKey, Record, Refusal, Signature, SigningKey,
    StreamId, Submission, Verdict, public_key, sign_hash,
};
use island_env::{Clock, Storage};

use crate::chain::{Chain, Fault, ReplayError, Step};
use crate::frame::{self, FrameError};
use crate::{LogError, decode_records};

/// A submission is in the log, durably, at `seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    /// The entry's position.
    pub seq: u64,
    /// The entry's hash.
    pub hash: Hash,
}

/// A submission is not in the log as an entry of its own. Except for
/// [`Rejected::Refused`], nothing was written and the writer's counter did
/// not move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// The appender has stopped after a storage failure. Open the log
    /// again.
    Stopped,
    /// The writer's signature does not verify.
    BadSignature,
    /// The counter was already used, by this or an earlier submission: a
    /// replay.
    CounterOld {
        /// The writer's next number.
        expected: Option<u64>,
        /// The submission's counter.
        found: u64,
    },
    /// The counter skips ahead.
    CounterGap {
        /// The writer's next number.
        expected: Option<u64>,
        /// The submission's counter.
        found: u64,
    },
    /// The counter is the writer's latest, with different content.
    CounterConflict {
        /// The counter.
        counter: u64,
    },
    /// The gate refused it. A `verdict` entry records the refusal and the
    /// writer's counter has advanced; an exact resend gets this same
    /// answer and adds nothing.
    Refused {
        /// The verdict entry's position.
        seq: u64,
        /// The verdict entry's hash.
        hash: Hash,
        /// Why it was refused.
        reason: Refusal,
    },
    /// Authority does not allow the write: no grant covers it, or the
    /// grant or revoke it carries is not allowed.
    Denied(Denied),
    /// A `verdict` submitted from outside. Only the appender's own gate
    /// writes them.
    VerdictFromOutside,
    /// The record would not be a valid next entry.
    Invalid(Fault),
    /// The record is larger than a frame may hold.
    TooLarge,
    /// Storage failed. The appender has stopped; whether the entry is
    /// durable is unknown until the log is opened again, so resend then.
    Storage(io::ErrorKind),
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => f.write_str("the appender has stopped after a storage failure"),
            Self::BadSignature => f.write_str("writer signature does not verify"),
            Self::CounterOld { found, .. } => write!(f, "counter {found} was already used"),
            Self::CounterGap {
                expected: Some(expected),
                found,
            } => write!(f, "counter {found} skips ahead; the next is {expected}"),
            Self::CounterGap { found, .. } => write!(f, "counter {found} skips ahead"),
            Self::CounterConflict { counter } => write!(
                f,
                "counter {counter} was already used for a different submission"
            ),
            Self::Refused { seq, reason, .. } => {
                write!(
                    f,
                    "refused by the gate ({reason:?}); verdict at entry {seq}"
                )
            }
            Self::Denied(denied) => write!(f, "not authorized: {denied}"),
            Self::VerdictFromOutside => f.write_str("only the appender writes verdict entries"),
            Self::Invalid(fault) => write!(f, "not a valid next entry: {fault}"),
            Self::TooLarge => f.write_str("the record is too large"),
            Self::Storage(kind) => write!(f, "storage failed ({kind}); the appender has stopped"),
        }
    }
}

impl std::error::Error for Rejected {}

/// Why a log could not be opened for appending.
#[derive(Debug)]
pub enum OpenError {
    /// Storage failed.
    Io(io::Error),
    /// The stored bytes are not a valid log.
    Log(LogError),
    /// The log's bootstrap entry declares another appender key.
    WrongAppenderKey,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "storage: {error}"),
            Self::Log(error) => error.fmt(f),
            Self::WrongAppenderKey => {
                f.write_str("this key is not the appender key the log declares")
            }
        }
    }
}

impl std::error::Error for OpenError {}

impl From<io::Error> for OpenError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<LogError> for OpenError {
    fn from(error: LogError) -> Self {
        Self::Log(error)
    }
}

impl From<FrameError> for OpenError {
    fn from(error: FrameError) -> Self {
        Self::Log(LogError::Frame(error))
    }
}

impl From<ReplayError> for OpenError {
    fn from(error: ReplayError) -> Self {
        Self::Log(LogError::Replay(error))
    }
}

impl From<(u64, DecodeError)> for OpenError {
    fn from((index, error): (u64, DecodeError)) -> Self {
        Self::Log(LogError::Decode { index, error })
    }
}

/// What opening found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// How many records the log holds.
    pub records: u64,
    /// How many bytes of an unfinished last record were dropped. Such a
    /// record was never acknowledged.
    pub dropped: u64,
}

/// The single appender of a log.
#[derive(Debug)]
pub struct Appender<S, C> {
    storage: S,
    clock: C,
    key: SigningKey,
    chain: Chain,
    stopped: bool,
    /// The record the running `submit_keeping` call has stored, if any.
    kept: Option<Record>,
    keeping: bool,
}

impl<S: Storage, C: Clock> Appender<S, C> {
    /// Open a log for appending: read what is stored, drop a torn tail,
    /// and replay from genesis.
    ///
    /// `pin` is the root public key. An empty store becomes a new log
    /// that accepts only a genesis declaring it.
    pub fn open(
        storage: S,
        clock: C,
        key: SigningKey,
        pin: PublicKey,
    ) -> Result<(Self, Recovery), OpenError> {
        let (appender, _, recovery) = Self::open_with_records(storage, clock, key, pin)?;
        Ok((appender, recovery))
    }

    /// [`Appender::open`], also handing back every record the log holds,
    /// for a caller that serves them to readers.
    pub fn open_with_records(
        mut storage: S,
        clock: C,
        key: SigningKey,
        pin: PublicKey,
    ) -> Result<(Self, Vec<Record>, Recovery), OpenError> {
        let bytes = storage.read()?;
        let scan = frame::scan(&bytes)?;
        let records = decode_records(&scan)?;
        let chain = crate::replay(pin, &records)?;
        if chain
            .appender()
            .is_some_and(|declared| declared != public_key(&key))
        {
            return Err(OpenError::WrongAppenderKey);
        }

        // Only now, with everything before it known to be a valid log, is
        // the torn tail dropped.
        if scan.torn > 0 {
            storage.truncate(scan.valid_len as u64)?;
        }
        if scan.valid_len == 0 {
            storage.append(&frame::header())?;
        }
        storage.sync()?;

        let recovery = Recovery {
            records: records.len() as u64,
            dropped: scan.torn as u64,
        };
        Ok((
            Self {
                storage,
                clock,
                key,
                chain,
                stopped: false,
                kept: None,
                keeping: false,
            },
            records,
            recovery,
        ))
    }

    /// [`Appender::submit`], also handing back the record the submission
    /// added to the log, if it added one: the entry itself, or the verdict
    /// of a gate refusal. A retry adds nothing.
    pub fn submit_keeping(
        &mut self,
        submission: Submission,
        sig_writer: Signature,
    ) -> (Result<Ack, Rejected>, Option<Record>) {
        self.keeping = true;
        let answer = self.submit(submission, sig_writer);
        self.keeping = false;
        (answer, self.kept.take())
    }

    /// The chain as of the last acknowledged entry.
    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// The appender's public key.
    pub fn public_key(&self) -> PublicKey {
        public_key(&self.key)
    }

    /// Whether a storage failure has stopped the appender.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Run the write path for one submission.
    pub fn submit(
        &mut self,
        submission: Submission,
        sig_writer: Signature,
    ) -> Result<Ack, Rejected> {
        if self.stopped {
            return Err(Rejected::Stopped);
        }

        // 1. Writer signature. A forged submission is not worth a record.
        submission
            .verify(&sig_writer)
            .map_err(|_| Rejected::BadSignature)?;

        // 2. Counter.
        let found = submission.counter;
        let expected = self.chain.next_counter(&submission.writer);
        if expected != Some(found) {
            let last = self.chain.state().writers.get(&submission.writer);
            return Err(match last {
                Some(last) if last.counter == found => {
                    if last.sub_hash != submission.hash() {
                        Rejected::CounterConflict { counter: found }
                    } else {
                        // A retry: the first answer again, nothing appended.
                        match last.answer {
                            Answer::Accepted { seq, hash } => return Ok(Ack { seq, hash }),
                            Answer::Refused { seq, hash, reason } => {
                                Rejected::Refused { seq, hash, reason }
                            }
                        }
                    }
                }
                _ if expected.is_some_and(|expected| found > expected) => {
                    Rejected::CounterGap { expected, found }
                }
                _ => Rejected::CounterOld { expected, found },
            });
        }

        // A verdict comes from the appender's own gate, never from outside.
        if matches!(submission.body, Body::Verdict(_)) && self.chain.next_seq() >= 2 {
            return Err(Rejected::VerdictFromOutside);
        }

        // 3 to 6. Position, hash and sign, then the check replay will make:
        // it holds the grant check and the gate.
        let record = self.seal(submission.clone(), sig_writer);
        match self.chain.check(&record) {
            Ok(step) => self.store(&record, step),
            // 4. The gate refuses: the verdict is appended instead, carrying
            // the whole submission, and that is the writer's answer.
            Err(Fault::Gate(reason)) => {
                let me = self.public_key();
                let verdict = Submission {
                    stream: StreamId::from(StreamId::GATE),
                    writer: me,
                    counter: self.chain.next_counter(&me).ok_or(Rejected::Invalid(
                        Fault::Counter {
                            expected: None,
                            found: 0,
                        },
                    ))?,
                    body: Body::Verdict(Verdict {
                        rejected: Box::new(submission),
                        sig_writer,
                        reason,
                    }),
                };
                let signature = sign_hash(&self.key, &verdict.hash());
                let record = self.seal(verdict, signature);
                let step = self.chain.check(&record).map_err(Rejected::Invalid)?;
                let ack = self.store(&record, step)?;
                Err(Rejected::Refused {
                    seq: ack.seq,
                    hash: ack.hash,
                    reason,
                })
            }
            Err(Fault::Denied(denied)) => Err(Rejected::Denied(denied)),
            Err(fault) => Err(Rejected::Invalid(fault)),
        }
    }

    /// 5 and 6. Give a submission the next position, then hash and sign.
    /// `at` never goes back, whatever the clock does.
    fn seal(&self, submission: Submission, sig_writer: Signature) -> Record {
        let now = self.clock.now_micros();
        Record::seal(
            Entry {
                seq: self.chain.next_seq(),
                at: self.chain.head().map_or(now, |head| now.max(head.at)),
                policy: self.chain.policy_seq(),
                prev: self.chain.prev(),
                submission,
                sig_writer,
            },
            &self.key,
        )
    }

    /// 7 and 8. Make a checked record durable, or stop; then take it into
    /// the state and acknowledge.
    fn store(&mut self, record: &Record, step: Step) -> Result<Ack, Rejected> {
        let framed = frame::frame(&record.encode()).ok_or(Rejected::TooLarge)?;
        if let Err(error) = self
            .storage
            .append(&framed)
            .and_then(|()| self.storage.sync())
        {
            self.stopped = true;
            return Err(Rejected::Storage(error.kind()));
        }
        self.chain.advance(record, step);
        if self.keeping {
            self.kept = Some(record.clone());
        }
        Ok(Ack {
            seq: record.entry.seq,
            hash: record.hash,
        })
    }
}
