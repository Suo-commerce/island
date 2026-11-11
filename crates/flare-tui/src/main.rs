//! `flare-tui`: a terminal on a shared Flare session.
//!
//! Type commands (`help` lists them); every event of the session is
//! printed as it happens, whoever caused it. Verses are signed here, with
//! this device's key, and the server only relays them. What `see` shows
//! is checked against this machine's own verified replica.
//!
//! Exit codes: 0 after `quit` or the end of input; 1 if the session could
//! not be attached or was closed by the server.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use flare_core::session::{
    ClientMessage, Event, EventKind, ProposeArgs, SeeArgs, ServerMessage, Verb, VerbCall,
    VerbResult,
};
use flare_core::{hex, short};
use flare_link::replica::{self, Replica, ReplicaError};
use flare_link::{PullError, pull};
use flare_tui::{HELP, Typed, parse, show};
use futures_util::{SinkExt, StreamExt};
use island_client::{Broker, Plain, Signed, Writer, codec};
use island_core::{PublicKey, SigningKey};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

/// A terminal on a shared Flare session.
#[derive(Parser)]
#[command(name = "flare-tui", version)]
struct Args {
    /// The session server.
    #[arg(
        long,
        value_name = "URL",
        default_value = "ws://127.0.0.1:4749/session"
    )]
    server: String,
    /// The session to attach to.
    #[arg(long, default_value = "main")]
    session: String,
    /// What this terminal is called in the session.
    #[arg(long, default_value = "terminal")]
    client: String,
    /// Attach after this event; left out, the newest 20 are shown first.
    #[arg(long, value_name = "SEQ")]
    after: Option<u64>,
    /// This device's key file, to sign verses with. Without it the
    /// terminal can see but not propose.
    #[arg(long)]
    key: Option<PathBuf>,
    /// The stream verses go to unless one is named.
    #[arg(long, default_value = "island")]
    stream: String,
    /// The broker: for this key's counter, and for this machine's replica.
    #[arg(long, value_name = "URL", default_value = "http://127.0.0.1:4747")]
    broker: String,
    /// The pinned root public key, in hex.
    #[arg(long, value_parser = parse_public_key)]
    root: PublicKey,
    /// This machine's replica, which `see` is checked against.
    #[arg(long, default_value_os_t = default_replica())]
    replica: PathBuf,
}

fn unhex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * index..2 * index + 2], 16).ok()?;
    }
    Some(out)
}

fn parse_public_key(text: &str) -> Result<PublicKey, String> {
    unhex32(text).ok_or_else(|| "a public key is 64 hex digits".to_owned())
}

fn default_replica() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flare")
        .join("island.log")
}

