//! The island socket, as the agent asks it (docs/slice-2.md, "The island
//! socket"): who it is, the next task, and the model.

use std::path::{Path, PathBuf};
use std::time::Duration;

use flare_core::session::Task;
use serde::Deserialize;

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
    /// The forks it has left.
    pub forks_left: u64,
    /// The model it gets when it names none.
    pub default_model: String,
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
