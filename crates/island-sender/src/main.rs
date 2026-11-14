//! `island-sender`: hold the Warden's sender sockets for the broker.
//!
//! Each pass: pull the new records of the Island log from the broker and
//! verify them against the pinned root; open each Vessel's sender socket
//! and list the released effects; for each, follow the approval rule
//! (island-client's `approval` module): send what is approved (append a
//! `dispatch` entry naming the approval), decline what is declined, and
//! leave the rest waiting, unknown or not. One JSON line per effect goes to standard output.
//!
//! Two ways to run: `--socket` holds one Vessel's socket (the spikes and
//! tests); `--senders` serves every `<vessel-id>.sock` in the Warden's
//! senders directory, so a new or restarted Vessel needs nothing from it
//! (the node's service, docs/slice-2.md, "Actions and the sender").
//!
//! Runs as root: the sender sockets are the Warden's, mode 0600.
//!
//! Exit codes: 0 after `--once`, 1 on a failure it cannot wait out (a log
//! that does not verify, a key without a counter), 3 when
//! `--stop-after-send` stopped it. Without `--once`, a broker that does
//! not answer and a socket that breaks are waited out: the next pass
//! tries again.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use island_client::approval::{self, Marks};
use island_client::{Broker, Writer};
use std::collections::{BTreeMap, BTreeSet};

use island_core::{Body, Dispatch, Hash, PublicKey, StreamId, VesselId, public_key};
use island_env::keyfile;
use island_log::Chain;
use island_sender::{
    Action, Ended, Ledger, Planned, Released, SenderConn, Step, serve, sockets_in, vessel_of,
};
use serde_json::json;
use taso_field::protocol::{OpenEffect, Outcome};

/// Taso's sender: sends only what the Island log approves.
#[derive(Parser)]
#[command(name = "island-sender", version)]
struct Args {
    /// One Vessel's sender socket: <run_dir>/senders/<vessel-id>.sock,
    /// the id in 32 hex digits.
    #[arg(long, conflicts_with = "senders", required_unless_present = "senders")]
    socket: Option<PathBuf>,
    /// The Warden's senders directory, <run_dir>/senders: every
    /// <vessel-id>.sock in it is served, each pass.
    #[arg(long, value_name = "DIR")]
    senders: Option<PathBuf>,
    /// The broker, for example http://10.77.0.1:4747.
    #[arg(long, value_name = "URL")]
    broker: String,
    /// The pinned root public key, in hex. The log is verified against it.
    #[arg(long, value_parser = parse_key)]
    root: PublicKey,
    /// This sender's key file: it signs the `dispatch` entries. Its grant
    /// must cover `dispatch` on the `approvals` stream. Whose approvals
    /// count is the grants' business: `approval` on `approvals`.
    #[arg(long)]
    key: PathBuf,
    /// One pass, then exit. With --socket it waits up to --wait for the
    /// socket first; with --senders it serves what is there.
    #[arg(long)]
    once: bool,
    /// How long to wait for the sender socket to appear, in seconds.
    #[arg(long, default_value_t = 120)]
    wait: u64,
    /// Time between passes, in milliseconds.
    #[arg(long, default_value_t = 500)]
    interval_ms: u64,
    /// Fault injection for spike S2: exit with code 3 right after the
    /// first `dispatch` is in the log, before reporting to Taso, as a
    /// sender that crashes there would. The next run must not send again.
    #[arg(long, hide = true)]
    stop_after_send: bool,
}

fn parse_key(text: &str) -> Result<PublicKey, String> {
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
            eprintln!("island-sender: no runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match run(&runtime, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("island-sender: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What went wrong in a pass.
enum Failed {
    /// Waited out without --once: the broker, a socket.
    Passing(String),
    /// Never waited out: the log does not verify, no counter left.
    Fatal(String),
}

impl From<String> for Failed {
    fn from(error: String) -> Self {
        Self::Passing(error)
    }
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Passing(why) | Self::Fatal(why) => f.write_str(why),
        }
    }
}

struct Sender {
    url: String,
    broker: Option<Broker>,
    writer: Writer,
    public: PublicKey,
    chain: Chain,
    marks: Marks,
    /// The last action printed for each effect: a pass prints only what
    /// changed, so a waiting effect is one line, not one per pass.
    said: BTreeMap<(VesselId, u64), String>,
    /// Actions whose dispatch the gate refused. The decision rule and the
    /// gate's are the same, so this means they disagree: such an action is
    /// not tried again by this process, and every refusal is a verdict in
    /// the log for someone to read.
    refused: BTreeSet<(VesselId, Hash)>,
}

