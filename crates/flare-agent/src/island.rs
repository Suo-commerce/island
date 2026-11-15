//! The island socket, as the agent asks it (docs/slice-2.md, "The island
//! socket"): who it is, the next task, its instructions, the model, and
//! the notes between a fork child and its prime.

use std::path::{Path, PathBuf};
use std::time::Duration;

use flare_core::session::Task;
use serde::{Deserialize, Serialize};

use crate::agent_loop::{Completion, Model};
use crate::http::ask;
use crate::message::Message;
use crate::openai;
use crate::tools::Spec;

/// The default path of the island socket in a Vessel.
pub const SOCKET: &str = "/run/island/island.sock";

/// A model call's digest, as the island socket computes it for its
/// `access` entry: over the body sent and the body received.
pub fn call_digest(request: &[u8], response: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("island.inference-call.v1");
    hasher.update(&(request.len() as u64).to_le_bytes());
    hasher.update(request);
    hasher.update(response);
    *hasher.finalize().as_bytes()
}

/// Who the agent is, from the peer uid.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Whoami {
    /// Its Vessel, in hex.
    pub vessel: String,
    /// Its Vessel's subject.
    pub subject: String,
    /// `prime` or `fork`.
    pub role: String,
    /// Its uid slot.
    pub slot: u32,
    /// The prime's subject: its own, or its parent's.
    #[serde(default)]
    pub prime: String,
    /// The forks it has left.
    pub forks_left: u64,
    /// The prime's fork slots, in the order Taso gives them out.
    #[serde(default)]
    pub fork_slots: Vec<u32>,
    /// The model it gets when it names none.
    pub default_model: String,
}

impl Whoami {
    /// Whether this is a fork child.
    pub fn is_fork(&self) -> bool {
        self.role == "fork"
    }

    /// The fork slots no child has taken, in the order Taso will give
    /// them: slots go in the config's order, each once.
    pub fn free_slots(&self) -> &[u32] {
        let left = usize::try_from(self.forks_left)
            .unwrap_or(usize::MAX)
            .min(self.fork_slots.len());
        &self.fork_slots[self.fork_slots.len() - left..]
    }
}

/// The longest note a fork child may post, in bytes (docs/slice-2.md,
/// "Forking, step by step").
pub const MAX_NOTE: usize = 16 * 1024;

/// A fork child's newest note, as its prime reads it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Note {
    /// The child's name.
    pub child: String,
    /// Its uid slot.
    pub slot: u32,
    /// The fork set it belongs to.
    pub set: String,
    /// What it made.
    pub text: String,
    /// Whether it is done.
    pub done: bool,
    /// Whether it failed before it could end a turn of its own: it holds
    /// nothing to promote.
    #[serde(default)]
    pub failed: bool,
}

/// What a fork child says of itself with a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// It started on its task.
    Started,
    /// Its turn is sealed with the task answered: it can be promoted.
    Done,
    /// It failed without a Seal of its own: nothing to promote.
    Failed,
}

/// What forking needs from the island socket: Taso tells an agent
/// neither who it is nor what its children made.
pub trait Forks {
    /// Who this is, asked now: the fork slots left change.
    fn whoami(&mut self) -> Result<Whoami, String>;
    /// A fork child's note to its prime.
    fn post_note(&mut self, set: &str, text: &str, progress: Progress) -> Result<(), String>;
    /// The newest note of each of the prime's children.
    fn notes(&mut self) -> Result<Vec<Note>, String>;
}

/// The island socket.
#[derive(Clone, Debug)]
pub struct Island {
    path: PathBuf,
}

/// Where tasks come from.
pub trait Tasks {
    /// The next task after `after`, waiting a while for one.
    fn next(&mut self, after: Option<u64>) -> Result<Option<Task>, String>;
    /// The agent's instructions, if Island has some for it; `None`: it
    /// uses its own.
    fn instructions(&mut self) -> Option<String> {
        None
    }
}

