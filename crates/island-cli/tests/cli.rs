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
    assert!(init.contains("entry 2   policy"));

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
    assert!(verify.contains("ok: 3 entries verified from genesis"));
    assert_eq!(field(&verify, "appender"), started.appender);

    let replay = ok(&[&["replay"], &at[..]].concat());
    assert!(replay.contains("3 entries replayed from genesis"));
    // The first policy is in force from entry 2, and names its build.
    let policy = replay
        .lines()
        .find(|line| line.starts_with("policy "))
        .expect("a policy line");
    assert!(policy.contains("entry 2, evaluator \""), "{policy}");
    assert!(policy.ends_with("invariants: VerseNamesEvidence, VerseTextMax(4096)"));
    assert!(
        !policy.contains("evaluator \"\""),
        "no build recorded: {policy}"
    );
    assert!(replay.contains("authority     2 entries, newest 1"));
    assert!(replay.contains("policy        1 entries, newest 2"));
    assert!(replay.contains(&format!("{}  counter 3, accepted as entry 2", started.root)));
    assert!(replay.contains("verdicts           0\noverride requests  0\n"));
    // Verify and replay report the same state hash.
    assert_eq!(field(&verify, "state hash"), field(&replay, "state hash"));

    let tail = ok(&[&["tail"], &at[..]].concat());
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("genesis") && lines[0].contains(&started.root));
    assert!(lines[1].contains("bootstrap") && lines[1].contains("may write verdict on gate"));
    assert!(lines[2].contains("policy") && lines[2].contains("VerseTextMax(4096)"));

    let last = ok(&[&["tail", "-n", "1"], &at[..]].concat());
    assert_eq!(last.lines().count(), 1);
    assert!(last.contains("policy"));
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
    let last = bytes.len() - 100; // inside entry 2
    bytes[last] ^= 1;
    std::fs::write(&started.log, &bytes).expect("write");

    for command in ["verify", "replay", "tail"] {
        let error = fails(&[command, "--log", &started.log, "--root", &started.root]);
        assert!(error.contains("entry 2"), "{command}: {error}");
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
    assert_eq!(out.trim(), "entry 3  grant 3.0");

    // Narrowed in place to one stream and one kind.
    let out = ok(&joined(
        "grant",
        &to_holder,
        &["--scope", "island", "--rights", "verse", "--narrows", "3.0"],
    ));
    assert_eq!(out.trim(), "entry 4  grant 4.0");

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
            "4.0",
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
    assert!(ok(&[&["verify"], &read[..]].concat()).contains("ok: 5 entries"));

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
        ok(&joined("revoke", &base, &["--grant", "4.0"])).trim(),
        "entry 5  revoked grant 4.0"
    );
    assert!(
        fails(&joined("revoke", &base, &["--grant", "4.0"]))
            .contains("grant 4.0 is no longer in force")
    );

    let listing = ok(&[&["grants"], &read[..]].concat());
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), 4, "{listing}");
    assert!(lines[0].starts_with("1.0 ") && lines[0].ends_with("(in force)  [root capability]"));
    assert!(lines[0].contains("policy grant revoke on authority policy"));
    assert!(lines[1].starts_with("1.1 ") && lines[1].ends_with("(in force)  [appender]"));
    assert!(lines[2].starts_with("3.0 ") && lines[2].contains(HOLDER));
    assert!(lines[2].contains("verse preference on flare island"));
    assert!(lines[2].ends_with("(replaced by 4.0 at entry 4)"));
    assert!(lines[3].contains("verse on island") && lines[3].ends_with("(revoked at entry 5)"));

    assert!(ok(&[&["verify"], &read[..]].concat()).contains("ok: 6 entries"));
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
    assert_eq!(out.trim(), "entry 3  grant 3.0");
    assert!(
        ok(&["verify", "--log", &started.log, "--root", &started.root]).contains("ok: 4 entries")
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

    // First run: three hashes, and no log file yet.
    let (code, instructions, _) = outcome(&command);
    assert_eq!(code, 3);
    let hashes = hashes(&instructions);
    assert_eq!(hashes.len(), 3);
    assert!(
        !Path::new(&log).exists(),
        "the log was created before it was signed"
    );

    // One signature is not enough.
    let signed: Vec<String> = hashes
        .iter()
        .map(|hash| hex(&sign_by_hand(&root_key, hash)))
        .collect();
    let (code, _, error) = outcome(&[&command[..], &["--signature", &signed[0]]].concat());
    assert_eq!(code, 1);
    assert!(error.contains("1 signatures given, 3 needed"), "{error}");
    assert!(!Path::new(&log).exists());

    let mut all = command.to_vec();
    for signature in &signed {
        all.extend(["--signature", signature]);
    }
    let out = ok(&all);
    assert!(out.contains("entry 2   policy"));
    assert!(ok(&["verify", "--log", &log, "--root", &root]).contains("ok: 3 entries"));
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

// ── The gate ────────────────────────────────────────────────────────

/// A started log, and a writer key granted verses on `island` and
/// preferences on `island` and `plan` (grant 3.0).
struct Writing {
    started: Started,
    appender_key: String,
    root_key: String,
    writer_key: String,
}

fn writing() -> Writing {
    let started = started();
    let (root_key, appender_key, writer_key) = (
        sibling(&started, "root.key"),
        sibling(&started, "appender.key"),
        sibling(&started, "writer.key"),
    );
    let public = field(&ok(&["keygen", &writer_key]), "public");
    assert_eq!(ok(&["public", &writer_key]).trim(), public);
    let out = ok(&[
        "grant",
        "--log",
        &started.log,
        "--appender-key",
        &appender_key,
        "--root-key",
        &root_key,
        "--holder",
        &public,
        "--scope",
        "island,plan",
        "--rights",
        "verse,preference",
    ]);
    assert_eq!(out.trim(), "entry 3  grant 3.0");
    Writing {
        started,
        appender_key,
        root_key,
        writer_key,
    }
}

impl Writing {
    /// The arguments an ordinary writer's commands share.
    fn as_writer(&self) -> [&str; 8] {
        [
            "--log",
            &self.started.log,
            "--appender-key",
            &self.appender_key,
            "--root",
            &self.started.root,
            "--key",
            &self.writer_key,
        ]
    }

    /// The arguments root's commands share.
    fn as_root(&self) -> [&str; 6] {
        [
            "--log",
            &self.started.log,
            "--appender-key",
            &self.appender_key,
            "--root-key",
            &self.root_key,
        ]
    }

    fn read(&self, command: &str) -> String {
        ok(&[
            command,
            "--log",
            &self.started.log,
            "--root",
            &self.started.root,
        ])
    }

    fn verse(&self, evidence: &str, text: &str, more: &[&str]) -> (i32, String, String) {
        let verse = ["--stream", "island", "--evidence", evidence, "--text", text];
        outcome(&joined(
            "verse",
            &self.as_writer(),
            &[&verse[..], more].concat(),
        ))
    }
}

/// The hash a refusal names, for an override request.
fn refused_hash(error: &str) -> String {
    let (_, hash) = error
        .split_once("the refused submission is ")
        .unwrap_or_else(|| panic!("no refused submission named in: {error}"));
    hash.trim().to_owned()
}

#[test]
fn a_verse_is_judged_and_a_refusal_leaves_a_verdict() {
    let writing = writing();

    let (code, out, _) = writing.verse("decision", "the gate judges every verse", &[]);
    assert_eq!(code, 0);
    assert!(out.starts_with("entry 4  "), "{out}");

    // A verse that names no kind of evidence: refused, with a verdict.
    let (code, out, error) = writing.verse("none", "trust me", &[]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        error.contains("refused by the gate: it breaks VerseNamesEvidence"),
        "{error}"
    );
    assert!(error.contains("verdict at entry 5"), "{error}");
    let refused = refused_hash(&error);
    assert_eq!(refused.len(), 64);

    // A verse longer than the bound.
    let (code, _, error) = writing.verse("measurement", &"x".repeat(4097), &[]);
    assert_eq!(code, 1);
    assert!(error.contains("it breaks VerseTextMax(4096)"), "{error}");
    assert!(error.contains("verdict at entry 6"), "{error}");

    // Outside the grant: refused with no entry at all.
    let other = joined(
        "verse",
        &writing.as_writer(),
        &["--stream", "flare", "--evidence", "decision", "--text", "x"],
    );
    let (code, _, error) = outcome(&other);
    assert_eq!(code, 1);
    assert!(error.contains("not authorized"), "{error}");

    // The log verifies, verdicts judged again; the writer's counter moved
    // for the refusals too.
    assert!(writing.read("verify").contains("ok: 7 entries"));
    let replay = writing.read("replay");
    assert!(
        replay.contains("gate          2 entries, newest 6"),
        "{replay}"
    );
    assert!(
        replay.contains("counter 3, refused, verdict at entry 6"),
        "{replay}"
    );
    assert!(replay.contains("verdicts           2\noverride requests  0\n"));
    let tail = writing.read("tail");
    let verdict = tail.lines().nth(5).expect("entry 5");
    assert!(
        verdict.contains("verdict") && verdict.contains("gate"),
        "{verdict}"
    );
    assert!(
        verdict.ends_with(&format!(
            "refused verse from {}, submission {}: it breaks VerseNamesEvidence",
            &ok(&["public", &writing.writer_key])[..8],
            &refused[..8]
        )),
        "{verdict}"
    );
}

#[test]
fn the_policy_tightens_and_a_loosening_is_refused_and_recorded() {
    let writing = writing();
    let policy = |more: &[&str]| outcome(&joined("policy", &writing.as_root(), more));

    // An artefact with no ref is fine under the first policy.
    assert_eq!(writing.verse("artefact", "built it", &[]).0, 0);

    // The tightening: a lower bound, and artefacts must carry a ref.
    let (code, out, _) = policy(&["--verse-text-max", "200", "--artefact-has-ref"]);
    assert_eq!(code, 0);
    assert!(
        out.starts_with(
            "entry 5  policy in force: VerseNamesEvidence, VerseTextMax(200), ArtefactHasRef"
        ),
        "{out}"
    );
    assert!(out.contains("evaluator  "), "{out}");

    // New verses are judged by it; the old one stays valid.
    let (code, _, error) = writing.verse("artefact", "built another", &[]);
    assert_eq!(code, 1);
    assert!(error.contains("it breaks ArtefactHasRef"), "{error}");
    let with_ref = ["--ref", &"ab".repeat(32)[..]];
    assert_eq!(writing.verse("artefact", "built another", &with_ref).0, 0);
    assert_eq!(writing.verse("decision", &"x".repeat(201), &[]).0, 1);

    // A loosening: refused by the gate, with a verdict, and the policy in
    // force stays.
    let (code, out, error) = policy(&["--verse-text-max", "4096"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        error.contains("refused by the gate: the policy would be weaker than the one in force"),
        "{error}"
    );
    assert!(error.contains("verdict at entry 9"), "{error}");
    let replay = writing.read("replay");
    assert!(
        replay.contains("policy      entry 5, evaluator"),
        "{replay}"
    );
    assert!(
        replay.contains("VerseTextMax(200), ArtefactHasRef"),
        "{replay}"
    );
    assert!(replay.contains("verdicts           3\n"), "{replay}");

    // Stating the policy again changes nothing but the entry it is in.
    let (code, out, _) = policy(&[]);
    assert_eq!(code, 0);
    assert!(out.starts_with("entry 10  policy in force: VerseNamesEvidence, VerseTextMax(200)"));
    assert!(writing.read("verify").contains("ok: 11 entries"));
}

#[test]
fn an_override_request_is_recorded_and_counted_and_lets_nothing_through() {
    let writing = writing();
    let (_, _, error) = writing.verse("none", "trust me", &[]);
    let refused = refused_hash(&error);
    let ask =
        |name: &str, why: &str| outcome(&joined("override", &writing.as_writer(), &[name, why]));

    // It must name a submission a verdict holds.
    let (code, _, error) = ask(&"0".repeat(64), "because");
    assert_eq!(code, 1);
    assert!(error.contains("no verdict in the log holds"), "{error}");
    let (code, _, error) = ask("abc", "because");
    assert_eq!(code, 1);
    assert!(error.contains("at least eight hex digits"), "{error}");

    // By its whole hash, or by the digits `tail` shows.
    let (code, out, _) = ask(&refused, "the evidence is in the text");
    assert_eq!(code, 0);
    assert!(
        out.starts_with(&format!(
            "entry 5  override requested for submission {refused}"
        )),
        "{out}"
    );
    let (code, out, _) = ask(&refused[..8], "asking again");
    assert_eq!(code, 0);
    assert!(out.contains(&refused), "{out}");

    let replay = writing.read("replay");
    assert!(
        replay.contains("verdicts           1\noverride requests  2\n"),
        "{replay}"
    );
    assert!(
        replay.contains("plan          2 entries, newest 6"),
        "{replay}"
    );
    let tail = writing.read("tail");
    let request = tail.lines().nth(5).expect("entry 5");
    assert!(
        request.contains(&format!(
            "\"override-request\" = \"{refused} the evidence is in the text\""
        )),
        "{request}"
    );
    // Nothing was let through: the refused verse is in no entry of its own.
    assert_eq!(tail.matches("trust me").count(), 0);
    assert!(writing.read("verify").contains("ok: 7 entries"));
}

#[test]
fn a_log_started_before_the_gate_gets_its_first_policy_from_the_policy_command() {
    use island_core::{SigningKey, public_key};
    use island_env::{FileStorage, SystemClock};
    use island_log::{Appender, genesis};

    // A log as `init` wrote it before build step 6: entries 0 and 1 only.
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
    let (root, appender) = (
        SigningKey::from_bytes(&[0x52; 32]),
        SigningKey::from_bytes(&[0x41; 32]),
    );
    std::fs::write(&root_key, hex(&[0x52; 32])).expect("write");
    std::fs::write(&appender_key, hex(&[0x41; 32])).expect("write");
    let pin = public_key(&root);
    {
        let storage = FileStorage::create(Path::new(&log)).expect("create");
        let (mut open, _) =
            Appender::open(storage, SystemClock, appender.clone(), pin).expect("open");
        for submission in [
            genesis::genesis(pin),
            genesis::bootstrap(pin, public_key(&appender)),
        ] {
            let signature = submission.sign(&root).expect("root signs");
            open.submit(submission, signature).expect("accepted");
        }
    }
    let read = ["--log", &log, "--root", &hex(&pin)];
    assert!(ok(&[&["replay"], &read[..]].concat()).contains("policy      none yet"));

    let out = ok(&[
        "policy",
        "--log",
        &log,
        "--appender-key",
        &appender_key,
        "--root-key",
        &root_key,
        "--verse-names-evidence",
        "--verse-text-max",
        "4096",
    ]);
    assert!(
        out.starts_with("entry 2  policy in force: VerseNamesEvidence, VerseTextMax(4096)"),
        "{out}"
    );
    assert!(ok(&[&["verify"], &read[..]].concat()).contains("ok: 3 entries"));
}
