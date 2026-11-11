//! Flare's session protocol, version 1 (docs/spec.md, "Flare's session
//! protocol").
//!
//! A session is an append-only list of events on `flare-server`. Clients
//! attach at a cursor, get every event after it, and then every new one as
//! it happens; the server gives each event its place, so two clients
//! typing at once never conflict. Messages are JSON, one per WebSocket
//! text message.
//!
//! ```text
//! client → server   attach   { session, after?, client }
//! client → server   command  { id, verb: "see" | "propose", args }
//! server → client   attached { session, next }
//! server → client   event    { seq, at, kind, payload }
//!    kinds: user_message, agent_text, verb_call, verb_result, error
//! ```
//!
//! A `propose` carries a submission the client signed with its own key;
//! the server only relays it to the broker. A `see` is answered from the
//! server's own verified replica, and names the head it was answered at,
//! so a client with a replica of its own can check it.

use serde::{Deserialize, Serialize};

/// The version of this protocol.
pub const SESSION_VERSION: u32 = 1;

/// How many events an attach without a cursor replays.
pub const RECENT: u64 = 20;

/// Whether `name` may name a session: 1 to 64 of `a-z`, `0-9` and `-`.
pub fn valid_session(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// What a client sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    /// Join a session. Sent once, first.
    Attach {
        /// The session's name.
        session: String,
        /// Replay the events after this one. Left out: the newest
        /// [`RECENT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<u64>,
        /// What the client calls itself in the session: `laptop`, `ssh`.
        client: String,
    },
    /// Ask the server to do something. Its call and its result become
    /// events every attached client sees.
    Command {
        /// The client's own name for the command, echoed in its events.
        id: String,
        /// What to do.
        verb: Verb,
        /// The verb's arguments: [`SeeArgs`] or [`ProposeArgs`].
        args: serde_json::Value,
    },
}

/// What a client can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    /// Show the log: the overview, or one stream.
    See,
    /// Hand a signed submission to the broker.
    Propose,
}

/// Arguments of `see`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeeArgs {
    /// The stream to show; left out, the overview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    /// How many entries of the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<usize>,
}

/// Arguments of `propose`: a submission and its writer's signature, as
/// the broker's `append` tool takes them, base64.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeArgs {
    /// The submission's canonical bytes.
    pub submission: String,
    /// The writer's signature over the submission's hash.
    pub sig_writer: String,
}

/// What the server sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    /// The attach worked. Replayed events follow, then new ones.
    Attached {
        /// The session's name.
        session: String,
        /// The place the next new event will take.
        next: u64,
        /// The protocol's version, [`SESSION_VERSION`].
        version: u32,
    },
    /// One event of the session.
    Event(Event),
    /// The client did something the protocol does not allow; the server
    /// closes the connection after this.
    Refused {
        /// Why.
        reason: String,
    },
}

/// One event of a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// Its place in the session, from 0.
    pub seq: u64,
    /// When the server recorded it, in microseconds since the epoch.
    pub at: u64,
    /// What kind of event.
    pub kind: EventKind,
    /// What it holds: [`VerbCall`], [`VerbResult`] or [`Problem`] by kind.
    pub payload: serde_json::Value,
}

/// The kinds of event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A person said something. Not used in slice 1.
    UserMessage,
    /// The agent said something. Not used in slice 1.
    AgentText,
    /// A client asked for a verb.
    VerbCall,
    /// What came of it.
    VerbResult,
    /// Something went wrong with a command.
    Error,
}

/// The payload of a `verb_call`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerbCall {
    /// The command's id.
    pub id: String,
    /// The client that asked.
    pub client: String,
    /// The verb.
    pub verb: Verb,
    /// What was asked for, in words: `see island`, or the verse proposed.
    pub what: String,
}

/// An entry's place in the log, as a result names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    /// Its position.
    pub seq: u64,
    /// Its hash, in hex.
    pub hash: String,
}

/// The payload of a `verb_result`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerbResult {
    /// The command's id.
    pub id: String,
    /// Whether it did what was asked: shown, or written.
    pub ok: bool,
    /// What came of it, as the server shows it.
    pub text: String,
    /// For `see`: the replica's head the text was made from. For a
    /// `propose` that was written: the new entry. For one the gate
    /// refused: the verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Place>,
}

/// The payload of an `error`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    /// The command's id, if the problem was with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// What went wrong.
    pub text: String,
}
