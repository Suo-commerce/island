//! Golden fixtures for the broker's wire format, version 1
//! (docs/spec.md, "Interfaces": "versioned and tested by golden
//! fixtures").
//!
//! `tests/golden/wire-v1.json` holds one example of everything that
//! crosses the wire. This test builds each example from Island's own
//! types and requires the JSON to be exactly the file's. A change to the
//! file is a change to the contract between the broker and every client:
//! it needs a new wire version, not a new fixture.
//!
//! To write the file anew after such a decision:
//! `ISLAND_BLESS=1 cargo test -p island-client --test golden`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use island_client::wire::{
    AppendRequest, Capabilities, CapabilitiesRequest, Code, GrantInfo, Head, LogQuery, Position,
    ReadStreamRequest, Records, Refusal, Refused, WIRE_VERSION,
};
use island_client::{Signed, Writer, codec};
use island_core::{
    Ended, Entry, Evidence, Grant, GrantId, Held, Kind, Record, Role, SigningKey, StreamId, Verse,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn writer() -> Writer {
    Writer::new(SigningKey::from_bytes(&[0x11; 32]))
}

/// The verse every fixture is about.
fn signed() -> Signed {
    let verse = Verse::new(
        Evidence::Artefact,
        "the wire format is pinned".to_owned(),
        vec![[0xab; 32]],
    );
    writer().verse("island", 3, verse)
}

/// That verse as entry 7 of a log, sealed by a fixed appender key.
fn record() -> Record {
    let Signed {
        submission,
        sig_writer,
    } = signed();
    Record::seal(
        Entry {
            seq: 7,
            at: 1_793_000_000_000_000,
            policy: 2,
            prev: [0x70; 32],
            submission,
            sig_writer,
        },
        &SigningKey::from_bytes(&[0x41; 32]),
    )
}

fn grants() -> Vec<GrantInfo> {
    let grant = |scope: &[&str], rights: &[Kind], narrows| Grant {
        holder: writer().public(),
        scope: scope.iter().map(|name| StreamId::from(*name)).collect(),
        rights: rights.iter().copied().collect(),
        narrows,
    };
    let held = [
        // Replaced by a narrowing, which was later revoked; and one in
        // force.
        (
            GrantId::new(3, 0),
            grant(&["island", "plan"], &[Kind::Verse, Kind::Preference], None),
            Some(Ended {
                at: 9,
                by: Some(GrantId::new(9, 0)),
            }),
        ),
        (
            GrantId::new(9, 0),
            grant(&["island"], &[Kind::Verse], Some(GrantId::new(3, 0))),
            Some(Ended { at: 12, by: None }),
        ),
        (
            GrantId::new(14, 0),
            grant(&["flare"], &[Kind::Verse], None),
            None,
        ),
    ];
    held.into_iter()
        .map(|(id, grant, ended)| {
            codec::grant_info(
                id,
                &Held {
                    grant,
                    role: Role::Ordinary,
                    ended,
                },
            )
        })
        .collect()
}

/// One example of everything that crosses the wire, by name.
fn examples() -> BTreeMap<&'static str, Value> {
    let json = |value: &dyn erased::Json| value.json();
    let record = record();
    let signed = signed();
    let gate = Refusal {
        code: Code::Gate,
        reason: "refused by the gate: it breaks VerseNamesEvidence".to_owned(),
        verdict: Some(codec::position(8, &[0x88; 32])),
        next_counter: None,
    };
    let gap = Refusal {
        code: Code::CounterGap,
        reason: "counter 9 skips ahead; the next is 4".to_owned(),
        verdict: None,
        next_counter: Some(4),
    };
    BTreeMap::from([
        (
            "append: input",
            json(&codec::append_request(
                &signed.submission,
                &signed.sig_writer,
            )),
        ),
        ("append: accepted", json(&codec::position(7, &record.hash))),
        (
            "append: refused by the gate",
            json(&Refused { refused: gate }),
        ),
        (
            "append: refused for its counter",
            json(&Refused { refused: gap }),
        ),
        (
            "read_stream: input",
            json(&ReadStreamRequest {
                stream: "island".to_owned(),
                after: Some(6),
                limit: Some(50),
            }),
        ),
        (
            "read_stream: input, from the start",
            json(&ReadStreamRequest {
                stream: "island".to_owned(),
                after: None,
                limit: None,
            }),
        ),
        (
            "read_stream and /log: output",
            json(&codec::records([&record], true)),
        ),
        (
            "my_capabilities: input",
            json(&CapabilitiesRequest {
                writer: codec::b64(&writer().public()),
            }),
        ),
        (
            "my_capabilities: output",
            json(&Capabilities {
                grants: grants(),
                next_counter: 4,
            }),
        ),
        (
            "/log: query",
            json(&LogQuery {
                after: Some(6),
                limit: Some(500),
            }),
        ),
        (
            "/head: output",
            json(&Head {
                seq: 7,
                hash: codec::b64(&record.hash),
                build: "f9634932b6a9079d9d5c2f5acc4f65f0f8a14d03".to_owned(),
                wire: WIRE_VERSION,
            }),
        ),
    ])
}

