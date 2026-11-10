//! The record types and their canonical encoding.
//!
//! Field order in each `Canonical` impl is the wire order. Enums are a `u8`
//! tag followed by the variant's fields; tags are stable, and a later
//! variant takes the next free tag.

use std::collections::BTreeSet;

use taso_core::canon::{Canon, Canonical};

/// A BLAKE3 hash.
pub type Hash = [u8; 32];

/// An ed25519 public key. A writer is named by its key; there is one key
/// per device or session.
pub type PublicKey = [u8; 32];

/// An ed25519 signature.
pub type Signature = [u8; 64];

// ── Streams ─────────────────────────────────────────────────────────

/// The name of a stream: a project, or one of the system streams.
///
/// Opaque bytes on the wire (a `u64` length, then the bytes). Names are
/// UTF-8 by convention only.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamId(Vec<u8>);

impl StreamId {
    /// The system stream that holds `policy` entries.
    pub const POLICY: &'static str = "policy";
    /// The system stream that holds `genesis`, `bootstrap`, `grant` and
    /// `revoke` entries.
    pub const AUTHORITY: &'static str = "authority";
    /// The system stream that holds `verdict` entries.
    pub const GATE: &'static str = "gate";
    /// The system stream for plan-level `preference` entries.
    pub const PLAN: &'static str = "plan";

    /// A stream id from its bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// The raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<&str> for StreamId {
    fn from(name: &str) -> Self {
        Self(name.as_bytes().to_vec())
    }
}

impl Canonical for StreamId {
    fn canon(&self, out: &mut Canon) {
        out.bytes(&self.0);
    }
}

// ── Kinds ───────────────────────────────────────────────────────────

/// What a submission is. The tag is the `u8` on the wire.
///
/// Tags 6 (`receipt`) and 7 (`access`) are reserved for slice 2 and are
/// refused by the decoder until then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Kind {
    /// The invariants in force and the build that judges them.
    Policy = 0,
    /// Authority for one key over some streams and kinds.
    Grant = 1,
    /// The end of a grant.
    Revoke = 2,
    /// Evidence of progress in a project.
    Verse = 3,
    /// A key and a value; never ratcheted.
    Preference = 4,
    /// A submission the gate refused, kept so replay can re-judge it.
    Verdict = 5,
    /// The root key. Entry 0 only.
    Genesis = 8,
    /// The initial grants. Entry 1 only.
    Bootstrap = 9,
}

impl Kind {
    /// The wire tag.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// The kind for a wire tag, if the tag is in use.
    pub const fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::Policy,
            1 => Self::Grant,
            2 => Self::Revoke,
            3 => Self::Verse,
            4 => Self::Preference,
            5 => Self::Verdict,
            8 => Self::Genesis,
            9 => Self::Bootstrap,
            _ => return None,
        })
    }
}

impl Canonical for Kind {
    fn canon(&self, out: &mut Canon) {
        out.tag(self.tag());
    }
}

// ── Verse ───────────────────────────────────────────────────────────

/// The kind of evidence a verse names. A `u8` on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Evidence {
    /// A decision was made.
    Decision = 0,
    /// An artefact exists; its hashes go in the verse's `refs`.
    Artefact = 1,
    /// Something was measured.
    Measurement = 2,
    /// A question was closed.
    ClosedQuestion = 3,
}

impl Evidence {
    /// The wire byte.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The evidence kind a wire byte names, if any.
    pub const fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Decision,
            1 => Self::Artefact,
            2 => Self::Measurement,
            3 => Self::ClosedQuestion,
            _ => return None,
        })
    }
}

/// Evidence of progress in a project.
///
/// `evidence` is kept as the raw wire byte. A byte that names none of the
/// [`Evidence`] kinds still decodes: such a verse "names no evidence", and
/// refusing it is the gate's job (`VerseNamesEvidence`), so the refusal
/// leaves a `verdict` entry. The decoder does not refuse it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verse {
    /// The evidence kind, as its wire byte. See [`Verse::evidence`].
    pub evidence: u8,
    /// What happened. Bounded by the `VerseTextMax` invariant, not here.
    pub text: String,
    /// Hashes the verse points at.
    pub refs: Vec<Hash>,
}

impl Verse {
    /// A verse that names its evidence.
    pub fn new(evidence: Evidence, text: impl Into<String>, refs: Vec<Hash>) -> Self {
        Self {
            evidence: evidence.as_u8(),
            text: text.into(),
            refs,
        }
    }

    /// The evidence kind this verse names, or `None` if the byte names
    /// none.
    pub const fn evidence(&self) -> Option<Evidence> {
        Evidence::from_u8(self.evidence)
    }
}

impl Canonical for Verse {
    fn canon(&self, out: &mut Canon) {
        out.u8(self.evidence);
        out.str(&self.text);
        out.seq(self.refs.iter());
    }
}

// ── Policy ──────────────────────────────────────────────────────────

