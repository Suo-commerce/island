//! One run of the prime (docs/slice-2.md, "One run of the prime"): load
//! what the last Seal holds, pull the next task, work on it with the
//! model and the tools, reply, end the turn; again.
//!
//! What the agent keeps lives in `/work/.flare/`: the task cursor, the
//! task being worked on, and the conversation. Every start rolls `/work`
//! back to the last Seal, so a crash costs at most the turn it hit: a
//! task not finished at the last Seal is worked on again, and its replies
//! carry the same keys, so Jarmo is told once.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use flare_core::session::{SayRequest, Task};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent_loop::{self, Ended, Host, Model};
use crate::field::{Declared, Field};
use crate::island::Tasks;
use crate::message::{Message, window};
use crate::tools::{Spec, specs, work_path};

/// A turn ends at the next tool round after this long (docs/slice-2.md,
/// "Turns"), so a crash costs little.
pub const TURN: Duration = Duration::from_secs(120);

/// The most bytes of conversation a request carries.
pub const WINDOW: usize = 200 * 1024;

/// The most messages kept in `/work/.flare/history.json`.
pub const KEEP: usize = 400;

/// The provider of the agent's replies.
pub const SAY: &str = "flare.say";

/// What the agent is told it is.
pub const SYSTEM: &str = "You are Flare's agent. You work for Jarmo, inside an isolated \
Taso Vessel on his Island server. You have no network. Your files are in /work: they are kept \
between turns, and after a crash they go back to the last turn that ended. You have tools: \
read, write, edit and bash work in /work; say sends Jarmo a message at once. Jarmo talks to \
you from Flare, his console. When you have done what he asked, answer him: your answer is \
sent to him as your reply. Keep it plain and short, and say what you did. Island is his \
signed log of decisions and work, in short entries called verses; you cannot write to it \
yet, but you may draft a verse for him in your reply.";

/// What the agent keeps between turns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The last task finished.
    pub cursor: Option<u64>,
    /// The task being worked on.
    pub working: Option<Task>,
    /// Messages said on it so far with `say`.
    pub says: u64,
}

/// An action's key: derived from the task, the purpose and an index,
/// never random, so a turn redone after a crash declares the same keys.
pub fn action_key(task: u64, purpose: &str, index: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("flare.action-key.v1");
    hasher.update(&task.to_le_bytes());
    hasher.update(&(purpose.len() as u64).to_le_bytes());
    hasher.update(purpose.as_bytes());
    hasher.update(&index.to_le_bytes());
    *hasher.finalize().as_bytes()
}

fn flare_file(name: &str) -> PathBuf {
    work_path(&format!(".flare/{name}"))
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Add an event to `/work/.flare/log.jsonl`: what a check on the host
/// reads (the starts, the tasks, the turns). Like everything in `/work`
/// it holds what the last Seal held, and this run's events after it.
pub fn note(event: Value) {
    use std::io::Write;
    let path = flare_file("log.jsonl");
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        })
        .and_then(|mut file| writeln!(file, "{event}"));
    if let Err(error) = written {
        eprintln!("flare-agent: {}: {error}", path.display());
    }
}

/// The state and the conversation, as the last Seal holds them.
pub fn load() -> Result<(State, Vec<Message>), String> {
    let read = |name: &str| -> Result<Option<Vec<u8>>, String> {
        match std::fs::read(flare_file(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{}: {error}", flare_file(name).display())),
        }
    };
    let state = match read("state.json")? {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("state.json: {error}"))?
        }
        None => State::default(),
    };
    let history = match read("history.json")? {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("history.json: {error}"))?
        }
        None => Vec::new(),
    };
    Ok((state, history))
}

/// Keep the state and the conversation for the next Seal.
pub fn save(state: &State, history: &[Message]) -> Result<(), String> {
    let from = history.len().saturating_sub(KEEP);
    write_atomic(
        &flare_file("history.json"),
        &serde_json::to_vec(&history[from..]).map_err(|error| error.to_string())?,
    )?;
    write_atomic(
        &flare_file("state.json"),
        &serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?,
    )
}

/// The agent.
pub struct Agent<'a> {
    field: &'a mut dyn Field,
    model: &'a mut dyn Model,
    tasks: &'a mut dyn Tasks,
    state: State,
    history: Vec<Message>,
    turn: u64,
    sealed_at: Instant,
    tools: Vec<Spec>,
    /// How long to wait for a refused action's turn.
    pub patience: Duration,
}

impl<'a> Agent<'a> {
    /// The agent, carrying on from the last Seal.
    pub fn new(
        field: &'a mut dyn Field,
        model: &'a mut dyn Model,
        tasks: &'a mut dyn Tasks,
    ) -> Result<Self, String> {
        let (state, history) = load()?;
        let turn = field.last_sealed_turn().map_or(1, |turn| turn + 1);
        Ok(Self {
            field,
            model,
            tasks,
            state,
            history,
            turn,
            sealed_at: Instant::now(),
            tools: specs(),
            patience: Duration::from_secs(60),
        })
    }

    /// What it keeps.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Its conversation.
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// The turn it is in.
    pub fn turn(&self) -> u64 {
        self.turn
    }

