//! Test support for `tests/crash.rs`: submit verses to a real log file as
//! fast as possible, printing one line per answer, until killed. Every
//! third verse names no evidence, which the log's policy refuses.
//!
//! `island-log-crash-writer <log file> <root public key, hex>`
//!
//! Prints `open <records> <dropped bytes>` once, then `ack <seq> <hash>`
//! after each acknowledged entry and `refused <seq> <hash>` after each
//! refusal, naming the verdict entry. A line is printed only after
//! `submit` returned, so every printed line names a durable entry.

use std::io::Write;
use std::process::ExitCode;

use island_core::{Body, Evidence, PublicKey, SigningKey, StreamId, Submission, Verse, public_key};
use island_env::{FileStorage, SystemClock};
use island_log::{Appender, Rejected};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<PublicKey> {
    if text.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(key)
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(pin)) = (args.next(), args.next()) else {
        return Err("usage: island-log-crash-writer <log file> <root public key, hex>".into());
    };
    let pin = unhex(&pin).ok_or("the root public key is 64 hex digits")?;

    // The same test keys as tests/common.
    let appender_key = SigningKey::from_bytes(&[0x41; 32]);
    let writer = SigningKey::from_bytes(&[0x57; 32]);

    let storage = FileStorage::open(&path).map_err(|error| format!("open {path}: {error}"))?;
    let (mut appender, recovery) = Appender::open(storage, SystemClock, appender_key, pin)
        .map_err(|error| format!("open {path}: {error}"))?;

    let mut out = std::io::stdout().lock();
    let mut say = |line: String| -> Result<(), String> {
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(|error| error.to_string())
    };
    say(format!("open {} {}", recovery.records, recovery.dropped))?;

    loop {
        let counter = appender
            .chain()
            .next_counter(&public_key(&writer))
            .ok_or("the writer has no counter left")?;
        let submission = Submission {
            stream: StreamId::from("crash"),
            writer: public_key(&writer),
            counter,
            body: Body::Verse(Verse {
                evidence: if counter.is_multiple_of(3) {
                    u8::MAX
                } else {
                    Evidence::Measurement.as_u8()
                },
                text: format!("verse {counter} of a writer that will be killed"),
                refs: vec![[counter as u8; 32]],
            }),
        };
        let signature = submission.sign(&writer).ok_or("the writer signs")?;
        match appender.submit(submission, signature) {
            Ok(ack) => say(format!("ack {} {}", ack.seq, hex(&ack.hash)))?,
            Err(Rejected::Refused { seq, hash, .. }) => {
                say(format!("refused {seq} {}", hex(&hash)))?;
            }
            Err(error) => return Err(format!("submit {counter}: {error}")),
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("island-log-crash-writer: {error}");
            ExitCode::FAILURE
        }
    }
}
