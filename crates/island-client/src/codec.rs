//! Between Island's own types and the wire's JSON.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use island_core::{
    DecodeError, GrantId, Hash, Held, Kind, PublicKey, Record, Signature, StreamId, Submission,
};

use crate::wire::{AppendRequest, GrantInfo, Position, Records};

/// Why a piece of the wire format could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// A field is not base64, or not of the length its type has.
    Bytes(&'static str),
    /// The bytes are not the canonical form of what they should hold.
    Canonical(&'static str, DecodeError),
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(what) => write!(f, "{what} is not base64 of the right length"),
            Self::Canonical(what, error) => write!(f, "{what} does not decode: {error}"),
        }
    }
}

impl std::error::Error for Malformed {}

/// Standard base64, with padding.
pub fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Bytes from standard base64.
pub fn unb64(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text).ok()
}

/// Exactly `N` bytes from standard base64.
pub fn unb64_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    unb64(text)?.try_into().ok()
}

/// The request that carries a signed submission.
pub fn append_request(submission: &Submission, sig_writer: &Signature) -> AppendRequest {
    AppendRequest {
        submission: b64(&submission.encode()),
        sig_writer: b64(sig_writer),
    }
}

/// The signed submission a request carries. Decoding is strict: bytes
/// that are not the one canonical form of a submission are refused.
pub fn parse_append(request: &AppendRequest) -> Result<(Submission, Signature), Malformed> {
    let bytes = unb64(&request.submission).ok_or(Malformed::Bytes("submission"))?;
    let submission =
        Submission::decode(&bytes).map_err(|error| Malformed::Canonical("submission", error))?;
    let sig_writer = unb64_fixed(&request.sig_writer).ok_or(Malformed::Bytes("sig_writer"))?;
    Ok((submission, sig_writer))
}

/// An entry's place, for the wire.
pub fn position(seq: u64, hash: &Hash) -> Position {
    Position {
        seq,
        hash: b64(hash),
    }
}

/// An entry's place, from the wire.
pub fn parse_position(position: &Position) -> Result<(u64, Hash), Malformed> {
    let hash = unb64_fixed(&position.hash).ok_or(Malformed::Bytes("hash"))?;
    Ok((position.seq, hash))
}

/// A writer's public key, from the wire.
pub fn parse_writer(text: &str) -> Result<PublicKey, Malformed> {
    unb64_fixed(text).ok_or(Malformed::Bytes("writer"))
}

/// Records, for the wire.
pub fn records<'a>(records: impl IntoIterator<Item = &'a Record>, more: bool) -> Records {
    Records {
        records: records
            .into_iter()
            .map(|record| b64(&record.encode()))
            .collect(),
        more,
    }
}

/// Records, from the wire. They are well-formed, not yet authentic:
/// replay them from genesis against the pinned root key before trusting
/// one.
pub fn parse_records(records: &Records) -> Result<Vec<Record>, Malformed> {
    records
        .records
        .iter()
        .map(|text| {
            let bytes = unb64(text).ok_or(Malformed::Bytes("record"))?;
            Record::decode(&bytes).map_err(|error| Malformed::Canonical("record", error))
        })
        .collect()
}

/// Every kind, with its name on the wire and on the command line.
const KINDS: [(Kind, &str); 8] = [
    (Kind::Policy, "policy"),
    (Kind::Grant, "grant"),
    (Kind::Revoke, "revoke"),
    (Kind::Verse, "verse"),
    (Kind::Preference, "preference"),
    (Kind::Verdict, "verdict"),
    (Kind::Genesis, "genesis"),
    (Kind::Bootstrap, "bootstrap"),
];

/// A kind's name.
pub fn kind_name(kind: Kind) -> &'static str {
    KINDS
        .iter()
        .find(|(known, _)| *known == kind)
        .map_or("unknown", |(_, name)| name)
}

/// The kind a name stands for.
pub fn kind_from_name(name: &str) -> Option<Kind> {
    KINDS
        .iter()
        .find(|(_, known)| *known == name)
        .map(|(kind, _)| *kind)
}

/// A stream's name. Stream ids are bytes; any that are not UTF-8 show
/// with the replacement character.
pub fn stream_name(stream: &StreamId) -> String {
    String::from_utf8_lossy(stream.as_bytes()).into_owned()
}

/// A grant as the log holds it, for the wire.
pub fn grant_info(id: GrantId, held: &Held) -> GrantInfo {
    let ended = held.ended;
    GrantInfo {
        id: id.to_string(),
        scope: held.grant.scope.iter().map(stream_name).collect(),
        rights: held
            .grant
            .rights
            .iter()
            .map(|kind| kind_name(*kind).to_owned())
            .collect(),
        narrows: held.grant.narrows.map(|id| id.to_string()),
        revoked_at: ended
            .filter(|ended| ended.by.is_none())
            .map(|ended| ended.at),
        replaced_by: ended.and_then(|ended| ended.by).map(|id| id.to_string()),
    }
}

/// An invariant as it is written in words: `VerseTextMax(4096)`.
pub fn invariant_name(invariant: &island_core::Invariant) -> String {
    use island_core::Invariant;
    match invariant {
        Invariant::VerseNamesEvidence => "VerseNamesEvidence".to_owned(),
        Invariant::VerseTextMax { bytes } => format!("VerseTextMax({bytes})"),
        Invariant::ArtefactHasRef => "ArtefactHasRef".to_owned(),
    }
}

/// Why the gate refused, in words.
pub fn gate_reason(reason: &island_core::Refusal) -> String {
    match reason {
        island_core::Refusal::Invariant(broken) => format!("it breaks {}", invariant_name(broken)),
        island_core::Refusal::WeakerPolicy => {
            "the policy would be weaker than the one in force".to_owned()
        }
    }
}
