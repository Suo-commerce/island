//! `flare-agent`: Flare's agent, in the Vessel `flare-home`.
//!
//! It asks the island socket who it is. As the prime it says Hello on the
//! Field and works task by task, for as long as it runs; when it promotes
//! a fork child it exits, and the Warden starts it again on that child's
//! files. As a fork child it does the one task its fork set gives it,
//! posts its note and exits; Taso never starts a child again.
//!
//! Exit codes: 0 for a fork child that posted its note, and for the prime
//! after a promotion; 1 when the Field or its files fail, after which the
//! Warden starts the prime again on its last Seal.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use flare_agent::field::{self, Field};
use flare_agent::island::Whoami;
use flare_agent::island::{self, Island};
use flare_agent::run::{Agent, Stepped, note};
use flare_agent::tools;

/// Flare's agent.
#[derive(Parser)]
#[command(name = "flare-agent", version)]
struct Args {
    /// The Field socket.
    #[arg(long, default_value = field::SOCKET)]
    field: PathBuf,
    /// The island socket.
    #[arg(long, default_value = island::SOCKET)]
    island: PathBuf,
    /// The work directory.
    #[arg(long, default_value = tools::WORK)]
    work: PathBuf,
    /// The longest the prime waits for its fork children's notes, in
    /// seconds, before `fork` returns what there is.
    #[arg(long, value_name = "SECONDS", default_value_t = 900)]
    fork_wait: u64,
}

fn main() -> ExitCode {
    let args = Args::parse();
    tools::set_work(&args.work);
    let island = Island::new(&args.island);
    // The island socket may start after the agent: wait for it.
    let me = loop {
        match island.whoami() {
            Ok(me) => break me,
            Err(why) => {
                eprintln!("flare-agent: waiting for the island socket: {why}");
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    };
    eprintln!(
        "flare-agent: {} {} in slot {}, vessel {}, model {}",
        me.subject, me.role, me.slot, me.vessel, me.default_model
    );
    match work(&args, island, &me) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("flare-agent: {why}");
            ExitCode::FAILURE
        }
    }
}

fn work(args: &Args, island: Island, me: &Whoami) -> Result<(), String> {
    let mut field = field::Socket::connect(&args.field)?;
    eprintln!(
        "flare-agent: last sealed turn {:?}",
        field.last_sealed_turn()
    );
    let mut model = island.clone();
    let mut forks = island.clone();
    let mut tasks = island;
    let mut agent = Agent::new(&mut field, &mut model, &mut tasks)?.with_forks(&mut forks);
    agent.waits.forks = Duration::from_secs(args.fork_wait);
    if me.is_fork() {
        note(serde_json::json!({
            "event": "start", "start": start_id(), "turn": agent.turn(), "fork": me.subject,
            "cursor": agent.state().cursor,
            "working": agent.state().working.as_ref().map(|task| task.id),
        }));
        return agent.run_child(me);
    }
    eprintln!(
        "flare-agent: carrying on after task {:?}, turn {}",
        agent.state().cursor,
        agent.turn()
    );
    note(serde_json::json!({
        "event": "start",
        "start": start_id(),
        "turn": agent.turn(),
        "cursor": agent.state().cursor,
        "working": agent.state().working.as_ref().map(|task| task.id),
    }));
    loop {
        match agent.step() {
            // Taso ends the run for the adoption, and the Warden starts
            // the agent again on the child's files.
            Ok(Stepped::Promoted(child)) => {
                eprintln!("flare-agent: {child} promoted; ending this run");
                return Ok(());
            }
            Ok(_) => {}
            // The island socket restarting, or the session server down:
            // tasks wait. Anything else from the Field or the files ends
            // the run.
            Err(why) if why.starts_with("a task") || why.contains("island.sock") => {
                eprintln!("flare-agent: {why}; trying again");
                std::thread::sleep(Duration::from_secs(5));
            }
            Err(why) => return Err(why),
        }
    }
}

/// This run, told apart from the others: the process's start time since
/// boot, from /proc, with its pid.
fn start_id() -> String {
    let started = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            // Field 22, counted after the command's closing parenthesis.
            let rest = stat.rsplit_once(')')?.1.to_owned();
            rest.split_whitespace().nth(19).map(str::to_owned)
        })
        .unwrap_or_default();
    format!("{}-{started}", std::process::id())
}