/// `serde_json::to_value` behind a trait object, so the examples can be
/// listed in one map.
mod erased {
    pub trait Json {
        fn json(&self) -> serde_json::Value;
    }

    impl<T: serde::Serialize> Json for T {
        fn json(&self) -> serde_json::Value {
            serde_json::to_value(self).expect("the wire types serialize")
        }
    }
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/wire-v1.json")
}

#[test]
fn the_wire_format_is_the_fixture() {
    let built = serde_json::to_string_pretty(&examples()).expect("JSON") + "\n";
    if std::env::var_os("ISLAND_BLESS").is_some() {
        std::fs::write(fixture(), &built).expect("write the fixture");
    }
    let pinned = std::fs::read_to_string(fixture()).expect("the fixture is committed");
    assert_eq!(
        built, pinned,
        "the wire format no longer matches tests/golden/wire-v1.json"
    );
}

/// Read one pinned example as the type it is an example of, and require
/// it to serialize back to the same JSON.
fn pinned<T: DeserializeOwned + Serialize>(name: &str) -> T {
    let text = std::fs::read_to_string(fixture()).expect("the fixture is committed");
    let all: BTreeMap<String, Value> = serde_json::from_str(&text).expect("JSON");
    let example = all
        .get(name)
        .unwrap_or_else(|| panic!("no example {name:?}"));
    let value: T =
        serde_json::from_value(example.clone()).unwrap_or_else(|error| panic!("{name}: {error}"));
    assert_eq!(
        &serde_json::to_value(&value).expect("JSON"),
        example,
        "{name}"
    );
    value
}

#[test]
fn every_pinned_example_reads_back_as_its_type() {
    // The request holds the very submission and signature that were
    // signed, and the record is the very record.
    let request: AppendRequest = pinned("append: input");
    let (submission, sig_writer) = codec::parse_append(&request).expect("canonical");
    assert_eq!(
        Signed {
            submission,
            sig_writer
        },
        signed()
    );
    let page: Records = pinned("read_stream and /log: output");
    assert_eq!(codec::parse_records(&page).expect("canonical"), [record()]);
    assert!(page.more);
    let accepted: Position = pinned("append: accepted");
    assert_eq!(codec::parse_position(&accepted), Ok((7, record().hash)));

    let gate: Refused = pinned("append: refused by the gate");
    assert_eq!(gate.refused.code, Code::Gate);
    assert_eq!(gate.refused.verdict.map(|verdict| verdict.seq), Some(8));
    let gap: Refused = pinned("append: refused for its counter");
    assert_eq!(
        (gap.refused.code, gap.refused.next_counter),
        (Code::CounterGap, Some(4))
    );

    let read: ReadStreamRequest = pinned("read_stream: input");
    assert_eq!((read.after, read.limit), (Some(6), Some(50)));
    let from_start: ReadStreamRequest = pinned("read_stream: input, from the start");
    assert_eq!((from_start.after, from_start.limit), (None, None));
    let _: LogQuery = pinned("/log: query");
    let who: CapabilitiesRequest = pinned("my_capabilities: input");
    assert_eq!(codec::parse_writer(&who.writer), Ok(writer().public()));

    let mine: Capabilities = pinned("my_capabilities: output");
    assert_eq!(mine.next_counter, 4);
    let ids: Vec<&str> = mine.grants.iter().map(|grant| grant.id.as_str()).collect();
    assert_eq!(ids, ["3.0", "9.0", "14.0"]);
    assert_eq!(mine.grants[0].replaced_by.as_deref(), Some("9.0"));
    assert_eq!(mine.grants[1].revoked_at, Some(12));
    assert_eq!(mine.grants[1].narrows.as_deref(), Some("3.0"));
    assert!(mine.grants[2].revoked_at.is_none() && mine.grants[2].replaced_by.is_none());

    let head: Head = pinned("/head: output");
    assert_eq!((head.seq, head.wire), (7, 1));
}

