//! Island log: the hash-chained log, its single appender, its reader and
//! replay.
//!
//! - [`frame`]: how records sit in a file, and what a crash can leave.
//! - [`chain`]: what makes records a valid log, and the state they replay
//!   to. Pure.
//! - [`appender`]: the write path, over `island-env`'s clock and storage.
//! - [`genesis`]: the two submissions that start a log.
//! - [`read`]: a log file's bytes to verified records. Pure; this and
//!   [`chain`] are the part Flare links.
//!
//! Build steps 3 and 5 of docs/spec.md. The gate (step 6) is not checked
//! yet: until then the appender takes any signed, correctly counted
//! submission that a grant covers.

#![warn(missing_docs)]

pub mod appender;
pub mod chain;
pub mod frame;
pub mod genesis;

use std::fmt;

use island_core::{DecodeError, PublicKey, Record};

pub use appender::{Ack, Appender, OpenError, Recovery, Rejected};
pub use chain::{Chain, Fault, Head, ReplayError, replay};
pub use frame::FrameError;
pub use island_auth::Denied;

/// Why bytes are not a valid log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogError {
    /// The file's framing is damaged.
    Frame(FrameError),
    /// A complete frame does not hold a canonical record.
    Decode {
        /// Which record, counting from 0.
        index: u64,
        /// Why.
        error: DecodeError,
    },
    /// A record does not follow the ones before it.
    Replay(ReplayError),
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => error.fmt(f),
            Self::Decode { index, error } => write!(f, "record {index} does not decode: {error}"),
            Self::Replay(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for LogError {}

/// A log read from bytes and verified from genesis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Log {
    /// Every record, in order.
    pub records: Vec<Record>,
    /// The chain they form, with the state they replay to.
    pub chain: Chain,
    /// How many bytes at the end belong to an unfinished record: one being
    /// written right now, or one torn by a crash. They are not part of the
    /// log.
    pub torn: usize,
}

/// Read a log file's bytes: split the frames, decode every record and
/// replay from genesis against the pinned root key `pin`.
///
/// Never changes anything; a torn tail is reported, not dropped.
pub fn read(bytes: &[u8], pin: PublicKey) -> Result<Log, LogError> {
    let scan = frame::scan(bytes).map_err(LogError::Frame)?;
    let records =
        decode_records(&scan).map_err(|(index, error)| LogError::Decode { index, error })?;
    let chain = replay(pin, &records).map_err(LogError::Replay)?;
    Ok(Log {
        records,
        chain,
        torn: scan.torn,
    })
}

/// Decode every scanned frame, naming the first that fails.
fn decode_records(scan: &frame::Scan<'_>) -> Result<Vec<Record>, (u64, DecodeError)> {
    scan.records
        .iter()
        .enumerate()
        .map(|(index, bytes)| Record::decode(bytes).map_err(|error| (index as u64, error)))
        .collect()
}
