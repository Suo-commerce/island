//! The replica: a verified copy of the Island log in a local file.
//!
//! The file has the log's own format (see `island_log::frame`), so every
//! Island tool reads it. It only ever grows: a batch of records is
//! appended whole after every record in it has been verified, or not at
//! all. One process holds it at a time, under an exclusive lock; others
//! may read it.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use island_core::{Hash, PublicKey, Record};
use island_log::{Chain, LogError, ReplayError, frame};

/// Why a replica could not be opened or written.
#[derive(Debug)]
pub enum ReplicaError {
    /// The file could not be read or written.
    Io(PathBuf, io::Error),
    /// Another process holds the replica.
    Busy(PathBuf),
    /// What the file holds is not a valid log for the pinned root: it was
    /// damaged, or belongs to another log.
    Invalid(PathBuf, LogError),
}

impl fmt::Display for ReplicaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Busy(path) => write!(
                f,
                "{}: another flare-link is pulling into this replica",
                path.display()
            ),
            Self::Invalid(path, error) => write!(
                f,
                "{}: the replica does not verify against the pinned root: {error}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ReplicaError {}

/// A batch the replica will not take, and the first record that showed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    /// The position of the first record that does not follow.
    pub seq: u64,
    /// Why.
    pub why: String,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "entry {}: {}", self.seq, self.why)
    }
}

impl From<ReplayError> for Refused {
    fn from(error: ReplayError) -> Self {
        Self {
            seq: error.seq,
            why: error.fault.to_string(),
        }
    }
}

/// A replica, open for pulling into.
pub struct Replica {
    path: PathBuf,
    file: File,
    records: Vec<Record>,
    chain: Chain,
}

/// A replica read without being held, while another process may be
/// pulling into it.
pub struct Snapshot {
    /// Every record, verified from genesis.
    pub records: Vec<Record>,
    /// The chain they replay to.
    pub chain: Chain,
}

/// Read and verify a replica without holding it. A record being appended
/// right now is not part of what is read.
pub fn read(path: &Path, pin: PublicKey) -> Result<Snapshot, ReplicaError> {
    let bytes = std::fs::read(path).map_err(|error| ReplicaError::Io(path.into(), error))?;
    if bytes.is_empty() {
        return Ok(Snapshot {
            records: Vec::new(),
            chain: Chain::new(pin),
        });
    }
    let log =
        island_log::read(&bytes, pin).map_err(|error| ReplicaError::Invalid(path.into(), error))?;
    Ok(Snapshot {
        records: log.records,
        chain: log.chain,
    })
}

impl Replica {
    /// Open the replica at `path`, creating it if there is none, and
    /// verify all of it from genesis against `pin`.
    ///
    /// A record cut short at the end, left by a pull that was killed, was
    /// never part of the replica and is dropped.
    pub fn open(path: &Path, pin: PublicKey) -> Result<Self, ReplicaError> {
        let at = |error| ReplicaError::Io(path.into(), error);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(at)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(at)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => ReplicaError::Busy(path.into()),
            std::fs::TryLockError::Error(error) => at(error),
        })?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(at)?;
        if bytes.is_empty() {
            file.write_all(&frame::header()).map_err(at)?;
            file.sync_all().map_err(at)?;
            return Ok(Self {
                path: path.into(),
                file,
                records: Vec::new(),
                chain: Chain::new(pin),
            });
        }
        let log = island_log::read(&bytes, pin)
            .map_err(|error| ReplicaError::Invalid(path.into(), error))?;
        if log.torn > 0 {
            let valid = (bytes.len() - log.torn) as u64;
            file.set_len(valid).map_err(at)?;
            file.sync_all().map_err(at)?;
        }
        Ok(Self {
            path: path.into(),
            file,
            records: log.records,
            chain: log.chain,
        })
    }

    /// Where the replica is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every record, verified.
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// The chain the records replay to.
    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// The newest entry's position and hash.
    pub fn head(&self) -> Option<(u64, Hash)> {
        self.chain.head().map(|head| (head.seq, head.hash))
    }

    /// Whether `batch` follows the replica: each record the next position,
    /// linked to the one before, its hash right, both signatures good, and
    /// valid under every rule of the log, judged again here. The chain it
    /// would reach, or the first record that does not follow.
    pub fn check(&self, batch: &[Record]) -> Result<Chain, Refused> {
        let mut chain = self.chain.clone();
        for record in batch {
            chain.apply(record)?;
        }
        Ok(chain)
    }

    /// Append `batch` if every record in it follows; otherwise change
    /// nothing. The records are durable when this returns `Ok`.
    pub fn append(&mut self, batch: Vec<Record>) -> Result<Result<(), Refused>, ReplicaError> {
        let chain = match self.check(&batch) {
            Ok(chain) => chain,
            Err(refused) => return Ok(Err(refused)),
        };
        let mut bytes = Vec::new();
        for record in &batch {
            let Some(framed) = frame::frame(&record.encode()) else {
                return Ok(Err(Refused {
                    seq: record.entry.seq,
                    why: "the record is larger than a log may hold".to_owned(),
                }));
            };
            bytes.extend(framed);
        }
        let at = |error| ReplicaError::Io(self.path.clone(), error);
        self.file.seek(SeekFrom::End(0)).map_err(at)?;
        self.file.write_all(&bytes).map_err(at)?;
        self.file.sync_data().map_err(at)?;
        self.records.extend(batch);
        self.chain = chain;
        Ok(Ok(()))
    }
}
