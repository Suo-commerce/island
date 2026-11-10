//! Island command line (docs/spec.md, build steps 3 and 5).
//!
//! - `keygen` and `init` start a log.
//! - `grant` and `revoke` write root-signed authority entries to it.
//! - `verify`, `replay`, `tail` and `grants` read one. They never change
//!   the file, and print nothing from a log they could not verify from
//!   genesis against the pinned root key.
//!
//! Until the broker exists (build step 7), the writing commands append to
//! the log file directly, holding its lock; they need the appender key.
//!
//! Exit codes: 0 done; 1 failed; 3 nothing written yet, sign the printed
//! hash and run the command again (see `signer`).

mod keys;
mod show;
mod signer;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use island_core::{
    Body, Grant, GrantId, Kind, PublicKey, Revoke, StreamId, Submission, public_key,
};
use island_env::{FileStorage, OsRng, SystemClock};
use island_log::{Ack, Appender, Log, genesis};

use keys::hex;
use signer::RootArgs;

/// Why a command stopped without doing its work.
pub enum Stop {
    /// It failed. Exit code 1.
    Failed(String),
    /// Nothing is wrong and nothing was written: a hash has to be signed
    /// by hand first. Exit code 3.
    SignFirst(String),
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for Stop {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_owned())
    }
}

#[derive(Parser)]
#[command(
    name = "island-cli",
    version,
    about = "Start, verify, read and administer an Island log"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a new ed25519 key file (for development, and for the appender).
    Keygen {
        /// Where to write it. Never replaces a file.
        file: PathBuf,
    },
    /// Start a new log: entry 0 (genesis) and entry 1 (bootstrap).
    Init {
        /// The log file to create. Must not exist.
        #[arg(long)]
        log: PathBuf,
        /// The appender key file. Orders and seals every entry.
        #[arg(long)]
        appender_key: PathBuf,
        #[command(flatten)]
        root: RootArgs,
    },
    /// Grant a key the right to write some kinds to some streams, or
    /// narrow a grant it holds.
    Grant {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The appender key file.
        #[arg(long)]
        appender_key: PathBuf,
        /// The public key that receives the grant, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        holder: PublicKey,
        /// The streams it covers, separated by commas.
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        /// The kinds it may write there, separated by commas: verse,
        /// preference, policy.
        #[arg(long, value_delimiter = ',', value_parser = parse_kind)]
        rights: Vec<Kind>,
        /// The grant this one replaces, as entry.index. Scope and rights
        /// must then be subsets of it, for the same holder.
        #[arg(long)]
        narrows: Option<GrantId>,
        #[command(flatten)]
        root: RootArgs,
    },
    /// End a grant. It stops covering writes from the next entry on.
    Revoke {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The appender key file.
        #[arg(long)]
        appender_key: PathBuf,
        /// The grant to end, as entry.index.
        #[arg(long)]
        grant: GrantId,
        #[command(flatten)]
        root: RootArgs,
    },
    /// List every grant a verified log has held, with its id.
    Grants {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The pinned root public key, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        root: PublicKey,
    },
    /// Check a log from genesis: chain, hashes, signatures, counters, grants.
    Verify {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The pinned root public key, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        root: PublicKey,
    },
    /// Replay a log from genesis and print the state it arrives at.
    Replay {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The pinned root public key, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        root: PublicKey,
    },
    /// Print the newest entries of a verified log.
    Tail {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The pinned root public key, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        root: PublicKey,
        /// How many entries.
        #[arg(short = 'n', long, default_value_t = 10)]
        entries: usize,
    },
}

/// A kind by its name, as `--rights` takes it.
fn parse_kind(name: &str) -> Result<Kind, String> {
    Ok(match name {
        "policy" => Kind::Policy,
        "grant" => Kind::Grant,
        "revoke" => Kind::Revoke,
        "verse" => Kind::Verse,
        "preference" => Kind::Preference,
        "verdict" => Kind::Verdict,
        "genesis" => Kind::Genesis,
        "bootstrap" => Kind::Bootstrap,
        _ => {
            return Err(format!(
                "`{name}` is not a kind (verse, preference, policy)"
            ));
        }
    })
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Stop::Failed(error)) => {
            eprintln!("island-cli: {error}");
            ExitCode::FAILURE
        }
        Err(Stop::SignFirst(instructions)) => {
            print!("{instructions}");
            ExitCode::from(3)
        }
    }
}

