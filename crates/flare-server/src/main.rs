//! `flare-server`: the shared Flare sessions.
//!
//! Exit codes: 0 after a requested stop; 1 if it could not start.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use flare_server::{Config, Server, router};
use island_core::PublicKey;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};

/// Serve the shared Flare sessions at /session (WebSocket).
#[derive(Parser)]
#[command(name = "flare-server", version)]
struct Args {
    /// The broker to read the log from and hand proposals to.
    #[arg(long, value_name = "URL")]
    broker: String,
    /// The pinned root public key, in hex.
    #[arg(long, value_parser = parse_public_key)]
    root: PublicKey,
    /// Where the sessions and the server's replica are kept.
    #[arg(long, default_value_os_t = default_data())]
    data: PathBuf,
    /// The address and port to listen on.
    #[arg(long, default_value = "127.0.0.1:4749")]
    listen: String,
    /// A host name or address clients reach the server by, besides
    /// loopback and the address it listens on. May be given more than
    /// once.
    #[arg(long, value_name = "HOST")]
    allow_host: Vec<String>,
}

fn parse_public_key(text: &str) -> Result<PublicKey, String> {
    let error = || "a public key is 64 hex digits".to_owned();
    if text.len() != 64 || !text.is_ascii() {
        return Err(error());
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * index..2 * index + 2], 16).map_err(|_| error())?;
    }
    Ok(key)
}

/// `$XDG_DATA_HOME/flare/server`, or `~/.local/share/flare/server`.
fn default_data() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flare")
        .join("server")
}

/// Microseconds since the epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_micros()).unwrap_or(u64::MAX)
        })
}

fn main() -> ExitCode {
    let args = Args::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("flare-server: no runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("flare-server: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let listener = TcpListener::bind(&args.listen)
        .await
        .map_err(|error| format!("{}: {error}", args.listen))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("{}: {error}", args.listen))?;
    if address.ip().is_unspecified() {
        return Err(format!(
            "{}: flare-server does not listen on every interface; give one address: \
             127.0.0.1, or this machine's tailnet address (`tailscale ip -4`)",
            args.listen
        ));
    }
    let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
        .map(str::to_owned)
        .to_vec();
    for host in std::iter::once(address.ip().to_string()).chain(args.allow_host) {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    let server = Server::new(Config {
        broker: args.broker.clone(),
        root: args.root,
        data: args.data.clone(),
        hosts: hosts.clone(),
        clock: now,
    })?;

    println!("flare-server  ws://{address}/session");
    println!("data          {}", args.data.display());
    println!("broker        {}", args.broker);
    println!("answers to    {}", hosts.join(", "));

    let mut terminate =
        signal(SignalKind::terminate()).map_err(|error| format!("no signal handler: {error}"))?;
    let stopping = server.clone();
    axum::serve(listener, router(server))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            stopping.stop();
        })
        .await
        .map_err(|error| format!("serving stopped: {error}"))?;
    println!("flare-server  stopped");
    Ok(())
}
