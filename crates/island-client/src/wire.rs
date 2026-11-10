//! The broker's wire format, version 1 (docs/spec.md, "Interfaces").
//!
//! JSON, in and out. Everything binary travels as standard base64 with
//! padding: a submission and a record as their canonical bytes, a key, a
//! signature and a hash as their raw bytes. A grant id is written `3.0`,
//! a stream and a kind by name.
//!
//! The same types serve the broker's three MCP tools and its two plain
//! HTTP paths, so there is one decoder:
//!
//! | Where | In | Out |
//! | --- | --- | --- |
//! | tool `append` | [`AppendRequest`] | [`Position`], or [`Refusal`] as a tool error |
//! | tool `read_stream` | [`ReadStreamRequest`] | [`Records`] |
//! | tool `my_capabilities` | [`CapabilitiesRequest`] | [`Capabilities`] |
//! | `GET /log?after=&limit=` | [`LogQuery`] | [`Records`] |
//! | `GET /head` | | [`Head`] |
//!
//! `tests/golden/` pins the JSON of each.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The version of this format. [`Head`] carries it.
pub const WIRE_VERSION: u32 = 1;

/// The most records one answer holds.
pub const MAX_LIMIT: u32 = 500;

/// How many records an answer holds when no limit is asked for.
pub const DEFAULT_LIMIT: u32 = 100;

/// Input of `append`: a submission and its writer's signature. The signed
/// submission is the credential; there is no other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppendRequest {
    /// The submission's canonical bytes, base64.
    pub submission: String,
    /// The writer's ed25519 signature over the submission's hash, base64.
    pub sig_writer: String,
}

/// An entry's place in the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Position {
    /// The entry's position.
    pub seq: u64,
    /// The entry's hash, base64.
    pub hash: String,
}

/// Why a submission did not become an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Code {
    /// The request does not hold a canonical submission and a signature.
    Malformed,
    /// The writer's signature does not verify.
    BadSignature,
    /// The counter was already used.
    CounterOld,
    /// The counter skips ahead.
    CounterGap,
    /// The counter is the writer's latest, with different content.
    CounterConflict,
    /// The gate refused it. A verdict entry records that, and the
    /// writer's counter has advanced.
    Gate,
    /// No grant allows the write, or the grant or revoke it carries is
    /// not allowed.
    Denied,
    /// A verdict from outside. Only the appender's own gate writes them.
    VerdictFromOutside,
    /// It would not be a valid next entry for another reason.
    Invalid,
    /// The record would be larger than the log allows.
    TooLarge,
    /// The broker cannot write right now. Nothing is known about this
    /// submission; send it again later, unchanged.
    Unavailable,
}

/// A refusal. The `append` tool returns it as a tool error, under the key
/// `refused`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    /// Which rule refused it.
    pub code: Code,
    /// The same in words.
    pub reason: String,
    /// The verdict entry, when the gate refused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Position>,
    /// The counter the writer's next submission must carry, when the
    /// counter was the problem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_counter: Option<u64>,
}

/// The shape a refusal has inside a tool error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Refused {
    /// The refusal.
    pub refused: Refusal,
}

/// Input of `read_stream`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadStreamRequest {
    /// The stream's name.
    pub stream: String,
    /// Return only entries after this position. Leave out to start at the
    /// stream's first entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// The most records to return, at most 500. 100 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// The query of `GET /log`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogQuery {
    /// Return only entries after this position. Leave out to start at
    /// entry 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// The most records to return, at most 500. 100 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Records, in log order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Records {
    /// Each record's bytes, base64.
    pub records: Vec<String>,
    /// Whether there are more after these.
    pub more: bool,
}

/// Input of `my_capabilities`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesRequest {
    /// The writer's public key, base64.
    pub writer: String,
}

/// One grant a writer holds or held.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantInfo {
    /// The grant's id: the entry that holds it and its index there,
    /// written `3.0`.
    pub id: String,
    /// The streams it covers.
    pub scope: Vec<String>,
    /// The kinds it lets the writer write.
    pub rights: Vec<String>,
    /// The grant it replaced by narrowing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrows: Option<String>,
    /// The entry that revoked it. Not in force from that entry on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    /// The grant that replaced it by narrowing. Not in force from that
    /// grant's entry on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
}

/// What a writer may do, and the counter its next submission carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Every grant the writer holds or held, by id.
    pub grants: Vec<GrantInfo>,
    /// The counter the writer's next submission must carry.
    pub next_counter: u64,
}

/// The newest entry, and who is answering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Head {
    /// The newest entry's position.
    pub seq: u64,
    /// The newest entry's hash, base64.
    pub hash: String,
    /// The git commit the broker was built from: the build that judges.
    pub build: String,
    /// The wire format's version, [`WIRE_VERSION`].
    pub wire: u32,
}
