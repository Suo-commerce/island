//! `flare-server`: the shared Flare sessions.
//!
//! Exit codes: 0 after a requested stop; 1 if it could not start.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use flare_server::{AgentPeers, Config, Peer, Server, agent_router, router, tls, tls_router};
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
    /// The session the agent listens in.
    #[arg(long, default_value = "main")]
    agent_session: String,
    /// Serve the agent's side on this Unix socket: tasks for the island
    /// socket, replies from island-sender.
    #[arg(long, value_name = "PATH")]
    agent_socket: Option<PathBuf>,
    /// Serve the face from this directory: the page and its files, and
    /// /log for it to read the log by. That page alone may then attach
    /// to a session; without this, no web page may.
    #[arg(long, value_name = "DIR")]
    face: Option<PathBuf>,
    /// Serve the same over TLS on this address as well, for the page:
    /// a browser gives a page protected keys only over https. Needs
    /// --tls-cert and --tls-key, and the certificate's name as --allow-host.
    #[arg(long, value_name = "ADDRESS", requires_all = ["tls_cert", "tls_key"])]
    tls_listen: Option<String>,
    /// The certificate and the chain above it, as a PEM file. Read again
    /// when it changes: a renewal needs no restart.
    #[arg(long, value_name = "FILE", requires = "tls_listen")]
    tls_cert: Option<PathBuf>,
    /// The certificate's private key, as a PEM file.
    #[arg(long, value_name = "FILE", requires = "tls_listen")]
    tls_key: Option<PathBuf>,
    /// The user that may take tasks: the island socket's.
    #[arg(long, value_name = "USER", default_value = "island-socket")]
    task_reader: String,
    /// The user that may deliver the agent's replies: island-sender's.
    #[arg(long, value_name = "USER", default_value = "root")]
    reply_sender: String,
}

/// A user's uid, from /etc/passwd.
fn uid_of(user: &str) -> Result<u32, String> {
    if let Ok(uid) = user.parse() {
        return Ok(uid);
    }
    let passwd =
        std::fs::read_to_string("/etc/passwd").map_err(|error| format!("/etc/passwd: {error}"))?;
    passwd
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.next() == Some(user))
                .then(|| fields.nth(1)?.parse().ok())
                .flatten()
        })
        .ok_or_else(|| format!("no user {user} in /etc/passwd"))
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
             127.0.0.1, or this machine's WireGuard address",
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
    if let Some(face) = &args.face
        && !face.join("index.html").is_file()
    {
        return Err(format!(
            "{}: no index.html there; --face names the directory of the built page",
            face.display()
        ));
    }
    let server = Server::new(Config {
        broker: args.broker.clone(),
        root: args.root,
        data: args.data.clone(),
        hosts: hosts.clone(),
        clock: now,
        agent_session: args.agent_session.clone(),
        face: args.face.clone(),
    })?;
    let agent = match &args.agent_socket {
        None => None,
        Some(path) => {
            let peers = AgentPeers {
                tasks: uid_of(&args.task_reader)?,
                replies: uid_of(&args.reply_sender)?,
            };
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", path.display())),
            }
            let listener = tokio::net::UnixListener::bind(path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            // Anyone may connect; the peer's uid decides what it gets.
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
                .map_err(|error| format!("{}: {error}", path.display()))?;
            Some((listener, peers))
        }
    };

    println!("flare-server  ws://{address}/session");
    println!("data          {}", args.data.display());
    println!("broker        {}", args.broker);
    println!("answers to    {}", hosts.join(", "));
    println!("agent         session {}", args.agent_session);
    match &args.face {
        Some(face) => println!("face          http://{address}/ from {}", face.display()),
        None => println!("face          none; no web page may attach"),
    }
    if let (Some(path), Some((_, peers))) = (&args.agent_socket, &agent) {
        println!(
            "agent socket  {} (tasks: uid {}, replies: uid {})",
            path.display(),
            peers.tasks,
            peers.replies
        );
    }
    if let Some((listener, peers)) = agent {
        let app = agent_router(server.clone(), peers).into_make_service_with_connect_info::<Peer>();
        let stopping = server.clone();
        tokio::spawn(async move {
            let mut stopped = stopping.stopped();
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.wait_for(|stop| *stop).await;
                })
                .await;
        });
    }

    // The page over https, beside the plain listener the terminals use.
    if let (Some(listen), Some(cert), Some(key)) = (&args.tls_listen, &args.tls_cert, &args.tls_key)
    {
        if args.face.is_none() {
            return Err("--tls-listen serves the page: it needs --face".to_owned());
        }
        let tcp = TcpListener::bind(listen)
            .await
            .map_err(|error| format!("{listen}: {error}"))?;
        let secure = tls::Listener::new(
            tcp,
            tls::Files {
                cert: cert.clone(),
                key: key.clone(),
            },
        )?;
        println!("face, https   {listen} with {}", cert.display());
        let app = tls_router(server.clone());
        let stopping = server.clone();
        tokio::spawn(async move {
            let mut stopped = stopping.stopped();
            let _ = axum::serve(secure, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.wait_for(|stop| *stop).await;
                })
                .await;
        });
    }

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
