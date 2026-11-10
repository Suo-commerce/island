//! Golden vectors: one verse submission and its entry, as bytes and hashes
//! (docs/spec.md, "Build order", step 2).
//!
//! `golden/verse.txt` is the contract. These tests rebuild the same values
//! from the inputs below and require every byte to match, so any change to
//! the encoding, a domain string or the signing turns CI red. An
//! implementation in another language can check itself against the same
//! file.
//!
//! The file is never regenerated to make a test pass. A deliberate format
//! change means new domain strings and a new file; `print_golden` prints
//! what the current code produces.

use island_core::{
    Body, Entry, Evidence, Record, SigningKey, StreamId, Submission, Verse, public_key,
};
use taso_core::canon::Canonical;

const GOLDEN: &str = include_str!("golden/verse.txt");

/// Test keys: 32 bytes of `W` for the writer, of `A` for the appender.
const WRITER_SECRET: [u8; 32] = [0x57; 32];
const APPENDER_SECRET: [u8; 32] = [0x41; 32];

const TEXT: &str = "island-core encodes its first verse";

fn submission() -> Submission {
    Submission {
        stream: StreamId::from("island"),
        writer: public_key(&SigningKey::from_bytes(&WRITER_SECRET)),
        counter: 1,
        body: Body::Verse(Verse::new(Evidence::Artefact, TEXT, vec![[0xAB; 32]])),
    }
}

fn record() -> Record {
    let submission = submission();
    let sig_writer = submission
        .sign(&SigningKey::from_bytes(&WRITER_SECRET))
        .expect("the writer key is the submission's writer");
    let entry = Entry {
        seq: 3,
        at: 1_794_312_000_000_000, // 2026-11-10T12:00:00Z in microseconds
        policy: 2,
        prev: [0x22; 32],
        submission,
        sig_writer,
    };
    Record::seal(entry, &SigningKey::from_bytes(&APPENDER_SECRET))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd hex length");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex digit"))
        .collect()
}

/// The value of `key = value` in the golden file.
fn golden(key: &str) -> Vec<u8> {
    let value = GOLDEN
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(" = "))
        .find_map(|(name, value)| (name == key).then_some(value))
        .unwrap_or_else(|| panic!("golden/verse.txt has no `{key}`"));
    unhex(value)
}

/// What the current code produces, in the file's format.
fn render() -> String {
    let record = record();
    let lines = [
        ("writer_secret", WRITER_SECRET.to_vec()),
        ("appender_secret", APPENDER_SECRET.to_vec()),
        ("writer", record.entry.submission.writer.to_vec()),
        (
            "appender",
            public_key(&SigningKey::from_bytes(&APPENDER_SECRET)).to_vec(),
        ),
        ("submission", record.entry.submission.canon_bytes()),
        ("sub_hash", record.entry.submission.hash().to_vec()),
        ("sig_writer", record.entry.sig_writer.to_vec()),
        ("entry", record.entry.canon_bytes()),
        ("hash", record.hash.to_vec()),
        ("sig_appender", record.sig_appender.to_vec()),
        ("record", record.encode()),
    ];
    lines
        .iter()
        .map(|(key, value)| format!("{key} = {}\n", hex(value)))
        .collect()
}

#[test]
fn submission_bytes_follow_the_written_layout() {
    // Laid out by hand from docs/spec.md, without the encoder.
    let mut expected = Vec::new();
    expected.extend(6u64.to_le_bytes()); // stream: length, then bytes
    expected.extend(b"island");
    expected.extend(golden("writer")); // writer: 32 bytes, no length
    expected.extend(1u64.to_le_bytes()); // counter
    expected.push(3); // kind: verse
    expected.push(1); // evidence: artefact
    expected.extend((TEXT.len() as u64).to_le_bytes()); // text: length, then bytes
    expected.extend(TEXT.as_bytes());
    expected.extend(1u64.to_le_bytes()); // refs: count, then 32 bytes each
    expected.extend([0xAB; 32]);

    assert_eq!(hex(&submission().canon_bytes()), hex(&expected));
    assert_eq!(hex(&golden("submission")), hex(&expected));
}

#[test]
fn entry_bytes_follow_the_written_layout() {
    let mut expected = Vec::new();
    expected.extend(3u64.to_le_bytes()); // seq
    expected.extend(1_794_312_000_000_000u64.to_le_bytes()); // at
    expected.extend(2u64.to_le_bytes()); // policy
    expected.extend([0x22; 32]); // prev
    expected.extend(golden("submission"));
    expected.extend(golden("sig_writer"));

    assert_eq!(hex(&record().entry.canon_bytes()), hex(&expected));
    assert_eq!(hex(&golden("entry")), hex(&expected));

    // record = canon(Entry) || hash || sig_appender
    expected.extend(golden("hash"));
    expected.extend(golden("sig_appender"));
    assert_eq!(hex(&golden("record")), hex(&expected));
}

#[test]
fn the_code_reproduces_the_golden_file() {
    let produced = render();
    for line in produced.lines() {
        let (key, value) = line.split_once(" = ").expect("key = value");
        assert_eq!(
            value,
            hex(&golden(key)),
            "`{key}` differs from golden/verse.txt"
        );
    }
    let keys = GOLDEN
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .count();
    assert_eq!(
        keys,
        produced.lines().count(),
        "golden/verse.txt has other lines"
    );
}

#[test]
fn the_golden_record_decodes_and_verifies() {
    let decoded = Record::decode(&golden("record")).expect("the golden record decodes");
    assert_eq!(decoded, record());
    assert_eq!(decoded.encode(), golden("record"));

    let appender: [u8; 32] = golden("appender").try_into().expect("32 bytes");
    decoded
        .verify(&appender)
        .expect("the golden record verifies");

    let submission = Submission::decode(&golden("submission")).expect("it decodes");
    assert_eq!(submission, self::submission());
}

/// `cargo test -p island-core --test golden -- --ignored --nocapture`
#[test]
#[ignore = "prints the vectors the current code produces; not a check"]
fn print_golden() {
    print!("{}", render());
}