/// A rule every new write must satisfy while it is in force. Invariants
/// judge new writes only, so tightening never invalidates history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invariant {
    /// Tag 0. A verse names one of the [`Evidence`] kinds.
    VerseNamesEvidence,
    /// Tag 1. A verse's text is at most `bytes` long. May only go down.
    VerseTextMax {
        /// The bound, in bytes of UTF-8.
        bytes: u32,
    },
    /// Tag 2. An artefact verse carries at least one ref.
    ArtefactHasRef,
}

impl Invariant {
    /// The wire tag.
    pub const fn tag(&self) -> u8 {
        match self {
            Self::VerseNamesEvidence => 0,
            Self::VerseTextMax { .. } => 1,
            Self::ArtefactHasRef => 2,
        }
    }
}

impl Canonical for Invariant {
    fn canon(&self, out: &mut Canon) {
        out.tag(self.tag());
        match self {
            Self::VerseNamesEvidence | Self::ArtefactHasRef => {}
            Self::VerseTextMax { bytes } => {
                out.u32(*bytes);
            }
        }
    }
}

/// The body of a `policy` entry: the invariants in force from this entry
/// on, and the build that judges them. The default is the empty policy in
/// force before the first `policy` entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// The git commit of the build that judges.
    pub evaluator: String,
    /// The invariants in force.
    pub invariants: Vec<Invariant>,
}

impl Canonical for Policy {
    fn canon(&self, out: &mut Canon) {
        out.str(&self.evaluator);
        out.seq(self.invariants.iter());
    }
}

// ── Authority ───────────────────────────────────────────────────────

/// The id of a grant: the entry that holds it, and its place in that
/// entry.
///
/// A `grant` entry holds one grant, at index 0. The bootstrap entry holds
/// several, so its grants are `1.0`, `1.1`, and so on. Written and parsed
/// as `entry.index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GrantId {
    /// The `seq` of the entry that holds the grant.
    pub entry: u64,
    /// Its position among that entry's grants.
    pub index: u32,
}

impl GrantId {
    /// The grant at `index` of entry `entry`.
    pub const fn new(entry: u64, index: u32) -> Self {
        Self { entry, index }
    }
}

impl std::fmt::Display for GrantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.entry, self.index)
    }
}

impl std::str::FromStr for GrantId {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        const ERROR: &str = "a grant id is written entry.index, for example 17.0";
        let (entry, index) = text.split_once('.').ok_or(ERROR)?;
        let plain = |digits: &str| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
        if !plain(entry) || !plain(index) {
            return Err(ERROR);
        }
        Ok(Self {
            entry: entry.parse().map_err(|_| ERROR)?,
            index: index.parse().map_err(|_| ERROR)?,
        })
    }
}

impl Canonical for GrantId {
    fn canon(&self, out: &mut Canon) {
        out.u64(self.entry);
        out.u32(self.index);
    }
}

/// Authority for one key: which kinds it may write to which streams.
///
/// The body of a `grant` entry, and the element of the list of initial
/// grants in [`Bootstrap`]. Its id is a [`GrantId`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    /// The key that holds the grant.
    pub holder: PublicKey,
    /// The streams it covers.
    pub scope: BTreeSet<StreamId>,
    /// The kinds it may write there.
    pub rights: BTreeSet<Kind>,
    /// The grant this one replaces, if any. The holder must then be the
    /// same, and scope and rights subsets of that grant's.
    pub narrows: Option<GrantId>,
}

impl Canonical for Grant {
    fn canon(&self, out: &mut Canon) {
        out.fixed(&self.holder);
        out.set(self.scope.iter());
        out.set(self.rights.iter());
        out.option(self.narrows.as_ref());
    }
}

/// The body of a `revoke` entry. Effective from that entry's `seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revoke {
    /// The grant that ends.
    pub grant: GrantId,
}

impl Canonical for Revoke {
    fn canon(&self, out: &mut Canon) {
        self.grant.canon(out);
    }
}

/// The body of entry 0: the root public key. Valid only if it matches the
/// key every replica pins in its config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Genesis {
    /// The root public key.
    pub root: PublicKey,
}

impl Canonical for Genesis {
    fn canon(&self, out: &mut Canon) {
        out.fixed(&self.root);
    }
}

/// The body of entry 1: the initial grants, signed by the root key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bootstrap {
    /// The initial grants, in order.
    pub grants: Vec<Grant>,
}

impl Canonical for Bootstrap {
    fn canon(&self, out: &mut Canon) {
        out.seq(self.grants.iter());
    }
}

// ── Preference ──────────────────────────────────────────────────────

/// The body of a `preference` entry. Never ratcheted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preference {
    /// The key.
    pub key: String,
    /// The value.
    pub value: String,
}

impl Canonical for Preference {
    fn canon(&self, out: &mut Canon) {
        out.str(&self.key);
        out.str(&self.value);
    }
}

// ── Verdict ─────────────────────────────────────────────────────────

