//! Hashing: BLAKE3 in derive-key mode, one domain string per hashed type.
//!
//! This is how Taso hashes its records (`taso-core`, `envelope`): the
//! domain string is BLAKE3's key-derivation context and the canonical
//! bytes are the input, so two types can never collide even when their
//! bytes happen to match. The domain string carries the version; changing
//! an encoding means a new domain.

use taso_core::canon::Canonical;

use crate::record::{Entry, Hash, Submission};

/// Domain string for a submission's hash, the message a writer signs.
pub const SUBMISSION_DOMAIN: &str = "island.submission.v1";

/// Domain string for an entry's hash, the message the appender signs and
/// the next entry's `prev`.
pub const ENTRY_DOMAIN: &str = "island.entry.v1";

/// `sub_hash`: the hash of a submission's canonical bytes.
pub fn sub_hash(submission: &Submission) -> Hash {
    blake3::derive_key(SUBMISSION_DOMAIN, &submission.canon_bytes())
}

/// `hash`: the hash of an entry's canonical bytes.
pub fn entry_hash(entry: &Entry) -> Hash {
    blake3::derive_key(ENTRY_DOMAIN, &entry.canon_bytes())
}
