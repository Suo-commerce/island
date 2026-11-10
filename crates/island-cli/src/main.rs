//! Island command line (docs/spec.md, build step 3).
//!
//! `keygen` and `init` start a log; `verify`, `replay` and `tail` read
//! one. The reading commands never change the file, and print nothing
//! from a log they could not verify from genesis against the pinned root
//! key.

mod keys;
mod show;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use island_core::{PublicKey, public_key};
use island_env::{FileStorage, OsRng, SystemClock};
use island_log::{Appender, Log, genesis};

use keys::hex;

#[derive(Parser)]
#[command(
    name = "island-cli",
    version,
    about = "Start, verify and read an Island log"
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
        /// The root key file. Signs both entries.
        #[arg(long)]
        root_key: PathBuf,
        /// The appender key file. Orders and seals every entry.
        #[arg(long)]
        appender_key: PathBuf,
    },
    /// Check a log from genesis: chain, hashes, both signatures, counters.
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

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("island-cli: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Keygen { file } => {
            let key = keys::generate(&file, &mut OsRng)?;
            println!("wrote   {}", file.display());
            println!("public  {}", hex(&public_key(&key)));
            Ok(())
        }
        Command::Init {
            log,
            root_key,
            appender_key,
        } => init(&log, &root_key, &appender_key),
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

fn init(log: &Path, root_key: &Path, appender_key: &Path) -> Result<(), String> {
    let root = keys::load(root_key)?;
    let appender = keys::load(appender_key)?;
    let (pin, appender_public) = (public_key(&root), public_key(&appender));
    if pin == appender_public {
        return Err("the appender key must not be the root key".to_owned());
    }

    let storage =
        FileStorage::create(log).map_err(|error| format!("{}: {error}", log.display()))?;
    let (mut appender, _) = Appender::open(storage, SystemClock, appender, pin)
        .map_err(|error| format!("{}: {error}", log.display()))?;

    println!("log       {}", log.display());
    println!("root      {}   pin this in every replica", hex(&pin));
    println!("appender  {}", hex(&appender_public));
    for (name, submission) in [
        ("genesis", genesis::genesis(pin)),
        ("bootstrap", genesis::bootstrap(pin, appender_public)),
    ] {
        let signature = submission
            .sign(&root)
            .ok_or("the root key does not match its own public key")?;
        let ack = appender
            .submit(submission, signature)
            .map_err(|error| format!("{name}: {error}"))?;
        println!("entry {}   {name:<9} {}", ack.seq, hex(&ack.hash));
    }
    Ok(())
}