    /// One task, if one comes within the island socket's wait: worked on,
    /// answered, and sealed. `Ok(None)`: none came.
    pub fn step(&mut self) -> Result<Option<u64>, String> {
        let task = match self.state.working.clone() {
            Some(task) => task,
            None => match self.tasks.next(self.state.cursor)? {
                None => return Ok(None),
                Some(task) => {
                    self.history.push(Message::User {
                        text: format!("[Task #{} from {}]\n{}", task.id, task.client, task.text),
                    });
                    self.state.working = Some(task.clone());
                    self.state.says = 0;
                    task
                }
            },
        };
        eprintln!("flare-agent: task #{} (turn {})", task.id, self.turn);
        note(json!({ "event": "task", "task": task.id, "turn": self.turn }));
        let system = Message::System {
            text: SYSTEM.to_owned(),
        };
        let mut host = TaskHost {
            field: &mut *self.field,
            task: &task,
            state: &mut self.state,
            turn: &mut self.turn,
            sealed_at: &mut self.sealed_at,
            system,
            patience: self.patience,
        };
        let ended = agent_loop::run(&mut *self.model, &mut host, &mut self.history, &self.tools);
        let reply = match ended {
            Ended::Answered(text) if !text.trim().is_empty() => Some(text),
            // Everything was said with `say` already.
            Ended::Answered(_) if host.state.says > 0 => None,
            Ended::Answered(_) => Some("(The model gave no answer.)".to_owned()),
            Ended::Failed(why) => Some(format!("I could not finish this: {why}")),
        };
        if let Some(text) = reply {
            host.declare_say(&text, "reply", 0)?;
        }
        self.state.cursor = Some(task.id);
        self.state.working = None;
        self.state.says = 0;
        self.end_turn()?;
        Ok(Some(task.id))
    }

    /// Save and seal.
    fn end_turn(&mut self) -> Result<(), String> {
        save(&self.state, &self.history)?;
        note(json!({ "event": "turn", "turn": self.turn, "cursor": self.state.cursor }));
        let sealed = self.field.turn_end(self.turn)?;
        eprintln!(
            "flare-agent: turn {} ended, sealed at {sealed:?}",
            self.turn
        );
        self.turn += 1;
        self.sealed_at = Instant::now();
        Ok(())
    }
}

/// The loop's host for one task.
struct TaskHost<'a> {
    field: &'a mut dyn Field,
    task: &'a Task,
    state: &'a mut State,
    turn: &'a mut u64,
    sealed_at: &'a mut Instant,
    system: Message,
    patience: Duration,
}

impl TaskHost<'_> {
    /// Declare `flare.say`: released, it goes to the session. While an
    /// earlier action's fate is open Taso refuses new ones; that wait is
    /// short for a reply, which needs no approval.
    fn declare_say(&mut self, text: &str, purpose: &str, index: u64) -> Result<String, String> {
        let request = serde_json::to_vec(&SayRequest {
            task: self.task.id,
            text: text.to_owned(),
        })
        .map_err(|error| error.to_string())?;
        let key = action_key(self.task.id, purpose, index);
        let started = Instant::now();
        loop {
            match self.field.declare(SAY, &request, &key)? {
                Declared::Released(op) => return Ok(format!("Sent (op {op}).")),
                Declared::Held(op) => {
                    return Ok(format!(
                        "Held (op {op}): a fork's message waits for its promotion."
                    ));
                }
                Declared::Refused(why) if started.elapsed() < self.patience => {
                    eprintln!("flare-agent: say refused for now: {why}");
                    std::thread::sleep(Duration::from_secs(1));
                }
                Declared::Refused(why) => return Err(format!("not sent: {why}")),
            }
        }
    }
}

impl Host for TaskHost<'_> {
    fn window(&self, history: &[Message]) -> Vec<Message> {
        window(&self.system, history, WINDOW)
    }

    fn say(&mut self, text: &str) -> Result<String, String> {
        let index = self.state.says;
        self.state.says += 1;
        self.declare_say(text, "say", index)
    }

    fn record(&mut self, entry: Value) {
        let mut entry = json!({ "turn": *self.turn, "task": self.task.id, "step": entry });
        let mut bytes = serde_json::to_vec(&entry).unwrap_or_default();
        // A transcript entry stays small; the conversation is in /work.
        if bytes.len() > 1024 * 1024 {
            entry["step"]["message"] = json!(format!("({} bytes, not kept here)", bytes.len()));
            bytes = serde_json::to_vec(&entry).unwrap_or_default();
        }
        if let Err(why) = self.field.transcript(&bytes) {
            eprintln!("flare-agent: transcript: {why}");
        }
    }

    fn round(&mut self, history: &[Message]) -> Result<(), String> {
        if self.sealed_at.elapsed() < TURN {
            return Ok(());
        }
        save(self.state, history)?;
        note(
            json!({ "event": "turn", "turn": *self.turn, "cursor": self.state.cursor, "mid_task": self.task.id }),
        );
        let sealed = self.field.turn_end(*self.turn)?;
        eprintln!(
            "flare-agent: turn {} ended mid-task, sealed at {sealed:?}",
            *self.turn
        );
        *self.turn += 1;
        *self.sealed_at = Instant::now();
        Ok(())
    }
}
