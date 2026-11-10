//! The keeper: the one thread that owns the log.
//!
//! "One appender, one thread, one order" (docs/spec.md, "Write path and
//! gate"). Every request, a write or a read, is a message to this thread
//! and is answered in the order it arrives. Nothing else in the process
//! touches the appender, the log file or the records, so there are no
//! locks, and a reader never sees an entry before it is durable.
//!
//! The keeper holds every record in memory to serve readers. That is
//! right for a log of verses; it is the first thing to change if a log
//! outgrows memory.

use std::collections::BTreeMap;
use std::path::Path;
use std::thread::JoinHandle;

use island_client::codec;
use island_client::wire::{
    Capabilities, DEFAULT_LIMIT, Head, LogQuery, MAX_LIMIT, ReadStreamRequest, Records,
    WIRE_VERSION,
};
use island_core::{PublicKey, Record, Signature, SigningKey, StreamId, Submission};
use island_env::{FileStorage, SystemClock};
use island_log::{Ack, Appender, Rejected};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// How many requests may wait for the keeper before senders wait too.
const QUEUE: usize = 256;

/// The keeper has stopped: the appender met a storage failure, or the
/// broker is shutting down. Nothing is known about a request that got
/// this answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gone;

/// One request to the keeper, with the place for its answer.
enum Command {
    Append {
        submission: Box<Submission>,
        sig_writer: Signature,
        reply: oneshot::Sender<Result<Ack, Rejected>>,
    },
    Stream {
        request: ReadStreamRequest,
        reply: oneshot::Sender<Records>,
    },
    Log {
        query: LogQuery,
        reply: oneshot::Sender<Records>,
    },
    Capabilities {
        writer: PublicKey,
        reply: oneshot::Sender<Capabilities>,
    },
    Head {
        reply: oneshot::Sender<Head>,
    },
}

/// A way to ask the keeper. Cheap to clone; every clone asks the same
/// thread.
#[derive(Clone)]
pub struct Keeper {
    tx: mpsc::Sender<Command>,
}

impl Keeper {
    /// Run the write path for one submission.
    pub async fn append(
        &self,
        submission: Submission,
        sig_writer: Signature,
    ) -> Result<Result<Ack, Rejected>, Gone> {
        self.ask(|reply| Command::Append {
            submission: Box::new(submission),
            sig_writer,
            reply,
        })
        .await
    }

    /// Records of one stream.
    pub async fn stream(&self, request: ReadStreamRequest) -> Result<Records, Gone> {
        self.ask(|reply| Command::Stream { request, reply }).await
    }

    /// Records of the whole log.
    pub async fn log(&self, query: LogQuery) -> Result<Records, Gone> {
        self.ask(|reply| Command::Log { query, reply }).await
    }

    /// A writer's grants and next counter.
    pub async fn capabilities(&self, writer: PublicKey) -> Result<Capabilities, Gone> {
        self.ask(|reply| Command::Capabilities { writer, reply })
            .await
    }

    /// The newest entry.
    pub async fn head(&self) -> Result<Head, Gone> {
        self.ask(|reply| Command::Head { reply }).await
    }

    async fn ask<T>(&self, command: impl FnOnce(oneshot::Sender<T>) -> Command) -> Result<T, Gone> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(command(reply)).await.map_err(|_| Gone)?;
        answer.await.map_err(|_| Gone)
    }
}

/// A started keeper.
pub struct Started {
    /// The way to ask it.
    pub keeper: Keeper,
    /// Its thread. It ends when every [`Keeper`] is dropped, or when the
    /// appender stops after a storage failure.
    pub thread: JoinHandle<()>,
    /// Cancelled when the thread ends.
    pub ended: CancellationToken,
    /// How many entries the log held when it was opened.
    pub entries: u64,
    /// How many bytes of an unfinished last record were dropped on
    /// opening. Such a record was never acknowledged.
    pub dropped: u64,
}

