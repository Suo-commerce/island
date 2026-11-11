//! `island-broker`: serve one Island log.
//!
//! Exit codes: 0 after a requested stop; 1 if it could not start, or if
//! the appender stopped after a storage failure. Start it again after
//! either: opening the log is the recovery.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use island_broker::{DEFAULT_LISTEN, http, keeper};
use island_core::{PublicKey, SigningKey, public_key};
use island_env::keyfile;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

/// The git commit this build was made from. See `build.rs`.
const BUILD: &str = env!("ISLAND_BUILD_COMMIT");

/// Serve one Island log: the MCP tools at /mcp, and GET /log and /head.
#[derive(Parser)]
#[command(name = "island-broker", version)]
struct Args {
    /// The log file. It must be started already (`island-cli init`).
    #[arg(long)]
    log: PathBuf,
    /// The appender key file.
    #[arg(long)]
    appender_key: PathBuf,
    /// The pinned root public key, in hex.
    #[arg(long, value_parser = parse_public_key)]
    root: PublicKey,
    /// The address and port to listen on.
    #[arg(long, default_value = DEFAULT_LISTEN)]
    listen: String,
    /// A host name or address clients reach the broker by, besides
    /// loopback and the address it listens on: its tailnet name, for
    /// example. May be given more than once.
    #[arg(long, value_name = "HOST")]
    allow_host: Vec<String>,
}

fn parse_public_key(text: &str) -> Result<PublicKey, String> {
    keyfile::unhex(text).ok_or_else(|| "a public key is 64 hex digits".to_owned())
}

fn main() -> ExitCode {
    let args = Args::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("island-broker: no runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("island-broker: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let key = SigningKey::from_bytes(&keyfile::load(&args.appender_key)?);
    let appender = public_key(&key);
    let started = keeper::start(&args.log, key, args.root, BUILD)?;
    if started.dropped > 0 {
        eprintln!(
            "island-broker: note: dropped {} bytes of an unfinished last record",
            started.dropped
        );
    }

    let listener = TcpListener::bind(&args.listen)
        .await
        .map_err(|error| format!("{}: {error}", args.listen))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("{}: {error}", args.listen))?;
    // Reads are open to whoever reaches the broker, so it listens on one
    // address: loopback, or the server's tailnet address. Every interface
    // at once would put the log on whatever network the machine is on.
    if address.ip().is_unspecified() {
        return Err(format!(
            "{}: the broker does not listen on every interface; give one address: \
             127.0.0.1, or this machine's tailnet address (`tailscale ip -4`)",
            args.listen
        ));
    }

    // Loopback, the address listened on, and what was asked for.
    let mut hosts: Vec<String> = http::LOOPBACK.map(str::to_owned).to_vec();
    let listened = (!address.ip().is_unspecified()).then(|| address.ip().to_string());
    for host in listened.into_iter().chain(args.allow_host) {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }

    let shutdown = CancellationToken::new();
    let router = http::router(started.keeper, hosts.clone(), &shutdown);

    println!("island-broker  http://{address}");
    println!(
        "log            {} ({} entries)",
        args.log.display(),
        started.entries
    );
    println!("root           {}", keyfile::hex(&args.root));
    println!("appender       {}", keyfile::hex(&appender));
    println!("build          {BUILD}");
    println!("answers to     {}", hosts.join(", "));

    // Stop on Ctrl-C, on SIGTERM, or when the keeper has stopped itself.
    let mut terminate =
        signal(SignalKind::terminate()).map_err(|error| format!("no signal handler: {error}"))?;
    let ended = started.ended.clone();
    let stop = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
            () = ended.cancelled() => {}
        }
        stop.cancel();
    });

    let stopped_itself = started.ended.clone();
    http::serve(listener, router, shutdown)
        .await
        .map_err(|error| format!("serving stopped: {error}"))?;

    // The routes are gone, and with them every way to ask the keeper, so
    // its thread ends; wait for it, so the log is closed when this returns.
    let failed = stopped_itself.is_cancelled();
    started
        .thread
        .join()
        .map_err(|_| "the keeper's thread panicked".to_owned())?;
    if failed {
        return Err(
            "the appender stopped after a storage failure; start the broker again to recover"
                .to_owned(),
        );
    }
    println!("island-broker  stopped");
    Ok(())
}