fn run(command: Command) -> Result<(), Stop> {
    match command {
        Command::Keygen { file } => {
            let key = keys::generate(&file, &mut OsRng)?;
            println!("wrote   {}", file.display());
            println!("public  {}", hex(&public_key(&key)));
            Ok(())
        }
        Command::Init {
            log,
            appender_key,
            root,
        } => init(&log, &appender_key, &root),
        Command::Grant {
            log,
            appender_key,
            holder,
            scope,
            rights,
            narrows,
            root,
        } => {
            let grant = Grant {
                holder,
                scope: scope
                    .iter()
                    .map(|name| StreamId::from(name.as_str()))
                    .collect(),
                rights: BTreeSet::from_iter(rights),
                narrows,
            };
            let ack = write_as_root(&log, &appender_key, &root, Body::Grant(grant))?;
            println!("entry {}  grant {}", ack.seq, GrantId::new(ack.seq, 0));
            Ok(())
        }
        Command::Revoke {
            log,
            appender_key,
            grant,
            root,
        } => {
            let ack = write_as_root(&log, &appender_key, &root, Body::Revoke(Revoke { grant }))?;
            println!("entry {}  revoked grant {grant}", ack.seq);
            Ok(())
        }
        Command::Grants { log, root } => {
            print!("{}", show::grants(&read(&log, root)?.chain));
            Ok(())
        }
        Command::Verify { log, root } => {
            let verified = read(&log, root)?;
            let Some(head) = verified.chain.head() else {
                println!("ok: an empty log");
                return Ok(());
            };
            println!(
                "ok: {} entries verified from genesis",
                verified.records.len()
            );
            println!("head        entry {}, {}", head.seq, hex(&head.hash));
            println!("state hash  {}", hex(&verified.chain.state_hash()));
            match verified.chain.appender() {
                Some(appender) => println!("appender    {}", hex(&appender)),
                None => println!("appender    not declared yet: the log ends before entry 1"),
            }
            Ok(())
        }
        Command::Replay { log, root } => {
            let verified = read(&log, root)?;
            println!("{} entries replayed from genesis", verified.records.len());
            print!("{}", show::state(&verified.chain));
            Ok(())
        }
        Command::Tail { log, root, entries } => {
            let verified = read(&log, root)?;
            let skip = verified.records.len().saturating_sub(entries);
            for record in &verified.records[skip..] {
                println!("{}", show::entry(record));
            }
            Ok(())
        }
    }
}

/// Read and verify a log file. A torn tail is reported on stderr.
fn read(path: &Path, root: PublicKey) -> Result<Log, String> {
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

fn init(log: &Path, appender_key: &Path, root: &RootArgs) -> Result<(), Stop> {
    let signer = root.signer()?;
    let appender = keys::load(appender_key)?;
    let (pin, appender_public) = (signer.public(), public_key(&appender));
    if pin == appender_public {
        return Err("the appender key must not be the root key".into());
    }

    // Both entries are fixed by the two keys alone, so they can be signed
    // before the log exists. Nothing is created until they are.
    let entries = [
        ("genesis", genesis::genesis(pin)),
        ("bootstrap", genesis::bootstrap(pin, appender_public)),
    ];
    let submissions: Vec<Submission> = entries.iter().map(|(_, entry)| entry.clone()).collect();
    let signatures = signer.sign(&submissions)?;

    let storage =
        FileStorage::create(log).map_err(|error| format!("{}: {error}", log.display()))?;
    let (mut appender, _) = Appender::open(storage, SystemClock, appender, pin)
        .map_err(|error| format!("{}: {error}", log.display()))?;

    println!("log       {}", log.display());
    println!("root      {}   pin this in every replica", hex(&pin));
    println!("appender  {}", hex(&appender_public));
    for ((name, submission), signature) in entries.into_iter().zip(signatures) {
        let ack = appender
            .submit(submission, signature)
            .map_err(|error| format!("{name}: {error}"))?;
        println!("entry {}   {name:<9} {}", ack.seq, hex(&ack.hash));
    }
    Ok(())
}

/// Append one entry written by the root key to the authority stream.
///
/// The log is opened and locked first, so the root's counter in the
/// submission is the one the log expects. If the hash has yet to be signed
/// by hand, this stops before writing anything.
fn write_as_root(
    log: &Path,
    appender_key: &Path,
    root: &RootArgs,
    body: Body,
) -> Result<Ack, Stop> {
    let signer = root.signer()?;
    let pin = signer.public();
    let appender = keys::load(appender_key)?;
    let storage = FileStorage::open(log).map_err(|error| format!("{}: {error}", log.display()))?;
    let (mut appender, recovery) = Appender::open(storage, SystemClock, appender, pin)
        .map_err(|error| format!("{}: {error}", log.display()))?;
    if recovery.dropped > 0 {
        eprintln!(
            "island-cli: note: dropped {} bytes of an unfinished last record",
            recovery.dropped
        );
    }

    let counter = appender
        .chain()
        .next_counter(&pin)
        .ok_or("the root key has no counter left")?;
    let submission = Submission {
        stream: StreamId::from(StreamId::AUTHORITY),
        writer: pin,
        counter,
        body,
    };
    let signature = signer
        .sign(std::slice::from_ref(&submission))?
        .pop()
        .ok_or("no signature")?;
    Ok(appender
        .submit(submission, signature)
        .map_err(|error| error.to_string())?)
}
