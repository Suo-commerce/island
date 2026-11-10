//! Where a command finds the log: in a file it opens itself, or behind a
//! running broker.
//!
//! Either way the command works on a log it has verified from genesis
//! against the pinned root key. Through a broker that means pulling every
//! record and replaying it here; the broker's word is taken for nothing
//! but which records there are.

use std::path::{Path, PathBuf};

use island_client::wire::Code;
use island_client::{Broker, wire};
use island_core::{PublicKey, Record, Signature, Submission};
use island_env::{FileStorage, SystemClock};
use island_log::{Ack, Appender, Chain, Log, Rejected, replay};
use tokio::runtime::Runtime;

use crate::keys::{self, hex};
use crate::show;

/// The log as a writing command holds it when it opens the file itself:
/// locked and replayed.
pub type Open = Appender<FileStorage, SystemClock>;

/// The log a reading command reads.
#[derive(clap::Args)]
pub struct ReadFrom {
    /// The log file.
    #[arg(long, required_unless_present = "broker", conflicts_with = "broker")]
    pub log: Option<PathBuf>,
    /// A running broker to read the log from instead of a file, for
    /// example http://127.0.0.1:4747.
    #[arg(long, value_name = "URL")]
    pub broker: Option<String>,
}

/// The log a writing command writes to.
#[derive(clap::Args)]
pub struct WriteTo {
    /// The log file, to append to it directly. No broker may be serving
    /// it then, and the appender key is needed.
    #[arg(long, required_unless_present = "broker", conflicts_with = "broker")]
    pub log: Option<PathBuf>,
    /// The appender key file, with --log.
    #[arg(long, requires = "log", conflicts_with = "broker")]
    pub appender_key: Option<PathBuf>,
    /// A running broker to write through instead, for example
    /// http://127.0.0.1:4747.
    #[arg(long, value_name = "URL")]
    pub broker: Option<String>,
}

impl WriteTo {
    /// The same log, for reading.
    pub fn for_reading(&self) -> ReadFrom {
        ReadFrom {
            log: self.log.clone(),
            broker: self.broker.clone(),
        }
    }
}

/// A runtime for the calls to a broker. One thread: a command does one
/// thing at a time.
pub fn runtime() -> Result<Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("no runtime: {error}"))
}

/// Read and verify a log, from a file or from a broker.
pub fn read(source: &ReadFrom, root: PublicKey) -> Result<Log, String> {
    match (&source.log, &source.broker) {
        (_, Some(url)) => {
            let runtime = runtime()?;
            let (records, chain) = runtime.block_on(async {
                let broker = connect(url).await?;
                let pulled = pull(&broker, url, root).await;
                broker.close().await;
                pulled
            })?;
            Ok(Log {
                records,
                chain,
                torn: 0,
            })
        }
        (Some(path), None) => read_file(path, root),
        (None, None) => Err("give --log or --broker".to_owned()),
    }
}

/// Read and verify a log file. A torn tail is reported on stderr.
fn read_file(path: &Path, root: PublicKey) -> Result<Log, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let log =
        island_log::read(&bytes, root).map_err(|error| format!("{}: {error}", path.display()))?;
    if log.torn > 0 {
        eprintln!(
            "island-cli: note: {} bytes of an unfinished last record are not part of the log",
            log.torn
        );
    }
    Ok(log)
}

async fn connect(url: &str) -> Result<Broker, String> {
    Broker::connect(url)
        .await
        .map_err(|error| error.to_string())
}

/// Pull every record from a broker and replay them from genesis.
async fn pull(broker: &Broker, url: &str, root: PublicKey) -> Result<(Vec<Record>, Chain), String> {
    let records = broker
        .whole_log()
        .await
        .map_err(|error| error.to_string())?;
    let chain = replay(root, &records)
        .map_err(|error| format!("{url}: the broker's log does not verify: {error}"))?;
    Ok((records, chain))
}

/// A broker, and the log as pulled from it and verified.
pub struct Remote {
    /// Runs the calls.
    runtime: Runtime,
    /// The connection.
    broker: Broker,
    /// The pulled log's chain.
    chain: Chain,
    /// The git commit the broker was built from.
    build: String,
}

