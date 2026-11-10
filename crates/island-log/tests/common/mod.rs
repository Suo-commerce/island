//! Shared by the integration tests: keys, submissions, an in-memory
//! storage that fails and crashes on command, and a clock set by hand.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;

use island_core::{
    Body, Entry, Evidence, PublicKey, Record, Signature, SigningKey, StreamId, Submission, Verse,
    public_key,
};
use island_env::{Clock, Storage};
use island_log::{Ack, Appender, genesis};

pub fn root() -> SigningKey {
    SigningKey::from_bytes(&[0x52; 32])
}

pub fn appender_key() -> SigningKey {
    SigningKey::from_bytes(&[0x41; 32])
}

pub fn writer() -> SigningKey {
    SigningKey::from_bytes(&[0x57; 32])
}

pub fn pin() -> PublicKey {
    public_key(&root())
}

pub fn sign(submission: &Submission, key: &SigningKey) -> Signature {
    submission.sign(key).expect("the key is the writer")
}

pub fn verse(key: &SigningKey, counter: u64, text: &str) -> Submission {
    Submission {
        stream: StreamId::from("island"),
        writer: public_key(key),
        counter,
        body: Body::Verse(Verse::new(Evidence::Decision, text, vec![])),
    }
}

/// Seal an entry the way an appender would, with any key and any fields.
pub fn seal(
    seq: u64,
    at: u64,
    policy: u64,
    prev: [u8; 32],
    submission: Submission,
    writer: &SigningKey,
    appender: &SigningKey,
) -> Record {
    let sig_writer = sign(&submission, writer);
    Record::seal(
        Entry {
            seq,
            at,
            policy,
            prev,
            submission,
            sig_writer,
        },
        appender,
    )
}

// ── Storage ─────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Mem {
    /// Everything appended.
    pub bytes: Vec<u8>,
    /// How many leading bytes a sync has made durable.
    pub synced: usize,
    /// The next append writes only this many bytes, then fails.
    pub tear_next_append: Option<usize>,
    /// The next sync fails.
    pub fail_next_sync: bool,
}

/// In-memory storage. Clones share the same bytes, so a test keeps one
/// handle to look at and break what the appender holds.
#[derive(Clone, Debug, Default)]
pub struct MemStorage(pub Rc<RefCell<Mem>>);

impl MemStorage {
    pub fn with_bytes(bytes: Vec<u8>) -> Self {
        let synced = bytes.len();
        Self(Rc::new(RefCell::new(Mem {
            bytes,
            synced,
            ..Mem::default()
        })))
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.0.borrow().bytes.clone()
    }

    pub fn len(&self) -> usize {
        self.0.borrow().bytes.len()
    }

    /// A crash: of the bytes not yet synced, only the first `keep`
    /// survive.
    pub fn crash(&self, keep: usize) {
        let mut mem = self.0.borrow_mut();
        let end = (mem.synced + keep).min(mem.bytes.len());
        mem.bytes.truncate(end);
        mem.synced = end;
    }
}

impl Storage for MemStorage {
    fn read(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.0.borrow().bytes.clone())
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut mem = self.0.borrow_mut();
        if let Some(keep) = mem.tear_next_append.take() {
            let keep = keep.min(bytes.len());
            mem.bytes.extend(&bytes[..keep]);
            return Err(io::Error::other("append torn"));
        }
        mem.bytes.extend(bytes);
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        let mut mem = self.0.borrow_mut();
        if std::mem::take(&mut mem.fail_next_sync) {
            return Err(io::Error::other("sync failed"));
        }
        mem.synced = mem.bytes.len();
        Ok(())
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        let mut mem = self.0.borrow_mut();
        mem.bytes.truncate(len as usize);
        mem.synced = mem.synced.min(len as usize);
        Ok(())
    }
}

// ── Clock ───────────────────────────────────────────────────────────

/// A clock that reads what the test last set.
#[derive(Clone, Debug)]
pub struct ManualClock(pub Rc<Cell<u64>>);

impl ManualClock {
    pub fn at(micros: u64) -> Self {
        Self(Rc::new(Cell::new(micros)))
    }

    pub fn set(&self, micros: u64) {
        self.0.set(micros);
    }
}

impl Clock for ManualClock {
    fn now_micros(&self) -> u64 {
        self.0.get()
    }
}

// ── A log to start from ─────────────────────────────────────────────

pub type TestAppender = Appender<MemStorage, ManualClock>;

pub fn open(storage: &MemStorage, clock: &ManualClock) -> TestAppender {
    Appender::open(storage.clone(), clock.clone(), appender_key(), pin())
        .expect("the log opens")
        .0
}

pub fn submit(appender: &mut TestAppender, submission: Submission, key: &SigningKey) -> Ack {
    let signature = sign(&submission, key);
    appender
        .submit(submission, signature)
        .expect("the submission is accepted")
}

/// A new log with its genesis and bootstrap entries.
pub fn started() -> (TestAppender, MemStorage, ManualClock) {
    let storage = MemStorage::default();
    let clock = ManualClock::at(1_000);
    let mut appender = open(&storage, &clock);
    submit(&mut appender, genesis::genesis(pin()), &root());
    submit(
        &mut appender,
        genesis::bootstrap(pin(), public_key(&appender_key())),
        &root(),
    );
    (appender, storage, clock)
}

/// The records of a started log followed by `n` verses.
pub fn records(n: u64) -> Vec<Record> {
    let (mut appender, storage, _) = started();
    for counter in 1..=n {
        submit(
            &mut appender,
            verse(&writer(), counter, &format!("verse {counter}")),
            &writer(),
        );
    }
    island_log::read(&storage.bytes(), pin())
        .expect("the log reads")
        .records
}
