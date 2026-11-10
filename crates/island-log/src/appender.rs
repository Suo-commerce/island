//! The appender: one writer of the log, one order (docs/spec.md, "Write
//! path and gate").
//!
//! For each submission, in this order:
//!
//! 1. the writer's signature;
//! 2. the writer's counter: the next number goes on, an exact resend gets
//!    its first answer again, anything else is refused;
//! 3. a grant in force that lets the writer write it (`island-auth`);
//! 4. *(the gate: build step 6)*;
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
    Answer, DecodeError, Entry, Hash, PublicKey, Record, Refusal, Signature, SigningKey,
    Submission, public_key,
};
use island_env::{Clock, Storage};

use crate::chain::{Chain, Fault, ReplayError};
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

/// A submission is not in the log. Except for [`Rejected::Refused`], no
/// entry was written and the writer's counter did not move.
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
    /// An exact resend of a submission the gate refused: the original
    /// refusal, with its `verdict` entry.
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
}

impl<S: Storage, C: Clock> Appender<S, C> {
    /// Open a log for appending: read what is stored, drop a torn tail,
    /// and replay from genesis.
    ///
    /// `pin` is the root public key. An empty store becomes a new log
    /// that accepts only a genesis declaring it.
    pub fn open(
        mut storage: S,
        clock: C,
        key: SigningKey,
        pin: PublicKey,
    ) -> Result<(Self, Recovery), OpenError> {
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
            },
            recovery,
        ))
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

        // 3 is part of the check below, so that replay makes it too.
        // 4. The gate joins here.

        // 5. Position. `at` never goes back, whatever the clock does.
        let now = self.clock.now_micros();
        let entry = Entry {
            seq: self.chain.next_seq(),
            at: self.chain.head().map_or(now, |head| now.max(head.at)),
            policy: self.chain.policy_seq(),
            prev: self.chain.prev(),
            submission,
            sig_writer,
        };

        // 6. Hash and sign, then the same check replay will make.
        let record = Record::seal(entry, &self.key);
        let step = self.chain.check(&record).map_err(|fault| match fault {
            Fault::Denied(denied) => Rejected::Denied(denied),
            fault => Rejected::Invalid(fault),
        })?;
        let framed = frame::frame(&record.encode()).ok_or(Rejected::TooLarge)?;

        // 7. Durable, or stop.
        if let Err(error) = self
            .storage
            .append(&framed)
            .and_then(|()| self.storage.sync())
        {
            self.stopped = true;
            return Err(Rejected::Storage(error.kind()));
        }

        // 8. Acknowledge.
        let ack = Ack {
            seq: record.entry.seq,
            hash: record.hash,
        };
        self.chain.advance(&record, step);
        Ok(ack)
    }
}