/// A log a writing command has open.
pub enum Via {
    /// The file itself, locked.
    File(Box<Open>),
    /// A broker.
    Broker(Box<Remote>),
}

impl Via {
    /// Open the log for writing, verified from genesis against `pin`.
    pub fn open(target: &WriteTo, pin: PublicKey) -> Result<Self, String> {
        if let Some(url) = &target.broker {
            let runtime = runtime()?;
            let (broker, chain, build) = runtime.block_on(async {
                let broker = connect(url).await?;
                let (_, chain) = pull(&broker, url, pin).await?;
                let head = broker.head().await.map_err(|error| error.to_string())?;
                if head.wire != wire::WIRE_VERSION {
                    return Err(format!(
                        "{url}: the broker speaks wire version {}, this command version {}",
                        head.wire,
                        wire::WIRE_VERSION
                    ));
                }
                Ok((broker, chain, head.build))
            })?;
            return Ok(Self::Broker(Box::new(Remote {
                runtime,
                broker,
                chain,
                build,
            })));
        }
        let (Some(log), Some(appender_key)) = (&target.log, &target.appender_key) else {
            return Err("to write to a log file directly, give --appender-key too; \
                 or write through a broker with --broker"
                .to_owned());
        };
        let appender = keys::load(appender_key)?;
        let storage =
            FileStorage::open(log).map_err(|error| format!("{}: {error}", log.display()))?;
        let (appender, recovery) = Appender::open(storage, SystemClock, appender, pin)
            .map_err(|error| format!("{}: {error}", log.display()))?;
        if recovery.dropped > 0 {
            eprintln!(
                "island-cli: note: dropped {} bytes of an unfinished last record",
                recovery.dropped
            );
        }
        Ok(Self::File(Box::new(appender)))
    }

    /// The log's chain as this command verified it.
    pub fn chain(&self) -> &Chain {
        match self {
            Self::File(appender) => appender.chain(),
            Self::Broker(remote) => &remote.chain,
        }
    }

    /// The build that will judge a submission: this one when the file is
    /// written directly, the broker's otherwise. A policy names it.
    pub fn evaluator(&self) -> &str {
        match self {
            Self::File(_) => crate::BUILD,
            Self::Broker(remote) => &remote.build,
        }
    }

    /// Hand in one signed submission. The error is worded for the person
    /// at the terminal; a gate refusal names the verdict, and the hash an
    /// override request needs.
    pub fn submit(&mut self, submission: Submission, sig_writer: Signature) -> Result<Ack, String> {
        let refused_by_gate = |seq: u64, reason: &str| {
            format!(
                "{reason}\nverdict at entry {seq}; the refused submission is {}",
                hex(&submission.hash())
            )
        };
        match self {
            Self::File(appender) => {
                appender
                    .submit(submission.clone(), sig_writer)
                    .map_err(|error| match error {
                        Rejected::Refused { seq, reason, .. } => refused_by_gate(
                            seq,
                            &format!("refused by the gate: {}", show::refusal(&reason)),
                        ),
                        other => other.to_string(),
                    })
            }
            Self::Broker(remote) => {
                let answer = remote
                    .runtime
                    .block_on(remote.broker.append(&submission, &sig_writer))
                    .map_err(|error| {
                        format!(
                            "{error}\nnothing is known about the submission; run the command again"
                        )
                    })?;
                match answer {
                    Ok((seq, hash)) => Ok(Ack { seq, hash }),
                    Err(refusal) => Err(match (refusal.code, &refusal.verdict) {
                        (Code::Gate, Some(verdict)) => {
                            refused_by_gate(verdict.seq, &refusal.reason)
                        }
                        _ => refusal.reason,
                    }),
                }
            }
        }
    }

    /// Done with the log: close the connection, or release the lock.
    pub fn close(self) {
        if let Self::Broker(remote) = self {
            let Remote {
                runtime, broker, ..
            } = *remote;
            runtime.block_on(broker.close());
        }
    }
}
