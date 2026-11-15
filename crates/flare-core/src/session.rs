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
//! client → server   attach   { session, after?, client, version? }
//! client → server   command  { id, verb: "see" | "propose" | "say", args }
//! server → client   attached { session, next, version }
//! server → client   event    { seq, at, kind, payload }
//!    kinds: user_message, agent_text, verb_call, verb_result, error
//! ```
//!
//! Version 2 (docs/slice-2.md, "Session protocol, version 2") adds `say`:
//! a message for the agent, recorded as a `user_message` whose place is
//! the task's id. The agent's reply comes back as an `agent_text` naming
//! that task. A client says which version it speaks when it attaches;
//! the server answers with the version it will use. Its other events:
//! `action_waiting` and `action_settled` for an action that needs Jarmo's
//! answer, and `fork_note` for what a fork child of the agent made.
//!
//! A `propose` carries a submission the client signed with its own key;
//! the server only relays it to the broker. A `see` is answered from the
//! server's own verified replica, and names the head it was answered at,
//! so a client with a replica of its own can check it.

use serde::{Deserialize, Serialize};

/// The newest version of this protocol.
pub const SESSION_VERSION: u32 = 2;

/// The longest message a `say` may carry, in bytes.
pub const MAX_SAY: usize = 64 * 1024;

/// The longest reply the agent may send, in bytes (docs/slice-2.md,
/// "Actions and the sender": `flare.say` is at most 256 KiB).
pub const MAX_REPLY: usize = 256 * 1024;

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
        /// The protocol version it speaks. Left out: 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u32>,
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
    /// Tell the agent something (version 2).
    Say,
    /// Approve an action waiting for Jarmo: an `approval` the client
    /// signed, relayed as `propose` relays (version 2). Arguments:
    /// [`ProposeArgs`].
    Approve,
    /// Decline one, the same way.
    Decline,
}

/// Arguments of `say`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SayArgs {
    /// The message, at most [`MAX_SAY`] bytes.
    pub text: String,
}

/// The payload of a `user_message`: something said to the agent. Its
/// event's place is the task's id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserMessage {
    /// The command's id.
    pub id: String,
    /// The client that said it.
    pub client: String,
    /// What was said.
    pub text: String,
}

/// The payload of an `agent_text`: the agent's reply, delivered by
/// `island-sender` as the action `flare.say`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentText {
    /// The task it answers: the place of its `user_message`.
    pub task: u64,
    /// The reply.
    pub text: String,
    /// The Vessel that said it, in hex.
    pub vessel: String,
    /// The action's key, in hex: the session keeps one event per key.
    pub key: String,
}

/// The request bytes of the action `flare.say`, as the agent declares it
/// and `flare-server` reads it: JSON, at most [`MAX_REPLY`] bytes of text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SayRequest {
    /// The task it answers.
    pub task: u64,
    /// The reply.
    pub text: String,
}

/// The payload of an `action_waiting`: an action a Vessel declared that
/// needs Jarmo's approval. A client recomputes the request hash from
/// `provider` and `request` before it signs an approval of it, so what is
/// approved is what is shown, not the server's word.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionWaiting {
    /// The Vessel, in hex.
    pub vessel: String,
    /// Its op in the Vessel's Chronicle.
    pub op: u64,
    /// The provider: `island.verse`.
    pub provider: String,
    /// The request hash an approval names, in hex.
    pub request_hash: String,
    /// The exact request bytes, in hex.
    pub request: String,
    /// The request as people read it.
    pub shown: String,
}

/// How an action was settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Settled {
    /// Sent: `entry` is what it wrote (a verse's entry).
    Sent,
    /// Declined: `entry` is the decline.
    Declined,
    /// Approved, but refused where it was sent: `why`.
    Refused,
}

/// The payload of an `action_settled`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSettled {
    /// The request hash, in hex.
    pub request_hash: String,
    /// How.
    pub settled: Settled,
    /// The Island log's entry that says so, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<u64>,
    /// Why it was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// What `island-sender` tells `flare-server` about an action that needs
/// approval, each time it changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReport {
    /// The Vessel, in hex.
    pub vessel: String,
    /// Its op.
    pub op: u64,
    /// The provider.
    pub provider: String,
    /// The request hash, in hex.
    pub request_hash: String,
    /// The request bytes, in hex.
    pub request: String,
    /// `waiting`, `sent`, `declined` or `refused`.
    pub state: String,
    /// The entry, for `sent` and `declined`.
    #[serde(default)]
    pub entry: Option<u64>,
    /// Why, for `refused`.
    #[serde(default)]
    pub why: Option<String>,
}

/// The longest note a fork child may post, in bytes (docs/slice-2.md,
/// "Forking, step by step": at most 16 KiB).
pub const MAX_NOTE: usize = 16 * 1024;

/// The payload of a `fork_note`, and what the island socket posts to
/// `flare-server` for one: a fork child of the agent's Vessel saying what
/// it made. A child's files and actions stay in its own Vessel unless the
/// agent promotes it; the note is all that leaves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkNote {
    /// The child's name: `<subject>-fork-<slot>`.
    pub child: String,
    /// The note, at most [`MAX_NOTE`] bytes.
    pub text: String,
    /// Whether the child is done.
    pub done: bool,
}

/// A task as `flare-server` hands it to the island socket, and the
/// island socket to the agent: the next `user_message` after a cursor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// Its id: the place of its `user_message`.
    pub id: u64,
    /// When it was said, in microseconds since the epoch.
    pub at: u64,
    /// The client that said it.
    pub client: String,
    /// What was said.
    pub text: String,
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
    /// A person said something to the agent: [`UserMessage`].
    UserMessage,
    /// The agent replied: [`AgentText`].
    AgentText,
    /// An action waits for Jarmo's answer: [`ActionWaiting`].
    ActionWaiting,
    /// An action was answered and settled: [`ActionSettled`].
    ActionSettled,
    /// A fork child of the agent posted a note: [`ForkNote`].
    ForkNote,
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
