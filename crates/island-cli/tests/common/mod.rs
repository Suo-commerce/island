//! What the command-line tests share: running the built `island-cli`
//! and reading what it prints.

#![allow(dead_code)]

use std::process::{Command, Output};

pub fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_island-cli"))
        .args(args)
        .output()
        .expect("run island-cli")
}

pub fn ok(args: &[&str]) -> String {
    let output = cli(args);
    assert!(
        output.status.success(),
        "island-cli {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 output")
}

pub fn fails(args: &[&str]) -> String {
    let output = cli(args);
    assert!(!output.status.success(), "island-cli {args:?} succeeded");
    String::from_utf8(output.stderr).expect("UTF-8 output")
}

/// The value after `label` on its line of `output`.
pub fn field(output: &str, label: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .unwrap_or_else(|| panic!("no `{label}` line in:\n{output}"))
        .split_whitespace()
        .next()
        .expect("a value")
        .to_owned()
}

/// The exit code and both outputs.
pub fn outcome(args: &[&str]) -> (i32, String, String) {
    let output = cli(args);
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).expect("UTF-8 output"),
        String::from_utf8(output.stderr).expect("UTF-8 output"),
    )
}

/// Sign a hash printed by the CLI with the key in a key file, as a vault
/// would: plain ed25519 over the 32 bytes.
pub fn sign_by_hand(key_file: &str, hash_hex: &str) -> [u8; 64] {
    let unhex = |text: &str| -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    };
    let seed: [u8; 32] = unhex(std::fs::read_to_string(key_file).expect("key file").trim())
        .try_into()
        .expect("32 bytes");
    let hash: [u8; 32] = unhex(hash_hex).try_into().expect("32 bytes");
    island_core::sign_hash(&island_core::SigningKey::from_bytes(&seed), &hash)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The hashes the CLI asks to have signed, in order.
pub fn hashes(instructions: &str) -> Vec<String> {
    instructions
        .lines()
        .filter_map(|line| line.split_once("hex     "))
        .map(|(_, hash)| hash.trim().to_owned())
        .collect()
}
