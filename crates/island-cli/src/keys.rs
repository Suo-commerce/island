//! Hex, and key files.
//!
//! A key file holds one ed25519 seed as 64 hex digits. Key files are for
//! development and for the appender's key on the server. A root key kept
//! in a vault signs by hand instead: see `signer`.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use island_core::{PublicKey, SigningKey};
use island_env::Rng;

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Exactly `N` bytes from `2 * N` hex digits.
pub fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// A public key given on the command line.
pub fn parse_public_key(text: &str) -> Result<PublicKey, String> {
    unhex(text).ok_or_else(|| "a public key is 64 hex digits".to_owned())
}

/// Write a new seed to `path`, readable by its owner only. Refuses to
/// replace a file.
pub fn generate(path: &Path, rng: &mut impl Rng) -> Result<SigningKey, String> {
    let mut seed = [0u8; 32];
    rng.fill(&mut seed)
        .map_err(|error| format!("no random bytes: {error}"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    writeln!(file, "{}", hex(&seed))
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Read a key file.
pub fn load(path: &Path) -> Result<SigningKey, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let seed: [u8; 32] = unhex(text.trim())
        .ok_or_else(|| format!("{}: a key file holds 64 hex digits", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}
