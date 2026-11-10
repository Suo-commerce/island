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
