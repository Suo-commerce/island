//! The state a log replays to (docs/spec.md, "State and replay").
//!
//! Only the data lives here, so that the gate, authority, the log and the
//! reference model can all read it. How an entry changes the state is the
//! log's replay; the model keeps its own copy of that rule.
//!
//! The state is never stored or shipped. Its hash is how two replays of
//! the same records are compared, so the encoding only has to be
//! canonical, not decodable.

use std::collections::BTreeMap;

use taso_core::canon::{Canon, Canonical};

use crate::record::{Grant, GrantId, Hash, Policy, PublicKey, Refusal, StreamId};

/// Domain string for the state hash.
pub const STATE_DOMAIN: &str = "island.state.v1";

/// What is known about one stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamState {
    /// How many entries the stream holds.
    pub entries: u64,
    /// The `seq` of its newest entry.
    pub last: u64,
}

impl Canonical for StreamState {
    fn canon(&self, out: &mut Canon) {
        out.u64(self.entries);
        out.u64(self.last);
    }
}

/// The answer a writer got for a submission. An exact resend gets it
/// again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Tag 0. The submission is the entry at `seq`.
    Accepted {
        /// The entry's position.
        seq: u64,
        /// The entry's hash.
        hash: Hash,
    },
    /// Tag 1. The gate refused it; the `verdict` entry is at `seq`.
    Refused {
        /// The verdict entry's position.
        seq: u64,
        /// The verdict entry's hash.
        hash: Hash,
        /// Why.
        reason: Refusal,
    },
}

impl Canonical for Answer {
    fn canon(&self, out: &mut Canon) {
        match self {
            Self::Accepted { seq, hash } => {
                out.tag(0);
                out.u64(*seq);
                out.fixed(hash);
            }
            Self::Refused { seq, hash, reason } => {
                out.tag(1);
                out.u64(*seq);
                out.fixed(hash);
                reason.canon(out);
            }
        }
    }
}

/// A writer's latest counted submission: the one number of state per
/// writer that makes a replay refusable and a retry safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Last {
    /// The writer's latest counter.
    pub counter: u64,
    /// The hash of the submission that carried it.
    pub sub_hash: Hash,
    /// The answer it got.
    pub answer: Answer,
}

impl Canonical for Last {
    fn canon(&self, out: &mut Canon) {
        out.u64(self.counter);
        out.fixed(&self.sub_hash);
        self.answer.canon(out);
    }
}

/// What a grant is for. The two grants every log starts with are
/// protected; all others are ordinary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    /// Tag 0. A grant like any other.
    Ordinary = 0,
    /// Tag 1. The root capability: the right to write grants and revokes.
    /// It cannot be revoked, and a narrowing must keep that right.
    Root = 1,
    /// Tag 2. The appender's right to write verdicts. Its holder is the
    /// key every entry's seal is checked against, so it cannot be
    /// narrowed or revoked.
    Appender = 2,
}

/// How a grant stopped being in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ended {
    /// The `seq` of the entry that ended it. Not in force from that entry
    /// on.
    pub at: u64,
    /// The grant that replaced it by narrowing; `None` if it was revoked.
    pub by: Option<GrantId>,
}

impl Canonical for Ended {
    fn canon(&self, out: &mut Canon) {
        out.u64(self.at);
        out.option(self.by.as_ref());
    }
}

/// A grant as the log knows it: what it says, what it is for, and whether
/// it is still in force.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The grant.
    pub grant: Grant,
    /// What it is for. A grant that narrows another takes its role.
    pub role: Role,
    /// Set once the grant is revoked or replaced.
    pub ended: Option<Ended>,
}

impl Held {
    /// Whether the grant is in force.
    pub const fn in_force(&self) -> bool {
        self.ended.is_none()
    }
}

impl Canonical for Held {
    fn canon(&self, out: &mut Canon) {
        self.grant.canon(out);
        out.tag(self.role as u8);
        out.option(self.ended.as_ref());
    }
}

/// Everything judging a new write may depend on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// Per stream: how many entries, and the newest.
    pub streams: BTreeMap<StreamId, StreamState>,
    /// Every grant ever issued, by id, in force or ended.
    pub grants: BTreeMap<GrantId, Held>,
    /// Per writer: its latest counter and the answer it got.
    pub writers: BTreeMap<PublicKey, Last>,
    /// The policy in force: the newest `policy` entry's body, or the empty
    /// policy before the first one.
    pub policy: Policy,
}

impl Canonical for State {
    fn canon(&self, out: &mut Canon) {
        self.streams.canon(out);
        self.grants.canon(out);
        self.writers.canon(out);
        self.policy.canon(out);
    }
}

/// `state_hash`: BLAKE3 in derive-key mode over the state's canonical
/// bytes, like every other Island hash.
pub fn state_hash(state: &State) -> Hash {
    blake3::derive_key(STATE_DOMAIN, &state.canon_bytes())
}
