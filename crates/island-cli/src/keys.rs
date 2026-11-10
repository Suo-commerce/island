//! Hex, and key files.
//!
//! A key file holds one ed25519 seed as 64 hex digits. Key files are for
//! development and for the appender's key on the server. A root key kept
//! in a vault signs by hand instead: see `signer`.

use std::path::Path;

use island_core::{PublicKey, SigningKey};
use island_env::{Rng, keyfile};

pub use island_env::keyfile::{hex, unhex};

/// A public key given on the command line.
pub fn parse_public_key(text: &str) -> Result<PublicKey, String> {
    unhex(text).ok_or_else(|| "a public key is 64 hex digits".to_owned())
}

/// Write a new seed to `path`, readable by its owner only. Refuses to
/// replace a file.
pub fn generate(path: &Path, rng: &mut impl Rng) -> Result<SigningKey, String> {
    keyfile::generate(path, rng).map(|seed| SigningKey::from_bytes(&seed))
}

/// Read a key file.
pub fn load(path: &Path) -> Result<SigningKey, String> {
    keyfile::load(path).map(|seed| SigningKey::from_bytes(&seed))
}
