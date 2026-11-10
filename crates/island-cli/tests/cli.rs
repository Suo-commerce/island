//! The command line, end to end on real files.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_island-cli"))
        .args(args)
        .output()
        .expect("run island-cli")
}

fn ok(args: &[&str]) -> String {
    let output = cli(args);
    assert!(
        output.status.success(),
        "island-cli {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 output")
}

fn fails(args: &[&str]) -> String {
    let output = cli(args);
    assert!(!output.status.success(), "island-cli {args:?} succeeded");
    String::from_utf8(output.stderr).expect("UTF-8 output")
}

/// The value after `label` on its line of `output`.
fn field(output: &str, label: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .unwrap_or_else(|| panic!("no `{label}` line in:\n{output}"))
        .split_whitespace()
        .next()
        .expect("a value")
        .to_owned()
}

struct Started {
    _dir: tempfile::TempDir,
    log: String,
    root: String,
    appender: String,
}

fn started() -> Started {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = |name: &str| {
        dir.path()
            .join(name)
            .to_str()
            .expect("UTF-8 path")
            .to_owned()
    };
    let (log, root_key, appender_key) =
        (path("island.log"), path("root.key"), path("appender.key"));

    let root = field(&ok(&["keygen", &root_key]), "public");
    let appender = field(&ok(&["keygen", &appender_key]), "public");
    let init = ok(&[
        "init",
        "--log",
        &log,
        "--root-key",
        &root_key,
        "--appender-key",
        &appender_key,
    ]);
    assert_eq!(field(&init, "root"), root);
    assert_eq!(field(&init, "appender"), appender);
    assert!(init.contains("entry 0   genesis"));
    assert!(init.contains("entry 1   bootstrap"));

    Started {
        _dir: dir,
        log,
        root,
        appender,
    }
}

#[test]
fn keygen_writes_a_private_file_and_never_replaces_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("dev.key");
    let file = file.to_str().expect("UTF-8 path");

    let public = field(&ok(&["keygen", file]), "public");
    assert_eq!(public.len(), 64);
    let mode = std::fs::metadata(file)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        std::fs::read_to_string(file).expect("read").trim().len(),
        64
    );

    assert!(fails(&["keygen", file]).contains("exists"));
}

#[test]
fn init_then_verify_replay_and_tail() {
    let started = started();
    let at = ["--log", &started.log, "--root", &started.root];

    let verify = ok(&[&["verify"], &at[..]].concat());
    assert!(verify.contains("ok: 2 entries verified from genesis"));
    assert_eq!(field(&verify, "appender"), started.appender);

    let replay = ok(&[&["replay"], &at[..]].concat());
    assert!(replay.contains("2 entries replayed from genesis"));
    assert!(replay.contains("policy      none yet"));
    assert!(replay.contains("authority     2 entries, newest 1"));
    assert!(replay.contains(&format!("{}  counter 2, accepted as entry 1", started.root)));
    // Verify and replay report the same state hash.
    assert_eq!(field(&verify, "state hash"), field(&replay, "state hash"));

    let tail = ok(&[&["tail"], &at[..]].concat());
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("genesis") && lines[0].contains(&started.root));
    assert!(lines[1].contains("bootstrap") && lines[1].contains("may write verdict on gate"));

    let last = ok(&[&["tail", "-n", "1"], &at[..]].concat());
    assert_eq!(last.lines().count(), 1);
    assert!(last.contains("bootstrap"));
}

#[test]
fn init_refuses_an_existing_log_and_a_shared_key() {
    let started = started();
    let dir = Path::new(&started.log).parent().expect("a directory");
    let root_key = dir.join("root.key");
    let root_key = root_key.to_str().expect("UTF-8 path");
    let appender_key = dir.join("appender.key");
    let appender_key = appender_key.to_str().expect("UTF-8 path");

    let again = fails(&[
        "init",
        "--log",
        &started.log,
        "--root-key",
        root_key,
        "--appender-key",
        appender_key,
    ]);
    assert!(again.contains("exists"));

    let other = dir.join("other.log");
    let shared = fails(&[
        "init",
        "--log",
        other.to_str().expect("UTF-8 path"),
        "--root-key",
        root_key,
        "--appender-key",
        root_key,
    ]);
    assert!(shared.contains("must not be the root key"));
    assert!(!other.exists(), "a refused init left a log behind");
}