impl Sender {
    /// The broker, connecting first if the last pass lost it.
    fn broker(&mut self, runtime: &tokio::runtime::Runtime) -> Result<&Broker, Failed> {
        if self.broker.is_none() {
            let broker = runtime
                .block_on(Broker::connect(&self.url))
                .map_err(|error| Failed::Passing(format!("the broker: {error}")))?;
            self.broker = Some(broker);
        }
        Ok(self.broker.as_ref().expect("connected above"))
    }

    /// Pull and verify everything after the chain's head; note the
    /// approvals and dispatches.
    fn refresh(&mut self, runtime: &tokio::runtime::Runtime) -> Result<(), Failed> {
        loop {
            let after = self.chain.head().map(|head| head.seq);
            let answer = runtime.block_on(self.broker(runtime)?.log(after, None));
            let (records, more) = match answer {
                Ok(answer) => answer,
                Err(error) => {
                    self.broker = None;
                    return Err(Failed::Passing(format!("reading the log: {error}")));
                }
            };
            for record in &records {
                self.chain.apply(record).map_err(|error| {
                    Failed::Fatal(format!(
                        "the log does not verify against the pinned root: {error}"
                    ))
                })?;
                self.marks.note(record);
            }
            if !more || records.is_empty() {
                return Ok(());
            }
        }
    }

    /// Append the `dispatch` for one effect; its entry.
    fn send(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        vessel: VesselId,
        planned: &Planned,
        approval: u64,
    ) -> Result<Result<u64, String>, Failed> {
        let counter = self
            .chain
            .next_counter(&self.public)
            .ok_or_else(|| Failed::Fatal("the sender's key has no counter left".to_owned()))?;
        let body = Body::Dispatch(Dispatch {
            vessel,
            op: planned.effect.op,
            key: planned.key,
            request: planned.hash,
            approval,
            // Nothing is submitted after it yet: the providers that send a
            // verse with the Vessel's key come later in slice 2.
            counter: 0,
        });
        let signed = self
            .writer
            .submission(StreamId::from(approval::STREAM), counter, body);
        let answer = runtime.block_on(
            self.broker(runtime)?
                .append(&signed.submission, &signed.sig_writer),
        );
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                // Whether it landed is not known: the next pass reads the
                // log and finds the dispatch, or sends it again with the
                // same counter.
                self.broker = None;
                return Err(Failed::Passing(format!("appending the dispatch: {error}")));
            }
        };
        // Read it back, so the dispatch (and the approval it used) count
        // from here on, whatever the answer was.
        self.refresh(runtime)?;
        Ok(match answer {
            Ok((seq, _)) => Ok(seq),
            Err(refusal) => Err(format!("the gate refused the dispatch: {refusal:?}")),
        })
    }
}

