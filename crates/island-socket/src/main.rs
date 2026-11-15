//! `island-socket serve`: the island socket (docs/slice-2.md, "The island
//! socket"). `island-socket call-digest`: a call's digest from its two
//! bodies, as the `access` entries count it.
//!
//! Exit codes: 0 on SIGTERM after the last flush, 1 on a failure at start
//! or when the socket's file is gone (a new agents' image replaced the
//! directory): systemd starts it again, and it binds afresh.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use island_client::{Broker, Writer};
use island_core::{SigningKey, public_key};
use island_env::keyfile;
use island_socket::access::{call_digest, flush};
use island_socket::server::{Peer, Shared, router};
use island_socket::{bind, config::Config, shared};

#[derive(Parser)]
#[command(
    name = "island-socket",
    version,
    about = "What a Vessel's agent may ask Island"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the socket.
    Serve {
        /// The config file.
        #[arg(long, default_value = "/etc/island-socket/config.toml")]
        config: PathBuf,
    },
    /// A call's digest, from the body the agent sent and the body it
    /// received, in hex.
    CallDigest {
        /// The request body.
        #[arg(long)]
        request: PathBuf,
        /// The response body.
        #[arg(long)]
        response: PathBuf,
    },
    /// An `access` entry's digest over calls' digests (hex, in order).
    BatchDigest {
        /// The calls' digests.
        #[arg(required = true)]
        calls: Vec<String>,
    },
    /// The callers a Warden config names, one line each: Vessel id, subject,
    /// role and slot.
    Vessels {
        /// Taso's Warden config.
        #[arg(long, default_value = "/etc/taso/warden.json")]
        warden_config: PathBuf,
    },
}

fn main() -> ExitCode {
    match Args::parse().command {
        Command::Vessels { warden_config } => {
            match island_socket::callers::Callers::load(&warden_config) {
                Ok(callers) => {
                    for caller in callers.all() {
                        println!(
                            "{} {} {} {}",
                            keyfile::hex(&caller.vessel),
                            caller.subject,
                            caller.role.name(),
                            caller.slot
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("island-socket: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::BatchDigest { calls } => {
            let Some(calls) = calls
                .iter()
                .map(|call| keyfile::unhex::<32>(call))
                .collect::<Option<Vec<_>>>()
            else {
                eprintln!("island-socket: a digest is 64 hex digits");
                return ExitCode::FAILURE;
            };
            println!(
                "{}",
                keyfile::hex(&island_socket::access::batch_digest(&calls))
            );
            ExitCode::SUCCESS
        }
        Command::CallDigest { request, response } => {
            let read = |path: &PathBuf| {
                std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
            };
            match read(&request).and_then(|req| Ok((req, read(&response)?))) {
                Ok((req, res)) => {
                    let hex: String = call_digest(&req, &res)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    println!("{hex}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("island-socket: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Serve { config } => {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("island-socket: no runtime: {error}");
                    return ExitCode::FAILURE;
                }
            };
            match runtime.block_on(serve(config)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("island-socket: {error}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

async fn serve(path: PathBuf) -> Result<(), String> {
    let config = Config::load(&path)?;
    let key = SigningKey::from_bytes(&keyfile::load(&config.key)?);
    let me = public_key(&key);
    let writer = Writer::new(key);
    let socket = config.socket.clone();
    let every = Duration::from_secs(config.access_every_secs);
    let broker_url = config.broker.clone();
    let shared = shared(config)?;
    let listener = bind(&socket)?;
    let inode = std::fs::metadata(&socket)
        .map_err(|error| format!("{}: {error}", socket.display()))?
        .ino();
    eprintln!(
        "island-socket: serving {} for {} caller(s); access entries every {}s to {broker_url}",
        socket.display(),
        shared.callers.lock().expect("not poisoned").0.all().len(),
        every.as_secs()
    );

    // The batched access entries.
    let flusher = {
        let shared = shared.clone();
        tokio::spawn(async move { flush_forever(shared, broker_url, writer, me, every).await })
    };

    // The socket's file: replaced with the image directory, it is gone
    // from where the Vessels look. Exit, and be started again.
    let watch = {
        let socket = socket.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                match std::fs::metadata(&socket) {
                    Ok(meta) if meta.ino() == inode => {}
                    _ => return format!("{} is gone or replaced", socket.display()),
                }
            }
        })
    };

    let app = router(shared.clone()).into_make_service_with_connect_info::<Peer>();
    let server = axum::serve(listener, app);
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| format!("signals: {error}"))?;
    let outcome = tokio::select! {
        result = server => result.map_err(|error| format!("serving: {error}")),
        gone = watch => Err(gone.unwrap_or_else(|_| "the watch ended".to_owned())),
        _ = term.recv() => Ok(()),
    };
    flusher.abort();
    // What is pending goes out before the end, if the broker answers.
    let shared_ref: &Shared = &shared;
    if !shared_ref.batches.lock().await.is_empty() {
        let config = &shared_ref.config;
        let key = SigningKey::from_bytes(&keyfile::load(&config.key)?);
        match Broker::connect(&config.broker).await {
            Ok(broker) => {
                if let Err(error) =
                    flush(&shared_ref.batches, &broker, &Writer::new(key), &me, true).await
                {
                    eprintln!("island-socket: the last access entries: {error}");
                }
            }
            Err(error) => eprintln!("island-socket: the last access entries: {error}"),
        }
    }
    outcome
}

async fn flush_forever(
    shared: Arc<Shared>,
    broker_url: String,
    writer: Writer,
    me: island_core::PublicKey,
    every: Duration,
) {
    let mut broker: Option<Broker> = None;
    // Quiet batches (task polls that found nothing) go out once an hour.
    let rounds_per_hour = (3600 / every.as_secs().max(1)).max(1);
    let mut round = 0u64;
    loop {
        tokio::time::sleep(every).await;
        round += 1;
        let all = round.is_multiple_of(rounds_per_hour);
        if !shared.batches.lock().await.due(all) {
            continue;
        }
        if broker.is_none() {
            match Broker::connect(&broker_url).await {
                Ok(connected) => broker = Some(connected),
                Err(error) => {
                    eprintln!("island-socket: the broker: {error}");
                    continue;
                }
            }
        }
        let connected = broker.as_ref().expect("connected above");
        match flush(&shared.batches, connected, &writer, &me, all).await {
            Ok(flushed) => {
                for refusal in flushed.refused {
                    eprintln!(
                        "island-socket: an access entry was refused, and dropped: {refusal:?}"
                    );
                }
                if !flushed.written.is_empty() {
                    eprintln!("island-socket: access entries {:?}", flushed.written);
                }
            }
            Err(error) => {
                eprintln!("island-socket: {error}; the entries wait for the next flush");
                broker = None;
            }
        }
    }
}