#[test]
fn a_log_does_not_verify_under_another_root() {
    let started = started();
    let error = fails(&["verify", "--log", &started.log, "--root", &started.appender]);
    assert!(error.contains("entry 0"));
    assert!(error.contains("not the pinned one"));
}

#[test]
fn a_damaged_log_fails_every_reading_command_and_names_the_entry() {
    let started = started();
    let mut bytes = std::fs::read(&started.log).expect("read");
    let last = bytes.len() - 100; // inside entry 1
    bytes[last] ^= 1;
    std::fs::write(&started.log, &bytes).expect("write");

    for command in ["verify", "replay", "tail"] {
        let error = fails(&[command, "--log", &started.log, "--root", &started.root]);
        assert!(error.contains("entry 1"), "{command}: {error}");
        let output = cli(&[command, "--log", &started.log, "--root", &started.root]);
        assert!(
            output.stdout.is_empty(),
            "{command} printed from a damaged log"
        );
    }
}

#[test]
fn a_torn_tail_is_reported_and_left_alone() {
    let started = started();
    let mut bytes = std::fs::read(&started.log).expect("read");
    bytes.extend([1, 2, 3]); // the start of a frame header, cut short
    std::fs::write(&started.log, &bytes).expect("write");

    let output = cli(&["verify", "--log", &started.log, "--root", &started.root]);
    assert!(output.status.success());
    let note = String::from_utf8(output.stderr).expect("UTF-8");
    assert!(note.contains("3 bytes of an unfinished last record"));
    assert_eq!(std::fs::read(&started.log).expect("read"), bytes);
}

#[test]
fn a_bad_root_argument_is_a_usage_error() {
    let error = fails(&["verify", "--log", "x", "--root", "abc"]);
    assert!(error.contains("64 hex digits"));
}

// ── Grants ──────────────────────────────────────────────────────────

/// The exit code and both outputs.
fn outcome(args: &[&str]) -> (i32, String, String) {
    let output = cli(args);
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).expect("UTF-8 output"),
        String::from_utf8(output.stderr).expect("UTF-8 output"),
    )
}

fn sibling(started: &Started, name: &str) -> String {
    Path::new(&started.log)
        .parent()
        .expect("a directory")
        .join(name)
        .to_str()
        .expect("UTF-8 path")
        .to_owned()
}

/// A command line: the command, its common arguments, then the rest.
fn joined<'a>(command: &'a str, base: &[&'a str], extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![command];
    args.extend(base);
    args.extend(extra);
    args
}

const HOLDER: &str = "4444444444444444444444444444444444444444444444444444444444444444";