fn run(runtime: &tokio::runtime::Runtime, args: &Args) -> Result<(), String> {
    let key = island_core::SigningKey::from_bytes(&keyfile::load(&args.key)?);
    let public = public_key(&key);
    let one = match &args.socket {
        Some(socket) => Some((
            socket.clone(),
            vessel_of(socket)
                .ok_or("the socket's name is not <vessel-id>.sock with a 32-digit hex id")?,
        )),
        None => None,
    };
    let mut sender = Sender {
        url: args.broker.clone(),
        broker: None,
        writer: Writer::new(key),
        public,
        chain: Chain::new(args.root),
        marks: Marks::new(),
        said: BTreeMap::new(),
        refused: BTreeSet::new(),
    };
    let interval = Duration::from_millis(args.interval_ms.max(50));
    let patience = args.wait.saturating_mul(1000) / interval.as_millis().max(1) as u64;
    let mut waited = 0_u64;
    loop {
        let sockets = match (&one, &args.senders) {
            (Some((socket, vessel)), _) => {
                if !socket.exists() {
                    if args.once && waited >= patience {
                        return Err(format!("no sender socket at {}", socket.display()));
                    }
                    waited += 1;
                    std::thread::sleep(interval);
                    continue;
                }
                vec![(socket.clone(), *vessel)]
            }
            (None, Some(dir)) => sockets_in(dir),
            (None, None) => unreachable!("clap requires one of them"),
        };
        waited = 0;
        let result = sender.refresh(runtime).and_then(|()| {
            for (socket, vessel) in &sockets {
                match pass(runtime, &mut sender, socket, *vessel, args) {
                    // One Vessel's socket breaking (its agent exited, the
                    // Warden restarted) does not keep the others waiting.
                    Err(Failed::Passing(error)) if one.is_none() && !args.once => {
                        eprintln!("island-sender: {error}");
                    }
                    other => other?,
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(Failed::Fatal(error)) => return Err(error),
            Err(Failed::Passing(error)) if args.once => return Err(error),
            Err(Failed::Passing(error)) => eprintln!("island-sender: {error}"),
        }
        if args.once {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
}

/// The sender's ledger for one pass: its log, through the broker.
struct Online<'a> {
    sender: &'a mut Sender,
    runtime: &'a tokio::runtime::Runtime,
}

impl Ledger<Failed> for Online<'_> {
    fn marks(&self) -> &Marks {
        &self.sender.marks
    }

    fn dispatch(
        &mut self,
        vessel: VesselId,
        planned: &Planned,
        approval: u64,
    ) -> Result<Result<u64, String>, Failed> {
        self.sender.send(self.runtime, vessel, planned, approval)
    }
}

/// A sender socket whose failures are waited out.
struct Socket(SenderConn<UnixStream>);

impl Released<Failed> for Socket {
    fn open(&mut self) -> Result<Vec<OpenEffect>, Failed> {
        Ok(Released::<String>::open(&mut self.0)?)
    }

    fn report(&mut self, op: u64, result: Outcome) -> Result<Option<String>, Failed> {
        Ok(Released::<String>::report(&mut self.0, op, result)?)
    }
}

fn pass(
    runtime: &tokio::runtime::Runtime,
    sender: &mut Sender,
    socket: &Path,
    vessel: VesselId,
    args: &Args,
) -> Result<(), Failed> {
    let stream =
        UnixStream::connect(socket).map_err(|error| format!("{}: {error}", socket.display()))?;
    let mut conn = Socket(SenderConn::hello(stream).map_err(|error| error.to_string())?);
    let mut refused = std::mem::take(&mut sender.refused);
    let mut lines = Vec::new();
    let ended = serve(
        &mut Online { sender, runtime },
        &mut conn,
        vessel,
        &mut refused,
        args.stop_after_send,
        |step| lines.push(line(vessel, step)),
    );
    sender.refused = refused;
    for (op, action, text) in lines {
        // A pass prints only what changed: a waiting effect is one line,
        // not one per pass.
        if sender.said.insert((vessel, op), action.clone()).as_deref() != Some(action.as_str()) {
            println!("{text}");
        }
    }
    if ended? == Ended::StoppedAfterSend {
        std::process::exit(3);
    }
    Ok(())
}

/// One step as a JSON line: (op, action, line).
fn line(vessel: VesselId, step: &Step) -> (u64, String, String) {
    let planned = &step.planned;
    let mut line = json!({
        "vessel": approval::hex(&vessel),
        "op": planned.effect.op,
        "provider": planned.effect.provider,
        "request_hash": approval::hex(&planned.hash),
        "key": approval::hex(&planned.key),
        "unknown": planned.effect.unknown,
        "action": step.action.name(),
    });
    match &step.action {
        Action::Decline(seq) => line["declined_at"] = json!(seq),
        Action::AlreadySent(seq) => line["sent_at"] = json!(seq),
        Action::Sent { entry, approval } => {
            line["approved_at"] = json!(approval);
            line["sent_at"] = json!(entry);
        }
        Action::SendRefused(why) => line["error"] = json!(why),
        Action::Wait | Action::HeldAfterRefusal => {}
    }
    match &step.reported {
        None if matches!(step.action, Action::Sent { .. }) => {
            line["stopped_before_report"] = json!(true);
        }
        None => {}
        Some(refused) => {
            line["reported"] = json!(refused.is_none());
            if let Some(why) = refused {
                line["report_refused"] = json!(why);
            }
        }
    }
    (
        planned.effect.op,
        step.action.name().to_owned(),
        line.to_string(),
    )
}
