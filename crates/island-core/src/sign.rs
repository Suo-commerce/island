//! Signatures: ed25519 over a 32-byte hash.
//!
//! A writer signs its submission's hash; the appender signs the entry's
//! hash. Signing takes a hash, not a value, so a key that never leaves a
//! vault can sign the same bytes remotely.
//!
//! Verification is strict (`verify_strict`): it refuses small-order keys
//! and non-canonical signatures, so one signed hash has one accepted
//! signature encoding.

use std::fmt;

use ed25519_dalek::{Signer, VerifyingKey};

pub use ed25519_dalek::SigningKey;

use crate::hash::{entry_hash, sub_hash};
use crate::record::{Entry, Hash, PublicKey, Record, Signature, Submission};

/// A signature that does not verify, or a public key that is not a valid
/// ed25519 key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadSignature;

impl fmt::Display for BadSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("signature does not verify")
    }
}

impl std::error::Error for BadSignature {}

/// The public key of a signing key.
pub fn public_key(key: &SigningKey) -> PublicKey {
    key.verifying_key().to_bytes()
}

/// Sign a hash.
pub fn sign_hash(key: &SigningKey, hash: &Hash) -> Signature {
    key.sign(hash).to_bytes()
}

/// Check a signature over a hash against a public key.
pub fn verify_hash(
    public: &PublicKey,
    hash: &Hash,
    signature: &Signature,
) -> Result<(), BadSignature> {
    let key = VerifyingKey::from_bytes(public).map_err(|_| BadSignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(signature);
    key.verify_strict(hash, &signature)
        .map_err(|_| BadSignature)
}

/// Why a record is not authentic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// The stored hash is not the hash of the entry.
    HashMismatch,
    /// The writer's signature over the submission does not verify.
    BadWriterSignature,
    /// The appender's signature over the entry hash does not verify.
    BadAppenderSignature,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::HashMismatch => "stored hash is not the entry's hash",
            Self::BadWriterSignature => "writer signature does not verify",
            Self::BadAppenderSignature => "appender signature does not verify",
        })
    }
}

impl std::error::Error for VerifyError {}

impl Submission {
    /// `sub_hash`: the hash a writer signs.
    pub fn hash(&self) -> Hash {
        sub_hash(self)
    }

    /// `sig_writer`: sign this submission as its writer.
    ///
    /// `None` if `key` is not the key named in `writer`: such a signature
    /// could never verify.
    pub fn sign(&self, key: &SigningKey) -> Option<Signature> {
        (public_key(key) == self.writer).then(|| sign_hash(key, &self.hash()))
    }

    /// Check `sig_writer` against the key named in `writer`.
    pub fn verify(&self, sig_writer: &Signature) -> Result<(), BadSignature> {
        verify_hash(&self.writer, &self.hash(), sig_writer)
    }
}

impl Entry {
    /// `hash`: what the appender signs, and the next entry's `prev`.
    pub fn hash(&self) -> Hash {
        entry_hash(self)
    }

    /// Check the writer's signature over the submission.
    pub fn verify_writer(&self) -> Result<(), BadSignature> {
        self.submission.verify(&self.sig_writer)
    }
}

impl Record {
    /// Hash an entry and sign the hash as the appender.
    pub fn seal(entry: Entry, appender: &SigningKey) -> Self {
        let hash = entry.hash();
        let sig_appender = sign_hash(appender, &hash);
        Self {
            entry,
            hash,
            sig_appender,
        }
    }

    /// Check that the record is authentic: the stored hash is the entry's
    /// hash, the writer signed the submission, and `appender` signed the
    /// hash.
    ///
    /// This is one record on its own. Its place in the chain (`seq`,
    /// `prev`, `at`) is the log's to check.
    pub fn verify(&self, appender: &PublicKey) -> Result<(), VerifyError> {
        if self.entry.hash() != self.hash {
            return Err(VerifyError::HashMismatch);
        }
        self.entry
            .verify_writer()
            .map_err(|_| VerifyError::BadWriterSignature)?;
        verify_hash(appender, &self.hash, &self.sig_appender)
            .map_err(|_| VerifyError::BadAppenderSignature)
    }
}