#[test]
fn every_code_has_its_name() {
    let names = [
        (Code::Malformed, "malformed"),
        (Code::BadSignature, "bad-signature"),
        (Code::CounterOld, "counter-old"),
        (Code::CounterGap, "counter-gap"),
        (Code::CounterConflict, "counter-conflict"),
        (Code::Gate, "gate"),
        (Code::Denied, "denied"),
        (Code::VerdictFromOutside, "verdict-from-outside"),
        (Code::Invalid, "invalid"),
        (Code::TooLarge, "too-large"),
        (Code::Unavailable, "unavailable"),
    ];
    for (code, name) in names {
        assert_eq!(serde_json::to_value(code).expect("JSON"), Value::from(name));
        assert_eq!(
            serde_json::from_value::<Code>(Value::from(name)).expect("known"),
            code
        );
    }
    assert!(serde_json::from_value::<Code>(Value::from("something-new")).is_err());
}

#[test]
fn a_field_the_format_does_not_have_is_refused() {
    // Strict in both directions: an answer with a field this client does
    // not know is from another wire version, and is not half understood.
    let with_extra = |name: &str| {
        let text = std::fs::read_to_string(fixture()).expect("the fixture is committed");
        let all: BTreeMap<String, Value> = serde_json::from_str(&text).expect("JSON");
        let mut example = all[name].clone();
        example
            .as_object_mut()
            .expect("an object")
            .insert("extra".to_owned(), Value::from(1));
        example
    };
    assert!(serde_json::from_value::<AppendRequest>(with_extra("append: input")).is_err());
    assert!(serde_json::from_value::<Position>(with_extra("append: accepted")).is_err());
    assert!(serde_json::from_value::<Head>(with_extra("/head: output")).is_err());
    assert!(serde_json::from_value::<Records>(with_extra("read_stream and /log: output")).is_err());
}

#[test]
fn names_of_kinds_go_both_ways() {
    for kind in [
        Kind::Policy,
        Kind::Grant,
        Kind::Revoke,
        Kind::Verse,
        Kind::Preference,
        Kind::Verdict,
        Kind::Genesis,
        Kind::Bootstrap,
        Kind::Receipt,
        Kind::Access,
        Kind::Approval,
        Kind::Dispatch,
    ] {
        let name = codec::kind_name(kind);
        assert_eq!(name, format!("{kind:?}").to_lowercase());
        assert_eq!(codec::kind_from_name(name), Some(kind));
    }
    assert_eq!(codec::kind_from_name("claim"), None);
}

#[test]
fn what_is_not_canonical_is_malformed() {
    let good = codec::append_request(&signed().submission, &signed().sig_writer);
    let mut longer = signed().submission.encode();
    longer.push(0);
    let cases = [
        ("submission", codec::b64(&longer), good.sig_writer.clone()),
        ("submission", "%%%".to_owned(), good.sig_writer.clone()),
        ("submission", String::new(), good.sig_writer.clone()),
        ("sig_writer", good.submission.clone(), codec::b64(&[0; 63])),
        ("sig_writer", good.submission.clone(), "%%%".to_owned()),
    ];
    for (field, submission, sig_writer) in cases {
        let error = codec::parse_append(&AppendRequest {
            submission,
            sig_writer,
        })
        .expect_err("malformed");
        assert!(error.to_string().starts_with(field), "{error}");
    }
    assert!(codec::parse_append(&good).is_ok());
}