/// Why the gate refused a submission.
///
/// One variant so far; a policy that does not strengthen the one in force
/// joins with build step 6, on the next free tag. Authority's refusals
/// leave no entry and are not listed here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Tag 0. This invariant, as it stood in the policy in force, does not
    /// hold for the write.
    Invariant(Invariant),
}

impl Canonical for Refusal {
    fn canon(&self, out: &mut Canon) {
        match self {
            Self::Invariant(invariant) => {
                out.tag(0);
                invariant.canon(out);
            }
        }
    }
}

/// The body of a `verdict` entry: a submission the gate refused, whole,
/// with the writer's signature over it, so replay can re-judge it.
///
/// The rejected submission is never itself a verdict: the appender writes
/// verdicts and the gate does not refuse them. The decoder refuses a
/// nested verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// The refused submission.
    pub rejected: Box<Submission>,
    /// The writer's signature over the refused submission's hash.
    pub sig_writer: Signature,
    /// Why it was refused.
    pub reason: Refusal,
}

impl Canonical for Verdict {
    fn canon(&self, out: &mut Canon) {
        self.rejected.canon(out);
        out.fixed(&self.sig_writer);
        self.reason.canon(out);
    }
}

// ── Submission ──────────────────────────────────────────────────────

/// The kind-specific part of a submission. The variant is the submission's
/// [`Kind`], so a kind and a body can never disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// Tag 0.
    Policy(Policy),
    /// Tag 1.
    Grant(Grant),
    /// Tag 2.
    Revoke(Revoke),
    /// Tag 3.
    Verse(Verse),
    /// Tag 4.
    Preference(Preference),
    /// Tag 5.
    Verdict(Verdict),
    /// Tag 8.
    Genesis(Genesis),
    /// Tag 9.
    Bootstrap(Bootstrap),
}

impl Body {
    /// The kind this body belongs to.
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Policy(_) => Kind::Policy,
            Self::Grant(_) => Kind::Grant,
            Self::Revoke(_) => Kind::Revoke,
            Self::Verse(_) => Kind::Verse,
            Self::Preference(_) => Kind::Preference,
            Self::Verdict(_) => Kind::Verdict,
            Self::Genesis(_) => Kind::Genesis,
            Self::Bootstrap(_) => Kind::Bootstrap,
        }
    }

    /// The body's fields, without the kind tag.
    fn canon_fields(&self, out: &mut Canon) {
        match self {
            Self::Policy(body) => body.canon(out),
            Self::Grant(body) => body.canon(out),
            Self::Revoke(body) => body.canon(out),
            Self::Verse(body) => body.canon(out),
            Self::Preference(body) => body.canon(out),
            Self::Verdict(body) => body.canon(out),
            Self::Genesis(body) => body.canon(out),
            Self::Bootstrap(body) => body.canon(out),
        }
    }
}

/// What a writer signs and submits.
///
/// On the wire: `stream`, `writer`, `counter`, the kind tag, then the
/// body's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    /// The project or system stream the write goes to.
    pub stream: StreamId,
    /// The writer's ed25519 public key.
    pub writer: PublicKey,
    /// This writer's own count: 1, 2, 3, ...
    pub counter: u64,
    /// The kind and its fields.
    pub body: Body,
}

impl Submission {
    /// The submission's kind.
    pub const fn kind(&self) -> Kind {
        self.body.kind()
    }
}

impl Canonical for Submission {
    fn canon(&self, out: &mut Canon) {
        self.stream.canon(out);
        out.fixed(&self.writer);
        out.u64(self.counter);
        self.kind().canon(out);
        self.body.canon_fields(out);
    }
}

// ── Entry and record ────────────────────────────────────────────────

/// A submission in its place in the log: the appender adds position and
/// time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Position in the log, gap-free from 0.
    pub seq: u64,
    /// Microseconds since the epoch on the appender's clock; never less
    /// than the previous entry's.
    pub at: u64,
    /// The `seq` of the policy entry in force when this was judged.
    pub policy: u64,
    /// The hash of entry `seq - 1`; zeros at genesis.
    pub prev: Hash,
    /// What the writer submitted.
    pub submission: Submission,
    /// The writer's signature over the submission's hash.
    pub sig_writer: Signature,
}

impl Canonical for Entry {
    fn canon(&self, out: &mut Canon) {
        out.u64(self.seq);
        out.u64(self.at);
        out.u64(self.policy);
        out.fixed(&self.prev);
        self.submission.canon(out);
        out.fixed(&self.sig_writer);
    }
}

/// What is stored and shipped: an entry, its hash and the appender's
/// signature over that hash.
///
/// On the wire: `canon(Entry) || hash || sig_appender`. A decoded record
/// is only well-formed; [`Record::verify`] says whether it is authentic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The entry.
    pub entry: Entry,
    /// The entry's hash.
    pub hash: Hash,
    /// The appender's signature over `hash`.
    pub sig_appender: Signature,
}

impl Record {
    /// The record's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Canon::new();
        self.entry.canon(&mut out);
        out.fixed(&self.hash);
        out.fixed(&self.sig_appender);
        out.into_bytes()
    }
}