fn load_key(path: &Path) -> Result<SigningKey, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let seed = unhex32(text.trim())
        .ok_or_else(|| format!("{}: a key file holds 64 hex digits", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The commands this terminal sent that have no result yet, and whether
/// the events from before the attach have all been shown.
#[derive(Default)]
struct Waiting {
    ids: std::sync::Mutex<BTreeSet<String>>,
    /// The proposals among them. A key's next counter is known only once
    /// its last proposal is answered.
    proposals: std::sync::Mutex<BTreeSet<String>>,
    answered: Notify,
    replayed: std::sync::atomic::AtomicBool,
}

impl Waiting {
    fn add(&self, id: &str) {
        if let Ok(mut ids) = self.ids.lock() {
            ids.insert(id.to_owned());
        }
    }

    fn proposing(&self, id: &str) {
        if let Ok(mut proposals) = self.proposals.lock() {
            proposals.insert(id.to_owned());
        }
    }

    fn proposals_answered(&self) -> bool {
        self.proposals
            .lock()
            .map_or(true, |proposals| proposals.is_empty())
    }

    /// Wait until every proposal of this terminal is answered, for up to
    /// half a minute.
    async fn settle(&self) {
        let patience = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(patience);
        while !self.proposals_answered() {
            tokio::select! {
                () = self.answered.notified() => {}
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
                () = &mut patience => return,
            }
        }
    }

    fn done(&self, id: &str) {
        if let Ok(mut ids) = self.ids.lock() {
            ids.remove(id);
        }
        if let Ok(mut proposals) = self.proposals.lock() {
            proposals.remove(id);
        }
        self.answered.notify_waiters();
    }

    fn empty(&self) -> bool {
        self.ids.lock().map_or(true, |ids| ids.is_empty())
            && self.replayed.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn replay_done(&self) {
        self.replayed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.answered.notify_waiters();
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("flare-tui: no runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("flare-tui: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let writer = args
        .key
        .as_deref()
        .map(load_key)
        .transpose()?
        .map(Writer::new);
    let (socket, _) = tokio_tungstenite::connect_async(args.server.as_str())
        .await
        .map_err(|error| format!("{}: {error}", args.server))?;
    let (mut outgoing, mut incoming) = socket.split();
    let attach = ClientMessage::Attach {
        session: args.session.clone(),
        after: args.after,
        client: args.client.clone(),
    };
    send(&mut outgoing, &attach).await?;

    let waiting = Arc::new(Waiting::default());
    let checker = Arc::new(Checker {
        replica: args.replica.clone(),
        root: args.root,
        plain: Plain::new(&args.broker).map_err(|error| error.to_string())?,
    });

    // Everything the server sends, printed as it comes.
    let after = args.after;
    let printer = {
        let waiting = waiting.clone();
        let checker = checker.clone();
        tokio::spawn(async move {
            let mut verbs: BTreeMap<String, Verb> = BTreeMap::new();
            // The last event from before the attach, once it is known.
            let mut last_replayed: Option<u64> = None;
            while let Some(message) = incoming.next().await {
                let text = match message {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::Attached { session, next, .. }) => {
                        println!("attached to session {session}: {next} events so far");
                        let nothing_to_replay =
                            next == 0 || after.is_some_and(|after| after.saturating_add(1) >= next);
                        if nothing_to_replay {
                            waiting.replay_done();
                        } else {
                            last_replayed = Some(next - 1);
                        }
                    }
                    Ok(ServerMessage::Event(event)) => {
                        println!("{}", show(&event));
                        if last_replayed.is_some_and(|last| event.seq >= last) {
                            last_replayed = None;
                            waiting.replay_done();
                        }
                        if let Some(line) = follow(&event, &mut verbs, &checker).await {
                            println!("{line}");
                        }
                        if let Some(id) = command_id(&event) {
                            waiting.done(&id);
                        }
                    }
                    Ok(ServerMessage::Refused { reason }) => {
                        eprintln!("flare-tui: the server says: {reason}");
                        return false;
                    }
                    Err(error) => {
                        eprintln!("flare-tui: a message this terminal does not read: {error}")
                    }
                }
            }
            false
        })
    };

    // Commands, typed.
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut broker: Option<Broker> = None;
    let mut last: Option<Signed> = None;
    let mut count = 0u64;
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line.map_err(|error| error.to_string())?,
            ended = async { while !printer.is_finished() { tokio::time::sleep(Duration::from_millis(100)).await; } } => {
                let () = ended;
                return Err("the server closed the session".to_owned());
            }
        };
        let Some(line) = line else { break };
        let typed = match parse(&line, &args.stream) {
            Ok(typed) => typed,
            Err(error) => {
                eprintln!("flare-tui: {error}");
                continue;
            }
        };
        count += 1;
        let id = format!("{}-{count}", args.client);
        let (verb, args_value) = match typed {
            Typed::Nothing => continue,
            Typed::Help => {
                print!("{HELP}");
                continue;
            }
            Typed::Quit => break,
            Typed::See { stream, entries } => {
                let see = SeeArgs { stream, entries };
                (
                    Verb::See,
                    serde_json::to_value(see).map_err(|error| error.to_string())?,
                )
            }
            Typed::Verse { stream, verse } => {
                let Some(writer) = &writer else {
                    eprintln!("flare-tui: no key to sign with; start with --key");
                    continue;
                };
                // The counter after the last proposal, once that one is
                // answered: written or refused, it is settled either way.
                waiting.settle().await;
                let counter = match next_counter(&mut broker, &args.broker, writer).await {
                    Ok(counter) => counter,
                    Err(error) => {
                        eprintln!("flare-tui: not proposed: {error}");
                        continue;
                    }
                };
                let signed = writer.verse(&stream, counter, verse);
                let value = proposal(&signed)?;
                last = Some(signed);
                (Verb::Propose, value)
            }
            Typed::Again => match &last {
                Some(signed) => (Verb::Propose, proposal(signed)?),
                None => {
                    eprintln!("flare-tui: nothing proposed yet");
                    continue;
                }
            },
        };
        waiting.add(&id);
        if verb == Verb::Propose {
            waiting.proposing(&id);
        }
        let command = ClientMessage::Command {
            id,
            verb,
            args: args_value,
        };
        send(&mut outgoing, &command).await?;
    }

    // Wait for what was asked, then leave; the session goes on.
    let patience = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(patience);
    while !waiting.empty() && !printer.is_finished() {
        tokio::select! {
            () = waiting.answered.notified() => {}
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
            () = &mut patience => {
                eprintln!("flare-tui: leaving without every answer");
                break;
            }
        }
    }
    let _ = outgoing.send(Message::Close(None)).await;
    if let Some(broker) = broker {
        broker.close().await;
    }
    Ok(())
}

async fn send<S>(outgoing: &mut S, message: &ClientMessage) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let text = serde_json::to_string(message).map_err(|error| error.to_string())?;
    outgoing
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| format!("the session is gone: {error}"))
}