#[test]
fn grant_narrow_refuse_a_widening_and_revoke_with_a_key_file() {
    let started = started();
    let (root_key, appender_key) = (
        sibling(&started, "root.key"),
        sibling(&started, "appender.key"),
    );
    let base = [
        "--log",
        &started.log,
        "--appender-key",
        &appender_key,
        "--root-key",
        &root_key,
    ];
    let to_holder = [&base[..], &["--holder", HOLDER]].concat();
    let read = ["--log", &started.log, "--root", &started.root];

    // A grant for the holder on two streams. Its id is the entry's.
    let out = ok(&joined(
        "grant",
        &to_holder,
        &["--scope", "island,flare", "--rights", "verse,preference"],
    ));
    assert_eq!(out.trim(), "entry 2  grant 2.0");

    // Narrowed in place to one stream and one kind.
    let out = ok(&joined(
        "grant",
        &to_holder,
        &["--scope", "island", "--rights", "verse", "--narrows", "2.0"],
    ));
    assert_eq!(out.trim(), "entry 3  grant 3.0");

    // A widening by way of --narrows is refused and writes nothing.
    let error = fails(&joined(
        "grant",
        &to_holder,
        &[
            "--scope",
            "island,flare",
            "--rights",
            "verse",
            "--narrows",
            "3.0",
        ],
    ));
    assert!(error.contains("widening takes a new grant"), "{error}");
    // So is a right no grant may carry, and a grant that is not there.
    let error = fails(&joined(
        "grant",
        &to_holder,
        &["--scope", "authority", "--rights", "grant"],
    ));
    assert!(
        error.contains("may not carry the right to write grant entries"),
        "{error}"
    );
    let error = fails(&joined(
        "grant",
        &to_holder,
        &["--scope", "island", "--rights", "verse", "--narrows", "9.0"],
    ));
    assert!(error.contains("there is no grant 9.0"), "{error}");
    assert!(ok(&[&["verify"], &read[..]].concat()).contains("ok: 4 entries"));

    // The two grants every log starts with are protected.
    for id in ["1.0", "1.1"] {
        let error = fails(&joined("revoke", &base, &["--grant", id]));
        assert!(
            error.contains(&format!("grant {id} is protected")),
            "{error}"
        );
    }

    // Revoke the narrowed grant; revoking it twice is refused.
    assert_eq!(
        ok(&joined("revoke", &base, &["--grant", "3.0"])).trim(),
        "entry 4  revoked grant 3.0"
    );
    assert!(
        fails(&joined("revoke", &base, &["--grant", "3.0"]))
            .contains("grant 3.0 is no longer in force")
    );

    let listing = ok(&[&["grants"], &read[..]].concat());
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), 4, "{listing}");
    assert!(lines[0].starts_with("1.0 ") && lines[0].ends_with("(in force)  [root capability]"));
    assert!(lines[0].contains("policy grant revoke on authority policy"));
    assert!(lines[1].starts_with("1.1 ") && lines[1].ends_with("(in force)  [appender]"));
    assert!(lines[2].starts_with("2.0 ") && lines[2].contains(HOLDER));
    assert!(lines[2].contains("verse preference on flare island"));
    assert!(lines[2].ends_with("(replaced by 3.0 at entry 3)"));
    assert!(lines[3].contains("verse on island") && lines[3].ends_with("(revoked at entry 4)"));

    assert!(ok(&[&["verify"], &read[..]].concat()).contains("ok: 5 entries"));
    assert!(ok(&[&["tail", "-n", "1"], &read[..]].concat()).contains("revoke"));
}

