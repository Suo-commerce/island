//! `island-sender`: hold one Vessel's sender socket for the broker.
//!
//! Each pass: pull the new records of the Island log from the broker and
//! verify them against the pinned root; open the Vessel's sender socket
//! and list the released effects; for each, follow the approval rule
//! (island-client's `approval` module): send what is approved (append a
//! `dispatch` entry naming the approval), decline what is declined, lapse
//! what was lost in a Warden restart and never approved, and leave the
//! rest. One JSON line per effect goes to standard output.
//!
//! Runs as root: the sender socket is the Warden's, mode 0600.
//!
//! Exit codes: 0 after `--once`, 1 on a failure it cannot wait out (a log
//! that does not verify, a key without a counter, a broken socket answer),
//! 3 when `--stop-after-send` stopped it.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use island_client::approval::{self, Decision, Marks};
use island_client::{Broker, Writer};
use island_core::{Body, Dispatch, PublicKey, StreamId, public_key};
use island_env::keyfile;
use island_log::Chain;
use island_sender::{Planned, SenderConn, outcome, plan, vessel_of};
use serde_json::json;

/// Taso's sender for one Vessel: sends only what the Island log approves.
#[derive(Parser)]
#[command(name = "island-sender", version)]
struct Args {
    /// The Vessel's sender socket: <run_dir>/senders/<vessel-id>.sock,
    /// the id in 32 hex digits.
    #[arg(long)]
    socket: PathBuf,
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
    /// One pass, then exit; waits up to --wait for the socket first.
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

struct Sender {
    broker: Broker,
    writer: Writer,
    public: PublicKey,
    chain: Chain,
    marks: Marks,
    vessel: island_core::VesselId,
}

impl Sender {
    /// Pull and verify everything after the chain's head; note the
    /// approvals and dispatches.
    fn refresh(&mut self, runtime: &tokio::runtime::Runtime) -> Result<(), String> {
        loop {
            let after = self.chain.head().map(|head| head.seq);
            let (records, more) = runtime
                .block_on(self.broker.log(after, None))
                .map_err(|error| format!("reading the log: {error}"))?;
            for record in &records {
                self.chain.apply(record).map_err(|error| {
                    format!("the log does not verify against the pinned root: {error}")
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
        planned: &Planned,
        approval: u64,
    ) -> Result<Result<u64, String>, String> {
        let counter = self
            .chain
            .next_counter(&self.public)
            .ok_or("the sender's key has no counter left")?;
        let body = Body::Dispatch(Dispatch {
            vessel: self.vessel,
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
        let answer = runtime
            .block_on(self.broker.append(&signed.submission, &signed.sig_writer))
            .map_err(|error| format!("appending the dispatch: {error}"))?;
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
    let vessel = vessel_of(&args.socket)
        .ok_or("the socket's name is not <vessel-id>.sock with a 32-digit hex id")?;
    let broker = runtime
        .block_on(Broker::connect(&args.broker))
        .map_err(|error| format!("{error}"))?;
    let mut sender = Sender {
        broker,
        writer: Writer::new(key),
        public,
        chain: Chain::new(args.root),
        marks: Marks::new(),
        vessel,
    };
    let interval = Duration::from_millis(args.interval_ms.max(50));
    let patience = args.wait.saturating_mul(1000) / interval.as_millis().max(1) as u64;
    let mut waited = 0_u64;
    loop {
        if !args.socket.exists() {
            if args.once && waited >= patience {
                return Err(format!("no sender socket at {}", args.socket.display()));
            }
            waited += 1;
            std::thread::sleep(interval);
            continue;
        }
        waited = 0;
        sender.refresh(runtime)?;
        match pass(runtime, &mut sender, args) {
            Ok(()) => {}
            // The agent exited and the Warden closed the socket: wait for
            // the next start, unless this was the only pass.
            Err(error) if !args.once => eprintln!("island-sender: {error}"),
            Err(error) => return Err(error),
        }
        if args.once {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
}

fn pass(runtime: &tokio::runtime::Runtime, sender: &mut Sender, args: &Args) -> Result<(), String> {
    let stream = UnixStream::connect(&args.socket)
        .map_err(|error| format!("{}: {error}", args.socket.display()))?;
    let mut conn = SenderConn::hello(stream).map_err(|error| error.to_string())?;
    let effects = conn.open().map_err(|error| error.to_string())?;
    for effect in effects {
        let planned = plan(&sender.marks, &sender.vessel, effect);
        let op = planned.effect.op;
        let mut line = json!({
            "vessel": approval::hex(&sender.vessel),
            "op": op,
            "provider": planned.effect.provider,
            "request_hash": approval::hex(&planned.hash),
            "key": approval::hex(&planned.key),
            "unknown": planned.effect.unknown,
        });
        let (action, sent_entry) = match planned.decision {
            Decision::Wait => ("wait", None),
            Decision::Lapse => ("lapse", None),
            Decision::Decline(seq) => {
                line["declined_at"] = json!(seq);
                ("decline", None)
            }
            Decision::AlreadySent(sent) => {
                line["sent_at"] = json!(sent.entry);
                ("already-sent", None)
            }
            Decision::Send(approval) => {
                line["approved_at"] = json!(approval);
                match sender.send(runtime, &planned, approval)? {
                    Ok(seq) => {
                        line["sent_at"] = json!(seq);
                        ("send", Some(seq))
                    }
                    Err(why) => {
                        line["error"] = json!(why);
                        ("send-refused", None)
                    }
                }
            }
        };
        line["action"] = json!(action);
        if action == "send" && args.stop_after_send {
            line["stopped_before_report"] = json!(true);
            println!("{line}");
            std::process::exit(3);
        }
        if let Some(result) = outcome(planned.decision, sent_entry) {
            let refused = conn.report(op, result).map_err(|error| error.to_string())?;
            line["reported"] = json!(refused.is_none());
            if let Some(why) = refused {
                line["report_refused"] = json!(why);
            }
        }
        println!("{line}");
    }
    Ok(())
}
