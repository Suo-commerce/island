//! Flare terminal: attaches to a shared session on `flare-server`; see
//! the log, and propose verses signed with this device's key
//! (docs/spec.md, "One session, three ways in" and build step 9).
//!
//! The terminal is line by line: commands are typed, and every event of
//! the session is printed as it happens, whoever caused it. This library
//! holds what the program does that can be tested on its own: reading a
//! command line, and printing an event.

#![warn(missing_docs)]

use flare_core::session::{Event, EventKind, Problem, VerbCall, VerbResult};
use flare_core::time;
use island_core::{Evidence, Hash, Verse};

/// A command typed at the prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Typed {
    /// `see` or `see STREAM [N]`.
    See {
        /// The stream, or the overview.
        stream: Option<String>,
        /// How many entries.
        entries: Option<usize>,
    },
    /// `verse EVIDENCE [ref=HASH]... TEXT`.
    Verse {
        /// The stream to write to.
        stream: String,
        /// The verse.
        verse: Verse,
    },
    /// `again`: propose the last proposal again, exactly.
    Again,
    /// `help`.
    Help,
    /// `quit`, or the end of input.
    Quit,
    /// Nothing typed.
    Nothing,
}

/// What `help` prints.
pub const HELP: &str = "\
commands:
  see                              the overview of the log
  see STREAM [N]                   the newest N entries of a stream (20)
  verse EVIDENCE [ref=HASH] TEXT   propose a verse on the default stream:
                                   EVIDENCE is decision, artefact,
                                   measurement or closed-question;
                                   HASH is 64 hex digits
  verse @STREAM EVIDENCE ... TEXT  the same, on another stream
  again                            propose the last proposal again, exactly
  help                             this
  quit                             leave; the session goes on
";

fn evidence(name: &str) -> Option<Evidence> {
    Some(match name {
        "decision" => Evidence::Decision,
        "artefact" => Evidence::Artefact,
        "measurement" => Evidence::Measurement,
        "closed-question" => Evidence::ClosedQuestion,
        _ => return None,
    })
}

fn unhex(text: &str) -> Option<Hash> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * index..2 * index + 2], 16).ok()?;
    }
    Some(out)
}

/// Read one typed line. `stream` is the stream a verse goes to unless the
/// line names another.
pub fn parse(line: &str, stream: &str) -> Result<Typed, String> {
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else {
        return Ok(Typed::Nothing);
    };
    match first {
        "see" => {
            let stream = words.next().map(str::to_owned);
            let entries = match words.next() {
                Some(count) => Some(
                    count
                        .parse()
                        .map_err(|_| format!("`{count}` is not a number"))?,
                ),
                None => None,
            };
            if words.next().is_some() {
                return Err("see takes a stream and a number at most".to_owned());
            }
            Ok(Typed::See { stream, entries })
        }
        "verse" => {
            let mut next = words
                .next()
                .ok_or("verse needs a kind of evidence and a text")?;
            let mut target = stream.to_owned();
            if let Some(named) = next.strip_prefix('@') {
                target = named.to_owned();
                next = words
                    .next()
                    .ok_or("verse needs a kind of evidence and a text")?;
            }
            let kind = evidence(next).ok_or_else(|| {
                format!("`{next}` is not a kind of evidence: decision, artefact, measurement, closed-question")
            })?;
            let mut refs = Vec::new();
            let mut text = Vec::new();
            for word in words {
                match word.strip_prefix("ref=") {
                    Some(hash) if text.is_empty() => {
                        refs.push(unhex(hash).ok_or("a ref is 64 hex digits")?);
                    }
                    _ => text.push(word),
                }
            }
            if text.is_empty() {
                return Err("verse needs a text".to_owned());
            }
            Ok(Typed::Verse {
                stream: target,
                verse: Verse::new(kind, text.join(" "), refs),
            })
        }
        "again" => Ok(Typed::Again),
        "help" | "?" => Ok(Typed::Help),
        "quit" | "exit" => Ok(Typed::Quit),
        other => Err(format!("`{other}` is not a command; `help` lists them")),
    }
}

/// The time of day an event happened, `HH:MM:SS`, UTC.
fn clock(at: u64) -> String {
    time(at).get(11..19).unwrap_or_default().to_owned()
}

/// An event as the terminal prints it.
pub fn show(event: &Event) -> String {
    let head = format!("#{} {}", event.seq, clock(event.at));
    let indent = |text: &str| {
        text.lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match event.kind {
        EventKind::VerbCall => match serde_json::from_value::<VerbCall>(event.payload.clone()) {
            Ok(call) => format!("{head}  {}: {}", call.client, call.what),
            Err(_) => format!("{head}  a call this terminal does not read"),
        },
        EventKind::VerbResult => {
            match serde_json::from_value::<VerbResult>(event.payload.clone()) {
                Ok(result) => {
                    let mark = if result.ok { "" } else { "not done: " };
                    let text = result.text.trim_end();
                    match text.split_once('\n') {
                        None => format!("{head}  {mark}{text}"),
                        Some((first, rest)) => format!("{head}  {mark}{first}\n{}", indent(rest)),
                    }
                }
                Err(_) => format!("{head}  a result this terminal does not read"),
            }
        }
        EventKind::Error => match serde_json::from_value::<Problem>(event.payload.clone()) {
            Ok(problem) => format!("{head}  error: {}", problem.text),
            Err(_) => format!("{head}  an error this terminal does not read"),
        },
        EventKind::UserMessage | EventKind::AgentText => {
            format!("{head}  {}", event.payload)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_read() {
        assert_eq!(parse("  ", "island"), Ok(Typed::Nothing));
        assert_eq!(
            parse("see", "island"),
            Ok(Typed::See {
                stream: None,
                entries: None
            })
        );
        assert_eq!(
            parse("see island 5", "x"),
            Ok(Typed::See {
                stream: Some("island".into()),
                entries: Some(5)
            })
        );
        assert!(parse("see island five", "x").is_err());
        assert_eq!(
            parse("verse decision keep grant ids as pairs", "island"),
            Ok(Typed::Verse {
                stream: "island".into(),
                verse: Verse::new(Evidence::Decision, "keep grant ids as pairs", vec![]),
            })
        );
        let hash = "ab".repeat(32);
        assert_eq!(
            parse(
                &format!("verse @flare artefact ref={hash} step 9 built"),
                "island"
            ),
            Ok(Typed::Verse {
                stream: "flare".into(),
                verse: Verse::new(Evidence::Artefact, "step 9 built", vec![[0xab; 32]]),
            })
        );
        // A ref after the text starts is text.
        assert_eq!(
            parse("verse decision use ref=x here", "island").map(|typed| match typed {
                Typed::Verse { verse, .. } => verse.text,
                _ => String::new(),
            }),
            Ok("use ref=x here".to_owned())
        );
        assert!(parse("verse opinion it went well", "island").is_err());
        assert!(parse("verse decision", "island").is_err());
        assert!(parse("verse artefact ref=abc built", "island").is_err());
        assert!(parse("dance", "island").is_err());
        assert_eq!(parse("quit", "island"), Ok(Typed::Quit));
    }
}
