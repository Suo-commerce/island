//! Island environment: the clock, randomness and storage the rest of the
//! workspace is not allowed to reach for on its own.
//!
//! [`Clock`], [`Rng`] and [`Storage`] are the three ways the outside world
//! gets into Island. The appender takes them as values, so the harness can
//! put a simulated clock, a seeded generator and a storage that fails on
//! command in their place and run the same code twice with the same
//! result. The production implementations are here too: this is the one
//! crate exempt from the workspace's determinism bans (see its
//! `clippy.toml`).

#![warn(missing_docs)]

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock time.
pub trait Clock {
    /// Microseconds since the Unix epoch.
    ///
    /// May go backwards between calls; the appender keeps `at` monotonic
    /// itself.
    fn now_micros(&self) -> u64;
}

/// A source of random bytes.
pub trait Rng {
    /// Fill `bytes` with random bytes.
    fn fill(&mut self, bytes: &mut [u8]) -> io::Result<()>;
}

/// An append-only byte store with an explicit durability point.
///
/// Bytes handed to [`Storage::append`] are durable only once a later
/// [`Storage::sync`] has returned `Ok`. After an error from either, the
/// stored bytes are in an unknown state until they are read again.
pub trait Storage {
    /// Everything stored so far, durable or not.
    fn read(&mut self) -> io::Result<Vec<u8>>;

    /// Add `bytes` at the end.
    fn append(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Make everything appended so far durable.
    fn sync(&mut self) -> io::Result<()>;

    /// Cut the store back to its first `len` bytes. Used once, on opening,
    /// to drop a record that was torn by a crash.
    fn truncate(&mut self, len: u64) -> io::Result<()>;
}

// ── Production implementations ──────────────────────────────────────

/// The system's wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_micros(&self) -> u64 {
        // A clock set before 1970 reads as 0; one past the year 586,000 as
        // the largest value.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_micros()).unwrap_or(u64::MAX)
            })
    }
}

/// The operating system's random generator.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsRng;

impl Rng for OsRng {
    fn fill(&mut self, bytes: &mut [u8]) -> io::Result<()> {
        getrandom::fill(bytes).map_err(io::Error::from)
    }
}

/// A log file on a local filesystem, held under an exclusive lock.
///
/// The lock is what makes "one appender" true across processes: a second
/// [`FileStorage`] on the same file fails to open while the first lives.
/// The lock is advisory and dies with the process, so a crash never
/// leaves the log locked.
#[derive(Debug)]
pub struct FileStorage {
    file: File,
}

impl FileStorage {
    /// Create a new, empty log file. Fails if the path exists.
    ///
    /// The file and its directory entry are synced before this returns, so
    /// a crash right after cannot leave a log that was written to but is
    /// not there.
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(path)?;
        lock(&file)?;
        file.sync_all()?;
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        File::open(parent)?.sync_all()?;
        Ok(Self { file })
    }

    /// Open an existing log file. Fails if another process holds it.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).append(true).open(path)?;
        lock(&file)?;
        Ok(Self { file })
    }
}

fn lock(file: &File) -> io::Result<()> {
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "the log is held by another appender",
        ),
        std::fs::TryLockError::Error(error) => error,
    })
}

impl Storage for FileStorage {
    fn read(&mut self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        // The file is open in append mode: every write lands at the end.
        self.file.write_all(bytes)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.file.set_len(len)?;
        self.file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_keeps_what_was_appended_and_synced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("island.log");
        {
            let mut storage = FileStorage::create(&path).expect("create");
            storage.append(b"one").expect("append");
            storage.append(b"two").expect("append");
            storage.sync().expect("sync");
            assert_eq!(storage.read().expect("read"), b"onetwo");
            // Reading does not move where the next append lands.
            storage.append(b"!").expect("append");
            assert_eq!(storage.read().expect("read"), b"onetwo!");
        }
        let mut storage = FileStorage::open(&path).expect("open");
        assert_eq!(storage.read().expect("read"), b"onetwo!");
        storage.truncate(3).expect("truncate");
        storage.append(b"2").expect("append");
        assert_eq!(storage.read().expect("read"), b"one2");
    }

    #[test]
    fn create_refuses_an_existing_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("island.log");
        drop(FileStorage::create(&path).expect("create"));
        let error = FileStorage::create(&path).expect_err("exists");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_second_appender_cannot_open_the_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("island.log");
        let first = FileStorage::create(&path).expect("create");
        let error = FileStorage::open(&path).expect_err("locked");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(first);
        FileStorage::open(&path).expect("free again");
    }

    #[test]
    fn the_clock_and_the_generator_work() {
        // 2026-01-01T00:00:00Z, in microseconds.
        assert!(SystemClock.now_micros() > 1_767_225_600_000_000);
        let mut bytes = [0u8; 32];
        OsRng.fill(&mut bytes).expect("random bytes");
        assert_ne!(bytes, [0u8; 32]);
    }
}
