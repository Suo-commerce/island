//! The agent's tools (docs/slice-2.md, "Tools of the loop"): `read`,
//! `write`, `edit` and `bash` work in `/work` and leave the Vessel as
//! nothing; `say` replies to Jarmo, as the action `flare.say`.
//!
//! The four file tools are ported from Pi's coding agent, with their
//! descriptions and messages, so a model that knows Pi knows these.

pub mod bash;
pub mod edit;
pub mod read;
pub mod write;

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde_json::Value;

/// The directory relative paths are taken from.
pub const WORK: &str = "/work";

/// What a tool call came to: the text the model reads, and whether it is
/// an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// What the model reads.
    pub text: String,
    /// Whether the call failed.
    pub is_error: bool,
}

impl Outcome {
    /// A result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    /// A failure.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// A string argument.
pub fn string_arg(args: &Value, name: &str) -> Result<String, String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("Validation failed for tool arguments: `{name}` must be a string"))
}

/// An optional number argument, as a whole number.
pub fn number_arg(args: &Value, name: &str) -> Result<Option<f64>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_f64().map(Some).ok_or_else(|| {
            format!("Validation failed for tool arguments: `{name}` must be a number")
        }),
    }
}

/// The work directory, if not `/work` (the tests, a run outside a Vessel).
static WORK_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Work in `dir` instead of `/work`.
pub fn set_work(dir: &Path) {
    if let Ok(mut work) = WORK_DIR.write() {
        *work = Some(dir.to_owned());
    }
}

/// The work directory.
pub fn work_dir() -> PathBuf {
    WORK_DIR
        .read()
        .ok()
        .and_then(|work| work.clone())
        .unwrap_or_else(|| PathBuf::from(WORK))
}

/// Tests that change the work directory take this first.
#[cfg(test)]
pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A path as a tool takes it: absolute as it is, relative from `/work`.
/// A leading `@` (as some models write paths) is dropped. Where it may
/// lead is the Vessel's business: Landlock and its filesystem.
pub fn work_path(path: &str) -> PathBuf {
    let path = path.strip_prefix('@').unwrap_or(path);
    let work = work_dir();
    if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        work.join(path)
    }
}

/// A tool as the model sees it.
#[derive(Clone, Debug)]
pub struct Spec {
    /// Its name.
    pub name: &'static str,
    /// What it does.
    pub description: String,
    /// Its parameters, as JSON Schema.
    pub parameters: Value,
}

/// The description of `say`.
pub const SAY_DESCRIPTION: &str = "Send a message to Jarmo now, while you keep working: a \
question, or progress on a long task. Your final answer is sent to him when you finish, so do \
not repeat it here.";

/// Every tool, as the model sees them.
pub fn specs() -> Vec<Spec> {
    vec![
        Spec {
            name: "read",
            description: read::description(),
            parameters: read::parameters(),
        },
        Spec {
            name: "write",
            description: write::DESCRIPTION.to_owned(),
            parameters: write::parameters(),
        },
        Spec {
            name: "edit",
            description: edit::DESCRIPTION.to_owned(),
            parameters: edit::parameters(),
        },
        Spec {
            name: "bash",
            description: bash::description(),
            parameters: bash::parameters(),
        },
        Spec {
            name: "say",
            description: SAY_DESCRIPTION.to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string", "description": "The message" } },
                "required": ["text"]
            }),
        },
    ]
}

/// The tools as an OpenAI-compatible request lists them.
pub fn openai(specs: &[Spec]) -> Value {
    Value::Array(
        specs
            .iter()
            .map(|spec| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": spec.parameters,
                    }
                })
            })
            .collect(),
    )
}

/// Run a file tool by name; `None` for a name that is not one (`say` is
/// the loop's).
pub fn run_file_tool(name: &str, args: &Value) -> Option<Outcome> {
    Some(match name {
        "read" => read::run(args),
        "write" => write::run(args),
        "edit" => edit::run(args),
        "bash" => bash::run(args),
        _ => return None,
    })
}
