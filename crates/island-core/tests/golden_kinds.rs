//! Golden vectors for the kinds slice 2 adds: `receipt`, `access`,
//! `approval` and `dispatch`, and the two hashes that name a request and
//! an action key.
//!
//! `golden/kinds.txt` is the contract, as `golden/verse.txt` is for the
//! first verse. The file is never regenerated to make a test pass;
//! `print_golden_kinds` prints what the current code produces.

use island_core::{
    Access, AccessKind, Answered, Approval, Body, Dispatch, Receipt, StreamId, Submission,
    effect_key, request_hash,
};
use taso_core::canon::Canonical;

const GOLDEN: &str = include_str!("golden/kinds.txt");

/// The writer in every vector: 32 bytes of 0x57 (a key, not a secret).
const WRITER: [u8; 32] = [0x57; 32];

const PROVIDER: &str = "github";
const REQUEST: &[u8] = b"{\"op\":\"open-pr\",\"repo\":\"island\"}";
const RAW_KEY: &[u8] = b"pr:island:step-1";

fn submission(stream: &str, counter: u64, body: Body) -> Submission {
    Submission {
        stream: StreamId::from(stream),
        writer: WRITER,
        counter,
        body,
    }
}

fn receipt() -> Submission {
    submission(
        "repos",
        1,
        Body::Receipt(Receipt {
            repo: "island".into(),
            branch: "main".into(),
            head: [0x11; 20],
            previous: Some([0x10; 20]),
            commits: 3,
            observed_at: 1_794_312_000_000_000,
        }),
    )
}

fn access() -> Submission {
    submission(
        "access",
        2,
        Body::Access(Access {
            vessel: [0x0C; 16],
            what: AccessKind::Inference,
            provider: "local".into(),
            calls: 4,
            bytes_in: 1000,
            bytes_out: 2000,
            digest: [0xD1; 32],
        }),
    )
}

fn approval() -> Submission {
    submission(
        "approvals",
        3,
        Body::Approval(Approval {
            request: request_hash(PROVIDER, REQUEST),
            verdict: Answered::Approve,
            proof: None,
        }),
    )
}

fn dispatch() -> Submission {
    submission(
        "approvals",
        4,
        Body::Dispatch(Dispatch {
            vessel: [0x0C; 16],
            op: 7,
            key: effect_key(RAW_KEY),
            request: request_hash(PROVIDER, REQUEST),
            approval: 5,
            counter: 0,
        }),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn golden(key: &str) -> String {
    GOLDEN
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(" = "))
        .find_map(|(name, value)| (name == key).then(|| value.to_owned()))
        .unwrap_or_else(|| panic!("golden/kinds.txt has no `{key}`"))
}

fn render() -> String {
    let lines = [
        ("request_hash", request_hash(PROVIDER, REQUEST).to_vec()),
        ("effect_key", effect_key(RAW_KEY).to_vec()),
        ("receipt", receipt().canon_bytes()),
        ("receipt_hash", receipt().hash().to_vec()),
        ("access", access().canon_bytes()),
        ("access_hash", access().hash().to_vec()),
        ("approval", approval().canon_bytes()),
        ("approval_hash", approval().hash().to_vec()),
        ("dispatch", dispatch().canon_bytes()),
        ("dispatch_hash", dispatch().hash().to_vec()),
    ];
    lines
        .iter()
        .map(|(key, value)| format!("{key} = {}\n", hex(value)))
        .collect()
}

/// The common head of a submission, by hand: stream, writer, counter, kind.
fn head(stream: &str, counter: u64, kind: u8) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((stream.len() as u64).to_le_bytes());
    out.extend(stream.as_bytes());
    out.extend(WRITER);
    out.extend(counter.to_le_bytes());
    out.push(kind);
    out
}

#[test]
fn the_request_hash_follows_the_written_layout() {
    // derive_key("island.effect-request.v1", u64le(len) || provider || request)
    let mut input = Vec::new();
    input.extend((PROVIDER.len() as u64).to_le_bytes());
    input.extend(PROVIDER.as_bytes());
    input.extend(REQUEST);
    let expected = blake3::derive_key("island.effect-request.v1", &input);
    assert_eq!(request_hash(PROVIDER, REQUEST), expected);
    assert_eq!(
        effect_key(RAW_KEY),
        blake3::derive_key("island.effect-key.v1", RAW_KEY)
    );
}

#[test]
fn the_new_bodies_follow_the_written_layout() {
    let mut expected = head("repos", 1, 6);
    expected.extend(6u64.to_le_bytes());
    expected.extend(b"island");
    expected.extend(4u64.to_le_bytes());
    expected.extend(b"main");
    expected.extend([0x11; 20]);
    expected.push(1); // previous: present
    expected.extend([0x10; 20]);
    expected.extend(3u32.to_le_bytes());
    expected.extend(1_794_312_000_000_000u64.to_le_bytes());
    assert_eq!(hex(&receipt().canon_bytes()), hex(&expected));

    let mut expected = head("access", 2, 7);
    expected.extend([0x0C; 16]);
    expected.push(0); // inference
    expected.extend(5u64.to_le_bytes());
    expected.extend(b"local");
    expected.extend(4u32.to_le_bytes());
    expected.extend(1000u64.to_le_bytes());
    expected.extend(2000u64.to_le_bytes());
    expected.extend([0xD1; 32]);
    assert_eq!(hex(&access().canon_bytes()), hex(&expected));

    let mut expected = head("approvals", 3, 10);
    expected.extend(request_hash(PROVIDER, REQUEST));
    expected.push(0); // approve
    expected.push(0); // proof: absent
    assert_eq!(hex(&approval().canon_bytes()), hex(&expected));

    let mut expected = head("approvals", 4, 11);
    expected.extend([0x0C; 16]);
    expected.extend(7u64.to_le_bytes());
    expected.extend(effect_key(RAW_KEY));
    expected.extend(request_hash(PROVIDER, REQUEST));
    expected.extend(5u64.to_le_bytes());
    expected.extend(0u64.to_le_bytes());
    assert_eq!(hex(&dispatch().canon_bytes()), hex(&expected));
}

#[test]
fn the_code_reproduces_the_golden_file() {
    let produced = render();
    for line in produced.lines() {
        let (key, value) = line.split_once(" = ").expect("key = value");
        assert_eq!(value, golden(key), "`{key}` differs from golden/kinds.txt");
    }
    let keys = GOLDEN
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .count();
    assert_eq!(
        keys,
        produced.lines().count(),
        "golden/kinds.txt has other lines"
    );
}

#[test]
fn the_golden_submissions_decode() {
    for (name, built) in [
        ("receipt", receipt()),
        ("access", access()),
        ("approval", approval()),
        ("dispatch", dispatch()),
    ] {
        let bytes: Vec<u8> = (0..golden(name).len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&golden(name)[i..i + 2], 16).expect("hex"))
            .collect();
        assert_eq!(Submission::decode(&bytes), Ok(built), "{name}");
    }
}

#[test]
fn a_proof_is_refused_until_slice_3_gives_it_a_form() {
    let mut bytes = approval().canon_bytes();
    *bytes.last_mut().expect("not empty") = 1;
    assert!(Submission::decode(&bytes).is_err());
}

/// `cargo test -p island-core --test golden_kinds -- --ignored --nocapture`
#[test]
#[ignore = "prints the vectors the current code produces; not a check"]
fn print_golden_kinds() {
    print!("{}", render());
}
