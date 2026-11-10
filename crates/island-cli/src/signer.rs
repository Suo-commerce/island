//! Signing as root.
//!
//! The root key signs the genesis, grants, revokes and policy entries, and
//! nothing else. Two ways to sign:
//!
//! - `--root-key <file>`: a key file, for development logs.
//! - `--root <public key>`: by hand. The command prints the hash to sign
//!   and writes nothing. The hash is signed wherever the key lives (a
//!   vault's signing API, a hardware key), and the same command is run
//!   again with `--signature`.
//!
//! Either way a signature is verified against the root public key before
//! anything is submitted, so one made for another hash, by another key or
//! in another mode is refused here, with the log untouched.

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::Args;
use island_core::{PublicKey, Signature, SigningKey, Submission, public_key};

use crate::Stop;
use crate::keys::{self, hex, unhex};

/// How to sign as root.
#[derive(Args)]
pub struct RootArgs {
    /// Sign with this root key file (development logs).
    #[arg(long, conflicts_with_all = ["root", "signature"])]
    root_key: Option<PathBuf>,
    /// The root public key in hex: sign by hand, wherever the key lives.
    #[arg(long, value_parser = keys::parse_public_key)]
    root: Option<PublicKey>,
    /// A signature made by hand: hex, base64, or as OpenBao Transit
    /// returns it (vault:v1:...). Once per hash, in the order printed.
    #[arg(long, requires = "root")]
    signature: Vec<String>,
}

/// The root key, or the means to use it.
pub enum RootSigner {
    /// A key file.
    KeyFile(SigningKey),
    /// Signatures made elsewhere.
    ByHand {
        /// The root public key.
        root: PublicKey,
        /// The signatures given so far, as typed.
        signatures: Vec<String>,
    },
}

impl RootArgs {
    /// The signer these arguments describe.
    pub fn signer(&self) -> Result<RootSigner, String> {
        match (&self.root_key, self.root) {
            (Some(path), None) => keys::load(path).map(RootSigner::KeyFile),
            (None, Some(root)) => Ok(RootSigner::ByHand {
                root,
                signatures: self.signature.clone(),
            }),
            _ => Err(
                "give either --root-key <file>, or --root <public key> to sign by hand".to_owned(),
            ),
        }
    }
}

impl RootSigner {
    /// The root public key.
    pub fn public(&self) -> PublicKey {
        match self {
            Self::KeyFile(key) => public_key(key),
            Self::ByHand { root, .. } => *root,
        }
    }

    /// One verified signature per submission, in order. By hand and with
    /// no signatures given yet, this stops with what to sign.
    pub fn sign(&self, submissions: &[Submission]) -> Result<Vec<Signature>, Stop> {
        let root = self.public();
        if let Some(other) = submissions
            .iter()
            .find(|submission| submission.writer != root)
        {
            return Err(Stop::Failed(format!(
                "a submission is written by {}, not by the root key",
                hex(&other.writer)
            )));
        }
        match self {
            Self::KeyFile(key) => submissions
                .iter()
                .map(|submission| {
                    submission
                        .sign(key)
                        .ok_or_else(|| Stop::Failed("the root key does not sign".to_owned()))
                })
                .collect(),
            Self::ByHand { signatures, .. } if signatures.is_empty() => {
                Err(Stop::SignFirst(instructions(&root, submissions)))
            }
            Self::ByHand { signatures, .. } => {
                if signatures.len() != submissions.len() {
                    return Err(Stop::Failed(format!(
                        "{} signatures given, {} needed; run without --signature to see \
                         the hashes",
                        signatures.len(),
                        submissions.len()
                    )));
                }
                submissions
                    .iter()
                    .zip(signatures)
                    .map(|(submission, text)| {
                        let signature = parse_signature(text).map_err(Stop::Failed)?;
                        submission.verify(&signature).map_err(|_| {
                            Stop::Failed(format!(
                                "a signature does not verify for hash {} under root key {}.\n\
                                 Either the log has moved on since the hash was printed, or \
                                 island-cli was built again from another commit (a policy \
                                 names its build): run again without --signature. Or it was \
                                 signed with another key, or not as plain ed25519 over the 32 \
                                 bytes (in OpenBao Transit, leave `prehashed` off).",
                                hex(&submission.hash()),
                                hex(&root)
                            ))
                        })?;
                        Ok(signature)
                    })
                    .collect()
            }
        }
    }
}

/// What to sign, and how to come back.
fn instructions(root: &PublicKey, submissions: &[Submission]) -> String {
    let mut out = String::from(
        "Nothing has been written. Sign with the root key, as plain ed25519 over the 32 \
         bytes of each hash:\n\n",
    );
    for (index, submission) in submissions.iter().enumerate() {
        let hash = submission.hash();
        out.push_str(&format!(
            "  {}. {:<9} hex     {}\n               base64  {}\n",
            index + 1,
            format!("{:?}", submission.kind()).to_lowercase(),
            hex(&hash),
            STANDARD.encode(hash)
        ));
    }
    out.push_str(&format!(
        "\nroot key  {}\n\n\
         With OpenBao Transit, send the base64 value as `input` to transit/sign/<key name>\n\
         and leave `prehashed` off.\n\n\
         Then run the same command again, adding one --signature <signature> per hash, in\n\
         this order. A signature may be hex, base64, or the vault:v1:... form.\n",
        hex(root)
    ));
    out
}

/// A signature as hex, base64, or OpenBao Transit's `vault:v1:<base64>`.
pub fn parse_signature(text: &str) -> Result<Signature, String> {
    let text = text.trim();
    if let Some(signature) = unhex::<64>(text) {
        return Ok(signature);
    }
    let encoded = match text.strip_prefix("vault:") {
        Some(versioned) => versioned
            .split_once(':')
            .map_or(versioned, |(_, rest)| rest),
        None => text,
    };
    STANDARD
        .decode(encoded)
        .ok()
        .and_then(|bytes| Signature::try_from(bytes).ok())
        .ok_or_else(|| {
            "a signature is 64 bytes: 128 hex digits, base64, or vault:v1:<base64>".to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_parses_from_every_form() {
        let signature: Signature = std::array::from_fn(|i| i as u8);
        let base64 = STANDARD.encode(signature);
        assert_eq!(parse_signature(&hex(&signature)), Ok(signature));
        assert_eq!(parse_signature(&base64), Ok(signature));
        assert_eq!(
            parse_signature(&format!("vault:v1:{base64}")),
            Ok(signature)
        );
        assert_eq!(
            parse_signature(&format!("  vault:v12:{base64}\n")),
            Ok(signature)
        );

        for bad in ["", "abc", "vault:v1:", &base64[4..], &hex(&signature)[2..]] {
            assert!(parse_signature(bad).is_err(), "{bad:?} parsed");
        }
    }
}