impl Island {
    /// The socket at `path`.
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
        }
    }

    fn get(&self, target: &str, quiet: Duration) -> Result<(u16, Vec<u8>), String> {
        ask(&self.path, "GET", target, b"", quiet)
            .map(|answer| (answer.status, answer.body))
            .map_err(|error| format!("{}: {error}", self.path.display()))
    }

    /// Who this is.
    pub fn whoami(&self) -> Result<Whoami, String> {
        match self.get("/whoami", Duration::from_secs(30))? {
            (200, body) => {
                serde_json::from_slice(&body).map_err(|error| format!("whoami: {error}"))
            }
            (status, body) => Err(format!(
                "whoami: {status}: {}",
                String::from_utf8_lossy(&body)
            )),
        }
    }
}

impl Tasks for Island {
    fn next(&mut self, after: Option<u64>) -> Result<Option<Task>, String> {
        let target = match after {
            Some(after) => format!("/task?after={after}"),
            None => "/task".to_owned(),
        };
        // The socket waits up to 50 s for a task.
        match self.get(&target, Duration::from_secs(90))? {
            (200, body) => serde_json::from_slice(&body)
                .map(Some)
                .map_err(|error| format!("a task: {error}")),
            (204, _) => Ok(None),
            (status, body) => Err(format!(
                "a task: {status}: {}",
                String::from_utf8_lossy(&body)
            )),
        }
    }

    /// Island's instructions for the agent, as they are now. Anything but
    /// a text (none set, the socket restarting) leaves the agent on its
    /// own built-in ones; the transcript says which it ran on.
    fn instructions(&mut self) -> Option<String> {
        match self.get("/instructions", Duration::from_secs(30)) {
            Ok((200, body)) => String::from_utf8(body)
                .ok()
                .filter(|text| !text.trim().is_empty()),
            Ok((404, _)) => None,
            Ok((status, body)) => {
                eprintln!(
                    "flare-agent: instructions: {status}: {}",
                    String::from_utf8_lossy(&body)
                );
                None
            }
            Err(why) => {
                eprintln!("flare-agent: instructions: {why}");
                None
            }
        }
    }
}

impl Forks for Island {
    fn whoami(&mut self) -> Result<Whoami, String> {
        Island::whoami(self)
    }

    fn post_note(&mut self, set: &str, text: &str, progress: Progress) -> Result<(), String> {
        let body = serde_json::json!({
            "set": set, "text": text,
            "done": progress != Progress::Started,
            "failed": progress == Progress::Failed,
        })
        .to_string();
        let answer = ask(
            &self.path,
            "POST",
            "/note",
            body.as_bytes(),
            Duration::from_secs(60),
        )
        .map_err(|error| format!("{}: {error}", self.path.display()))?;
        match answer.status {
            200 => Ok(()),
            status => Err(format!(
                "a note: {status}: {}",
                String::from_utf8_lossy(&answer.body)
            )),
        }
    }

    fn notes(&mut self) -> Result<Vec<Note>, String> {
        #[derive(Deserialize)]
        struct Notes {
            notes: Vec<Note>,
        }
        match self.get("/notes", Duration::from_secs(30))? {
            (200, body) => serde_json::from_slice::<Notes>(&body)
                .map(|notes| notes.notes)
                .map_err(|error| format!("notes: {error}")),
            (status, body) => Err(format!(
                "notes: {status}: {}",
                String::from_utf8_lossy(&body)
            )),
        }
    }
}

impl Model for Island {
    fn complete(&mut self, messages: &[Message], tools: &[Spec]) -> Result<Completion, String> {
        let request = openai::request(messages, tools);
        // A model may think a long while between two pieces of its answer.
        let answer = ask(
            &self.path,
            "POST",
            "/v1/chat/completions",
            &request,
            Duration::from_secs(300),
        )
        .map_err(|error| format!("the model: {error}"))?;
        let digest = call_digest(&request, &answer.body);
        if answer.status != 200 {
            let why = serde_json::from_slice::<serde_json::Value>(&answer.body)
                .ok()
                .and_then(|value| value["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| String::from_utf8_lossy(&answer.body).into_owned());
            return Err(format!("the model ({}): {why}", answer.status));
        }
        Ok(Completion {
            assistant: openai::read(&answer.body),
            digest,
            bytes_in: request.len(),
            bytes_out: answer.body.len(),
        })
    }
}