/// Sign a hash printed by the CLI with the key in a key file, as a vault
/// would: plain ed25519 over the 32 bytes.
fn sign_by_hand(key_file: &str, hash_hex: &str) -> [u8; 64] {
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The hashes the CLI asks to have signed, in order.
fn hashes(instructions: &str) -> Vec<String> {
    instructions
        .lines()
        .filter_map(|line| line.split_once("hex     "))
        .map(|(_, hash)| hash.trim().to_owned())
        .collect()
}

#[test]
fn a_grant_signed_by_hand_takes_two_runs_and_writes_nothing_in_the_first() {
    use base64::Engine;

    let started = started();
    let (root_key, appender_key) = (
        sibling(&started, "root.key"),
        sibling(&started, "appender.key"),
    );
    let command = [
        "grant",
        "--log",
        &started.log,
        "--appender-key",
        &appender_key,
        "--root",
        &started.root,
        "--holder",
        HOLDER,
        "--scope",
        "island",
        "--rights",
        "verse",
    ];
    let before = std::fs::read(&started.log).expect("read");

    // First run: the hash to sign, exit code 3, the log untouched.
    let (code, instructions, _) = outcome(&command);
    assert_eq!(code, 3);
    assert!(instructions.contains("Nothing has been written"));
    assert!(instructions.contains("leave `prehashed` off"));
    let hash = hashes(&instructions).pop().expect("one hash");
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&hash[i..i + 2], 16).expect("hex"))
            .collect::<Vec<u8>>(),
    );
    assert!(instructions.contains(&format!("base64  {encoded}")));
    assert_eq!(std::fs::read(&started.log).expect("read"), before);

    // A signature by another key, or over another hash, is refused before
    // anything is written.
    let other_key = sibling(&started, "appender.key");
    for wrong in [
        hex(&sign_by_hand(&other_key, &hash)),
        hex(&sign_by_hand(&root_key, &"00".repeat(32))),
    ] {
        let (code, _, error) = outcome(&[&command[..], &["--signature", &wrong]].concat());
        assert_eq!(code, 1);
        assert!(error.contains("does not verify"), "{error}");
        assert_eq!(std::fs::read(&started.log).expect("read"), before);
    }

    // Second run with the signature in the form OpenBao Transit returns.
    let signature = sign_by_hand(&root_key, &hash);
    let transit = format!(
        "vault:v1:{}",
        base64::engine::general_purpose::STANDARD.encode(signature)
    );
    let out = ok(&[&command[..], &["--signature", &transit]].concat());
    assert_eq!(out.trim(), "entry 2  grant 2.0");
    assert!(
        ok(&["verify", "--log", &started.log, "--root", &started.root]).contains("ok: 3 entries")
    );

    // The same signature again is for a hash the log has moved past.
    let (code, _, error) = outcome(&[&command[..], &["--signature", &transit]].concat());
    assert_eq!(code, 1);
    assert!(error.contains("the log has moved on"), "{error}");
}

#[test]
fn a_log_can_be_started_with_the_root_key_signing_by_hand() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = |name: &str| {
        dir.path()
            .join(name)
            .to_str()
            .expect("UTF-8 path")
            .to_owned()
    };
    let (log, root_key, appender_key) =
        (path("island.log"), path("root.key"), path("appender.key"));
    let root = field(&ok(&["keygen", &root_key]), "public");
    ok(&["keygen", &appender_key]);
    let command = [
        "init",
        "--log",
        &log,
        "--appender-key",
        &appender_key,
        "--root",
        &root,
    ];

    // First run: two hashes, and no log file yet.
    let (code, instructions, _) = outcome(&command);
    assert_eq!(code, 3);
    let hashes = hashes(&instructions);
    assert_eq!(hashes.len(), 2);
    assert!(
        !Path::new(&log).exists(),
        "the log was created before it was signed"
    );

    // One signature is not enough.
    let first = hex(&sign_by_hand(&root_key, &hashes[0]));
    let second = hex(&sign_by_hand(&root_key, &hashes[1]));
    let (code, _, error) = outcome(&[&command[..], &["--signature", &first]].concat());
    assert_eq!(code, 1);
    assert!(error.contains("1 signatures given, 2 needed"), "{error}");
    assert!(!Path::new(&log).exists());

    let out = ok(&[
        &command[..],
        &["--signature", &first, "--signature", &second],
    ]
    .concat());
    assert!(out.contains("entry 1   bootstrap"));
    assert!(ok(&["verify", "--log", &log, "--root", &root]).contains("ok: 2 entries"));
}

#[test]
fn root_arguments_must_say_one_way_to_sign() {
    let started = started();
    let appender_key = sibling(&started, "appender.key");
    let command = [
        "revoke",
        "--log",
        &started.log,
        "--appender-key",
        &appender_key,
        "--grant",
        "1.0",
    ];
    assert!(fails(&command).contains("--root-key <file>, or --root <public key>"));
    let root_key = sibling(&started, "root.key");
    let both = [
        &command[..],
        &["--root-key", &root_key, "--root", &started.root],
    ]
    .concat();
    assert!(fails(&both).contains("cannot be used with"));
    let bad_id = [
        "revoke",
        "--log",
        &started.log,
        "--appender-key",
        &appender_key,
        "--grant",
        "7",
    ];
    assert!(fails(&bad_id).contains("entry.index"));
}