fn proposal(signed: &Signed) -> Result<serde_json::Value, String> {
    let request = codec::append_request(&signed.submission, &signed.sig_writer);
    serde_json::to_value(ProposeArgs {
        submission: request.submission,
        sig_writer: request.sig_writer,
    })
    .map_err(|error| error.to_string())
}

/// This key's next counter, from the broker.
async fn next_counter(
    broker: &mut Option<Broker>,
    url: &str,
    writer: &Writer,
) -> Result<u64, String> {
    for _ in 0..2 {
        let connection = match broker.take() {
            Some(connection) => connection,
            None => Broker::connect(url)
                .await
                .map_err(|error| error.to_string())?,
        };
        match connection.capabilities(&writer.public()).await {
            Ok(mine) => {
                *broker = Some(connection);
                return Ok(mine.next_counter);
            }
            Err(island_client::Error::Unreachable(_)) => connection.close().await,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(format!("{url}: the broker cannot be reached"))
}

/// The command id an event answers, if it is a result or an error.
fn command_id(event: &Event) -> Option<String> {
    match event.kind {
        EventKind::VerbResult => serde_json::from_value::<VerbResult>(event.payload.clone())
            .ok()
            .map(|result| result.id),
        EventKind::Error => event.payload.get("id")?.as_str().map(str::to_owned),
        _ => None,
    }
}

/// What to add after an event: for the result of a `see`, whether this
/// machine's replica holds the same entry at the head it was made at.
async fn follow(
    event: &Event,
    verbs: &mut BTreeMap<String, Verb>,
    checker: &Checker,
) -> Option<String> {
    match event.kind {
        EventKind::VerbCall => {
            let call: VerbCall = serde_json::from_value(event.payload.clone()).ok()?;
            verbs.insert(call.id, call.verb);
            None
        }
        EventKind::VerbResult => {
            let result: VerbResult = serde_json::from_value(event.payload.clone()).ok()?;
            if verbs.remove(&result.id) != Some(Verb::See) || !result.ok {
                return None;
            }
            let place = result.at?;
            Some(checker.check(place.seq, &place.hash).await)
        }
        _ => None,
    }
}

/// This machine's replica, which what the server shows is checked against.
struct Checker {
    replica: PathBuf,
    root: PublicKey,
    plain: Plain,
}

impl Checker {
    async fn check(&self, seq: u64, hash: &str) -> String {
        // Pull first if the replica is free; if another flare-link holds
        // it, read it as it stands.
        let records = match Replica::open(&self.replica, self.root) {
            Ok(mut replica) => {
                match pull(&mut replica, &self.plain).await {
                    Ok(_) | Err(PullError::Broker(_)) => {}
                    Err(error) => return format!("    ✗ your replica refused the broker: {error}"),
                }
                replica.records().to_vec()
            }
            Err(ReplicaError::Busy(_)) => match replica::read(&self.replica, self.root) {
                Ok(snapshot) => snapshot.records,
                Err(error) => return format!("    ? not checked: {error}"),
            },
            Err(error) => return format!("    ? not checked: {error}"),
        };
        let Some(mine) = usize::try_from(seq)
            .ok()
            .and_then(|index| records.get(index))
        else {
            return format!(
                "    ? not checked: your replica ends before entry {seq} (it holds {})",
                records.len()
            );
        };
        if hex(&mine.hash) == hash {
            format!(
                "    ✓ matches your replica at entry {seq}, {}",
                short(&mine.hash)
            )
        } else {
            format!(
                "    ✗ DOES NOT MATCH your replica: the server's entry {seq} is {}, yours is {}",
                hash.get(..8).unwrap_or(hash),
                short(&mine.hash)
            )
        }
    }
}
