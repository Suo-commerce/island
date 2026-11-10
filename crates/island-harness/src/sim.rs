//! The simulated environment: storage that fails and crashes on command,
//! a clock set by hand, and a seeded generator. They implement
//! `island-env`'s traits, so the appender under test is the real one.
//!
//! Nothing here reads the real clock, the real disk or real randomness,
//! which is what lets a case run twice with the same result.

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;

use island_env::{Clock, Rng, Storage};

#[derive(Debug, Default)]
struct Disk {
    /// Everything appended: the page cache.
    bytes: Vec<u8>,
    /// How many leading bytes a sync has made durable.
    synced: usize,
    /// The next sync fails.
    fail_sync: bool,
    /// The next append writes only this many bytes, then fails.
    tear_append: Option<usize>,
    /// A fault has fired since it was last asked.
    fired: bool,
}

/// In-memory storage with a durability point, like a file with a page
/// cache. Clones share the same disk, so the harness keeps a handle to the
/// storage the appender owns.
#[derive(Clone, Debug, Default)]
pub struct SimStorage(Rc<RefCell<Disk>>);

impl SimStorage {
    /// Everything appended so far, durable or not.
    pub fn bytes(&self) -> Vec<u8> {
        self.0.borrow().bytes.clone()
    }

    /// How many bytes are stored, durable or not.
    pub fn len(&self) -> usize {
        self.0.borrow().bytes.len()
    }

    /// Whether nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many leading bytes are durable.
    pub fn synced_len(&self) -> usize {
        self.0.borrow().synced
    }

    /// Make the next sync fail.
    pub fn fail_next_sync(&self) {
        self.0.borrow_mut().fail_sync = true;
    }

    /// Make the next append write only its first `at` bytes, then fail.
    pub fn tear_next_append(&self, at: usize) {
        self.0.borrow_mut().tear_append = Some(at);
    }

    /// Forget any fault that has not fired yet.
    pub fn disarm(&self) {
        let mut disk = self.0.borrow_mut();
        disk.fail_sync = false;
        disk.tear_append = None;
    }

    /// Whether a fault has fired since this was last asked.
    pub fn take_fired(&self) -> bool {
        std::mem::take(&mut self.0.borrow_mut().fired)
    }

    /// A crash: of the bytes not yet durable, only the first `keep`
    /// survive. Returns how many were lost.
    pub fn crash(&self, keep: usize) -> usize {
        let mut disk = self.0.borrow_mut();
        let end = disk.synced.saturating_add(keep).min(disk.bytes.len());
        let lost = disk.bytes.len() - end;
        disk.bytes.truncate(end);
        disk.synced = end;
        lost
    }
}

impl Storage for SimStorage {
    fn read(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.bytes())
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut disk = self.0.borrow_mut();
        if let Some(at) = disk.tear_append.take() {
            let at = at.min(bytes.len());
            disk.bytes.extend(&bytes[..at]);
            disk.fired = true;
            return Err(io::Error::other("simulated: append torn"));
        }
        disk.bytes.extend(bytes);
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        let mut disk = self.0.borrow_mut();
        if std::mem::take(&mut disk.fail_sync) {
            disk.fired = true;
            return Err(io::Error::other("simulated: sync failed"));
        }
        disk.synced = disk.bytes.len();
        Ok(())
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        let mut disk = self.0.borrow_mut();
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        disk.bytes.truncate(len);
        disk.synced = disk.synced.min(len);
        Ok(())
    }
}

/// A clock that reads what the harness last set. It can go backwards.
#[derive(Clone, Debug)]
pub struct SimClock(Rc<Cell<u64>>);

impl SimClock {
    /// A clock that reads `micros`.
    pub fn at(micros: u64) -> Self {
        Self(Rc::new(Cell::new(micros)))
    }

    /// Move forward.
    pub fn advance(&self, micros: u64) {
        self.0.set(self.0.get().saturating_add(micros));
    }

    /// Jump back, as a clock corrected by NTP does.
    pub fn back(&self, micros: u64) {
        self.0.set(self.0.get().saturating_sub(micros));
    }
}

impl Clock for SimClock {
    fn now_micros(&self) -> u64 {
        self.0.get()
    }
}

/// A generator that gives the same bytes for the same seed (SplitMix64).
/// Not for keys that must be secret.
#[derive(Clone, Debug)]
pub struct SeededRng(u64);

impl SeededRng {
    /// A generator for `seed`.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl Rng for SeededRng {
    fn fill(&mut self, bytes: &mut [u8]) -> io::Result<()> {
        for chunk in bytes.chunks_mut(8) {
            let word = self.next().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_keeps_what_was_synced_and_a_prefix_of_the_rest() {
        let disk = SimStorage::default();
        let mut storage = disk.clone();
        storage.append(b"durable").expect("append");
        storage.sync().expect("sync");
        storage.append(b"pending").expect("append");
        assert_eq!((disk.len(), disk.synced_len()), (14, 7));

        assert_eq!(disk.crash(3), 4);
        assert_eq!(disk.bytes(), b"durablepen");
        assert_eq!(disk.synced_len(), 10);
        assert_eq!(disk.crash(100), 0);
    }

    #[test]
    fn faults_fire_once_and_say_so() {
        let disk = SimStorage::default();
        let mut storage = disk.clone();

        disk.fail_next_sync();
        storage.append(b"abc").expect("append");
        assert!(storage.sync().is_err());
        assert!(disk.take_fired());
        assert!(!disk.take_fired());
        assert_eq!(disk.synced_len(), 0);
        storage.sync().expect("the next sync works");

        disk.tear_next_append(2);
        assert!(storage.append(b"defg").is_err());
        assert_eq!(disk.bytes(), b"abcde");
        assert!(disk.take_fired());

        disk.fail_next_sync();
        disk.disarm();
        storage.sync().expect("disarmed");
    }

    #[test]
    fn the_seeded_generator_repeats_for_a_seed_and_differs_between_seeds() {
        let fill = |seed| {
            let mut bytes = [0u8; 20];
            SeededRng::new(seed).fill(&mut bytes).expect("fill");
            bytes
        };
        assert_eq!(fill(7), fill(7));
        assert_ne!(fill(7), fill(8));
        // SplitMix64's first output for seed 0.
        assert_eq!(SeededRng::new(0).next(), 0xE220_A839_7B1D_CDAF);
    }

    #[test]
    fn the_clock_goes_where_it_is_told() {
        let clock = SimClock::at(100);
        clock.advance(50);
        assert_eq!(clock.now_micros(), 150);
        clock.back(500);
        assert_eq!(clock.now_micros(), 0);
    }
}
