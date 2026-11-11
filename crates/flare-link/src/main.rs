//! `flare-link`: keep a verified replica of the Island log on this
//! machine, and see what it holds.
//!
//! - `pull`: bring the replica up to the broker's head, once or on a
//!   timer.
//! - `see`: pull, then show the replica: an overview, or one stream.
//!
//! Exit codes: 0 done; 1 failed, which includes a broker that served
//! something that does not verify (nothing of it was added); 2 usage.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use flare_core::{Overview, StreamView, hex, short};
use flare_link::replica::{self, Replica, ReplicaError};
use flare_link::{PullError, pull};
use island_client::Plain;
use island_core::PublicKey;

/// Keep a verified replica of the Island log, and see what it holds.
#[derive(Parser)]
#[command(name = "flare-link", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Where the log comes from and where the replica is.
#[derive(clap::Args)]
struct Source {
    /// The broker to pull from, for example http://127.0.0.1:4747.
    #[arg(long, value_name = "URL")]
    broker: String,
    /// The pinned root public key, in hex. Every record is verified
    /// against it from genesis.
    #[arg(long, value_parser = parse_public_key)]
    root: PublicKey,
    /// The replica file. Created on the first pull.
    #[arg(long, default_value_os_t = default_replica())]
    replica: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    /// Bring the replica up to the broker's head.
    Pull {
        #[command(flatten)]
        source: Source,
        /// Pull again every this many seconds, until stopped.
        #[arg(long, value_name = "SECONDS")]
        every: Option<u64>,
    },
    /// Pull, then show the replica: an overview, or the newest entries of
    /// one stream.
    See {
        #[command(flatten)]
        source: Source,
        /// The stream to show. Leave out for the overview.
        stream: Option<String>,
        /// How many entries of the stream.
        #[arg(short = 'n', long, default_value_t = 20)]
        entries: usize,
        /// Show the replica as it is, without pulling.
        #[arg(long)]
        offline: bool,
    },
}

fn parse_public_key(text: &str) -> Result<PublicKey, String> {
    let digits = text.as_bytes();
    if digits.len() != 64 {
        return Err("a public key is 64 hex digits".to_owned());
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&digits[2 * index..2 * index + 2])
            .map_err(|_| "a public key is 64 hex digits".to_owned())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| "a public key is 64 hex digits")?;
    }
    Ok(key)
}

/// `$XDG_DATA_HOME/flare/island.log`, or `~/.local/share/flare/island.log`.
fn default_replica() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    data.join("flare").join("island.log")
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("flare-link: no runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli.command)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("flare-link: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Pull { source, every } => {
            let broker = Plain::new(&source.broker).map_err(|error| error.to_string())?;
            let mut replica =
                Replica::open(&source.replica, source.root).map_err(|error| error.to_string())?;
            println!("replica  {}", replica.path().display());
            println!("root     {}", hex(&source.root));
            let Some(seconds) = every else {
                return once(&mut replica, &broker).await;
            };
            // On a timer: say only what changes, and keep going after a
            // failure; a broker that is down now may be up next time.
            let mut last_error = String::new();
            loop {
                match pull(&mut replica, &broker).await {
                    Ok(pulled) => {
                        last_error.clear();
                        if pulled.added > 0 {
                            println!("{}", said(pulled.added, pulled.head));
                        }
                    }
                    Err(error) => {
                        let error = error.to_string();
                        if error != last_error {
                            eprintln!("flare-link: {error}");
                            last_error = error;
                        }
                    }
                }
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(seconds.max(1))) => {}
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                }
            }
        }
        Command::See {
            source,
            stream,
            entries,
            offline,
        } => see(&source, stream.as_deref(), entries, offline).await,
    }
}

/// One pull, told.
async fn once(replica: &mut Replica, broker: &Plain) -> Result<(), String> {
    let pulled = pull(replica, broker)
        .await
        .map_err(|error| error.to_string())?;
    println!("{}", said(pulled.added, pulled.head));
    Ok(())
}

fn said(added: u64, head: Option<(u64, [u8; 32])>) -> String {
    let at = head.map_or("an empty log".to_owned(), |(seq, hash)| {
        format!("head entry {seq}, {}", short(&hash))
    });
    match added {
        0 => format!("up to date: {at}"),
        n => format!("pulled {}, verified: {at}", flare_core::entries(n)),
    }
}

async fn see(
    source: &Source,
    stream: Option<&str>,
    entries: usize,
    offline: bool,
) -> Result<(), String> {
    // Pull first, unless told not to. If another flare-link is pulling
    // into the replica, read it as it stands.
    let mut failed = None;
    let snapshot = match Replica::open(&source.replica, source.root) {
        Ok(mut replica) => {
            if !offline {
                let broker = Plain::new(&source.broker).map_err(|error| error.to_string())?;
                match pull(&mut replica, &broker).await {
                    Ok(_) => {}
                    Err(PullError::Broker(error)) => {
                        eprintln!("flare-link: not pulled: {error}");
                    }
                    Err(error) => failed = Some(error.to_string()),
                }
            }
            replica::Snapshot {
                records: replica.records().to_vec(),
                chain: replica.chain().clone(),
            }
        }
        Err(ReplicaError::Busy(_)) => {
            eprintln!("flare-link: another flare-link is pulling; showing the replica as it is");
            replica::read(&source.replica, source.root).map_err(|error| error.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };

    match stream {
        None => print!("{}", Overview::of(&snapshot.records, &snapshot.chain)),
        Some(name) => print!("{}", StreamView::of(&snapshot.records, name, entries)),
    }
    // What is shown is the verified replica either way; a broker that
    // served something false still makes this a failure.
    match failed {
        Some(error) => Err(format!("the broker is not to be trusted: {error}")),
        None => Ok(()),
    }
}