/// Open the log at `log`, replay it from genesis against `pin`, and start
/// the keeper's thread.
///
/// The log must already be started: `island-cli init` writes its first
/// entries. `build` is the git commit this broker was built from.
pub fn start(log: &Path, key: SigningKey, pin: PublicKey, build: &str) -> Result<Started, String> {
    let at = |error: &dyn std::fmt::Display| format!("{}: {error}", log.display());
    let storage = FileStorage::open(log).map_err(|error| at(&error))?;
    let (appender, records, recovery) =
        Appender::open_with_records(storage, SystemClock, key, pin).map_err(|error| at(&error))?;
    if appender.chain().next_seq() < 2 {
        return Err(at(&"not a started log; run `island-cli init` first"));
    }

    let mut shelf = Shelf {
        appender,
        records: Vec::new(),
        streams: BTreeMap::new(),
        build: build.to_owned(),
    };
    for record in records {
        shelf.add(record);
    }

    let (tx, mut rx) = mpsc::channel(QUEUE);
    let ended = CancellationToken::new();
    let signal = ended.clone();
    let thread = island_env::spawn("island-keeper", move || {
        while let Some(command) = rx.blocking_recv() {
            shelf.serve(command);
            // After a storage failure nothing more is served: whether the
            // last entry is durable is unknown until the log is opened
            // again, which a restart of the broker does.
            if shelf.appender.is_stopped() {
                break;
            }
        }
        signal.cancel();
    })
    .map_err(|error| format!("could not start the keeper's thread: {error}"))?;

    Ok(Started {
        keeper: Keeper { tx },
        thread,
        ended,
        entries: recovery.records,
        dropped: recovery.dropped,
    })
}

/// What the keeper's thread owns.
struct Shelf {
    appender: Appender<FileStorage, SystemClock>,
    /// Every record, by position.
    records: Vec<Record>,
    /// Per stream: the positions of its records, in order.
    streams: BTreeMap<StreamId, Vec<usize>>,
    build: String,
}

impl Shelf {
    fn add(&mut self, record: Record) {
        self.streams
            .entry(record.entry.submission.stream.clone())
            .or_default()
            .push(self.records.len());
        self.records.push(record);
    }

    /// Answer one request. A requester that has gone away is not an
    /// error: what it asked for was still done.
    fn serve(&mut self, command: Command) {
        match command {
            Command::Append {
                submission,
                sig_writer,
                reply,
            } => {
                let (answer, added) = self.appender.submit_keeping(*submission, sig_writer);
                if let Some(record) = added {
                    self.add(record);
                }
                let _ = reply.send(answer);
            }
            Command::Stream { request, reply } => {
                let positions = self
                    .streams
                    .get(&StreamId::from(request.stream.as_str()))
                    .map_or(&[][..], Vec::as_slice);
                // Positions are in log order, so the first one past
                // `after` is found by halving.
                let from = request.after.map_or(0, |after| {
                    positions.partition_point(|at| self.records[*at].entry.seq <= after)
                });
                let _ =
                    reply.send(self.page(&positions[from.min(positions.len())..], request.limit));
            }
            Command::Log { query, reply } => {
                // A record's position in the log is its index here.
                let all: Vec<usize> = (0..self.records.len()).collect();
                let from = query.after.map_or(0, |after| {
                    usize::try_from(after).map_or(usize::MAX, |a| a.saturating_add(1))
                });
                let _ = reply.send(self.page(&all[from.min(all.len())..], query.limit));
            }
            Command::Capabilities { writer, reply } => {
                let chain = self.appender.chain();
                let grants = chain
                    .state()
                    .grants
                    .iter()
                    .filter(|(_, held)| held.grant.holder == writer)
                    .map(|(id, held)| codec::grant_info(*id, held))
                    .collect();
                let _ = reply.send(Capabilities {
                    grants,
                    // Counters start at 1; 0 says the writer has none
                    // left, which takes 2^64 submissions.
                    next_counter: chain.next_counter(&writer).unwrap_or(0),
                });
            }
            Command::Head { reply } => {
                // `start` refuses a log without entries, so there is a head.
                if let Some(head) = self.appender.chain().head() {
                    let _ = reply.send(Head {
                        seq: head.seq,
                        hash: codec::b64(&head.hash),
                        build: self.build.clone(),
                        wire: WIRE_VERSION,
                    });
                }
            }
        }
    }

    /// The first `limit` of the records at `positions`, and whether that
    /// left any out.
    fn page(&self, positions: &[usize], limit: Option<u32>) -> Records {
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
        let taken = &positions[..positions.len().min(limit)];
        codec::records(
            taken.iter().map(|at| &self.records[*at]),
            taken.len() < positions.len(),
        )
    }
}
