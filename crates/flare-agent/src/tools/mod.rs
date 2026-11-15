//! The agent's tools (docs/slice-2.md, "Tools of the loop"): `read`,
//! `write`, `edit` and `bash` work in `/work` and leave the Vessel as
//! nothing; `say` messages Jarmo, as the action `flare.say`; `verse`
//! proposes a verse on the agent's own stream, as the action
//! `island.verse`, which waits for his approval; `fork` starts fork
//! children on alternatives and `promote` adopts one (Taso's `Fork` and
//! `Promote`, asked on the Field).
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

/// The description of `verse`.
pub const VERSE_DESCRIPTION: &str = "Propose a verse on your own stream, flare, in Jarmo's \
Island log: one short entry saying what was decided, made, measured or answered. It waits for \
his approval; approved, it is written once, signed as you. You may write only to flare: a \
verse for one of his projects you draft in your answer, and he writes it himself.";

/// The description of `fork`.
pub const FORK_DESCRIPTION: &str = "Try alternatives in fork children. Each task you give \
starts one child: a copy of this Vessel, with your files and this conversation as they are \
now, working on its task alone with the same tools. Nothing a child does reaches Jarmo or \
your files: its messages and verses are held. This call ends your turn, waits until the \
children are done, and returns each child's note saying what it made. Then either call \
promote with one child's name, or carry on without promoting: any other tool call, or your \
answer, gives the children up for good. A fork slot is used once and never comes back, and \
there are few (the result says how many are left): fork only when comparing real \
alternatives is worth it, usually two. Call it alone, as the last tool call of a message.";

/// The description of `promote`.
pub const PROMOTE_DESCRIPTION: &str = "Adopt one fork child, after fork returned its \
note: its files replace yours, and the messages and verses it held become yours. Its answer \
is sent to Jarmo as the reply to this task, and its verses wait for his approval. You are \
started again on the child's files, so what you wrote since calling fork is lost, and the \
other children are dropped.";

/// The most children one `fork` may start.
pub const MAX_CHILDREN: usize = 4;

/// The longest task a child may be given, in bytes.
pub const MAX_FORK_TASK: usize = 8 * 1024;

/// The stream the agent's verses go to (docs/slice-2.md, decision D3).
pub const OWN_STREAM: &str = "flare";

/// The tools of an agent that may fork: [`specs`], and `fork` and
/// `promote`. A fork child gets [`specs`]: it cannot fork.
pub fn specs_with_forks() -> Vec<Spec> {
    let mut specs = specs();
    specs.push(Spec {
        name: "fork",
        description: FORK_DESCRIPTION.to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": MAX_CHILDREN,
                    "description": "One task per child: what that child should do, complete in itself"
                }
            },
            "required": ["tasks"]
        }),
    });
    specs.push(Spec {
        name: "promote",
        description: PROMOTE_DESCRIPTION.to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "child": { "type": "string", "description": "The child's name, as fork's result gave it" }
            },
            "required": ["child"]
        }),
    });
    specs
}

/// The tools every agent has, as the model sees them.
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
            name: "verse",
            description: VERSE_DESCRIPTION.to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "evidence": {
                        "type": "string",
                        "enum": ["decision", "artefact", "measurement", "closed-question"],
                        "description": "What the verse rests on"
                    },
                    "text": { "type": "string", "description": "The verse, at most 2000 bytes" },
                    "refs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Hashes it points at, 64 hex digits each (an artefact needs one)"
                    }
                },
                "required": ["evidence", "text"]
            }),
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
