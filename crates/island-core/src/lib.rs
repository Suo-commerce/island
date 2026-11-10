//! Island core: record types, canonical encoding, hashing and signatures.
//!
//! Everything that enters the Island log is defined here (docs/spec.md,
//! "Record formats"): a writer builds a [`Submission`] and signs its hash;
//! the appender wraps it in an [`Entry`] with position and time, hashes the
//! entry and signs that; the stored and shipped [`Record`] is the entry's
//! canonical bytes followed by the hash and the appender's signature.
//!
//! The encoding is `taso-core`'s canonical form, reused as is through its
//! `Canon` buffer and `Canonical` trait. Decoding is strict, so the
//! encoding is one-to-one: bytes either decode to exactly one value whose
//! encoding is those bytes, or are refused.
//!
//! Pure: no I/O, no clock, no randomness. Keys come in from the caller.

#![warn(missing_docs)]

pub mod decode;
pub mod hash;
pub mod record;
pub mod sign;
pub mod state;

pub use decode::{DecodeError, Reader};
pub use hash::{ENTRY_DOMAIN, SUBMISSION_DOMAIN, entry_hash, sub_hash};
pub use record::{
    Body, Bootstrap, Entry, Evidence, Genesis, Grant, GrantId, Hash, Invariant, Kind, Policy,
    Preference, PublicKey, Record, Refusal, Revoke, Signature, StreamId, Submission, Verdict,
    Verse,
};
pub use sign::{BadSignature, SigningKey, VerifyError, public_key, sign_hash, verify_hash};
pub use state::{Answer, Ended, Held, Last, Role, STATE_DOMAIN, State, StreamState, state_hash};
