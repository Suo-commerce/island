//! Flare terminal: attaches to a shared session on `flare-server`; see
//! the log, and propose verses signed with this device's key
//! (docs/spec.md, "One session, three ways in" and build step 9).
//!
//! The terminal is line by line: commands are typed, and every event of
//! the session is printed as it happens, whoever caused it. This library
//! holds what the program does that can be tested on its own: reading a
//! command line, and printing an event.

#![warn(missing_docs)]

use flare_core::session::{
    ActionSettled, ActionWaiting, AgentText, Event, EventKind, Problem, Settled, UserMessage,
    VerbCall, VerbResult,
};
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
    /// Text, or `/say TEXT`: tell the agent.
    Say {
        /// What to tell it, as typed.
        text: String,
    },
    /// `/waiting`: the actions waiting for an answer.
    Waiting,
    /// `/approve N`: approve the action shown as event N.
    Approve {
        /// The `action_waiting` event.
        event: u64,
    },
    /// `/decline N`: decline it.
    Decline {
        /// The `action_waiting` event.
        event: u64,
    },
    /// `help`.
    Help,
    /// `quit`, or the end of input.
    Quit,
    /// Nothing typed.
    Nothing,
}

/// What `/help` prints.
pub const HELP: &str = "\
Anything typed is said to the agent; its reply comes to the session as
`agent`. Commands start with a slash:
  /see                              the overview of the log
  /see STREAM [N]                   the newest N entries of a stream (20)
  /verse EVIDENCE [ref=HASH] TEXT   propose a verse on the default stream:
                                    EVIDENCE is decision, artefact,
                                    measurement or closed-question;
                                    HASH is 64 hex digits
  /verse @STREAM EVIDENCE ... TEXT  the same, on another stream
  /again                            propose the last proposal again, exactly
  /waiting                          the actions waiting for your answer
  /approve N                        approve the action shown as #N
  /decline N                        decline it
  /say TEXT                         say TEXT, even if it starts with a slash
  /help                             this
  /quit                             leave; the session goes on
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
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(Typed::Nothing);
    }
    // Anything that is not a command is said to the agent.
    let Some(command) = trimmed.strip_prefix('/') else {
        return Ok(Typed::Say {
            text: trimmed.to_owned(),
        });
    };
    let mut words = command.split_whitespace();
    let Some(first) = words.next() else {
        return Err("a slash starts a command; /help lists them".to_owned());
    };
    let event = |words: &mut std::str::SplitWhitespace<'_>, verb: &str| -> Result<u64, String> {
        let number = words.next().ok_or_else(|| {
            format!("/{verb} needs the number of the action, as /waiting shows it")
        })?;
        number
            .trim_start_matches('#')
            .parse()
            .map_err(|_| format!("`{number}` is not an action's number"))
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
        "say" => {
            let text = command["say".len()..].trim();
            if text.is_empty() {
                return Err("/say needs something to say".to_owned());
            }
            Ok(Typed::Say {
                text: text.to_owned(),
            })
        }
        "waiting" => Ok(Typed::Waiting),
        "approve" => Ok(Typed::Approve {
            event: event(&mut words, "approve")?,
        }),
        "decline" => Ok(Typed::Decline {
            event: event(&mut words, "decline")?,
        }),
        "help" | "?" => Ok(Typed::Help),
        "quit" | "exit" => Ok(Typed::Quit),
        other => Err(format!("`/{other}` is not a command; /help lists them")),
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
        EventKind::UserMessage => {
            match serde_json::from_value::<UserMessage>(event.payload.clone()) {
                Ok(message) => format!(
                    "{head}  {} → agent: {}",
                    message.client,
                    multiline(&message.text, &indent)
                ),
                Err(_) => format!("{head}  a message this terminal does not read"),
            }
        }
        EventKind::ActionWaiting => {
            match serde_json::from_value::<ActionWaiting>(event.payload.clone()) {
                Ok(action) => format!(
                    "{head}  waiting for you: Vessel {}'s {}\n    /approve {} or /decline {}",
                    action.vessel.get(..8).unwrap_or(&action.vessel),
                    action.shown,
                    event.seq,
                    event.seq
                ),
                Err(_) => format!("{head}  an action this terminal does not read"),
            }
        }
        EventKind::ActionSettled => {
            match serde_json::from_value::<ActionSettled>(event.payload.clone()) {
                Ok(settled) => {
                    let which = settled
                        .request_hash
                        .get(..8)
                        .unwrap_or(&settled.request_hash);
                    let at = settled
                        .entry
                        .map(|seq| format!(" at entry {seq}"))
                        .unwrap_or_default();
                    match settled.settled {
                        Settled::Sent => format!("{head}  sent: request {which}, written{at}"),
                        Settled::Declined => format!("{head}  declined: request {which}{at}"),
                        Settled::Refused => format!(
                            "{head}  refused when sent: request {which}: {}",
                            settled.why.unwrap_or_default()
                        ),
                    }
                }
                Err(_) => format!("{head}  an action this terminal does not read"),
            }
        }
        EventKind::AgentText => match serde_json::from_value::<AgentText>(event.payload.clone()) {
            Ok(reply) => format!(
                "{head}  agent (to #{}): {}",
                reply.task,
                multiline(&reply.text, &indent)
            ),
            Err(_) => format!("{head}  a reply this terminal does not read"),
        },
    }
}

/// A text on the event's line, its further lines indented below.
fn multiline(text: &str, indent: &dyn Fn(&str) -> String) -> String {
    let text = text.trim_end();
    match text.split_once('\n') {
        None => text.to_owned(),
        Some((first, rest)) => format!("{first}\n{}", indent(rest)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_read() {
        assert_eq!(parse("  ", "island"), Ok(Typed::Nothing));
        assert_eq!(
            parse("/see", "island"),
            Ok(Typed::See {
                stream: None,
                entries: None
            })
        );
        assert_eq!(
            parse("/see island 5", "x"),
            Ok(Typed::See {
                stream: Some("island".into()),
                entries: Some(5)
            })
        );
        assert!(parse("/see island five", "x").is_err());
        assert_eq!(
            parse("/verse decision keep grant ids as pairs", "island"),
            Ok(Typed::Verse {
                stream: "island".into(),
                verse: Verse::new(Evidence::Decision, "keep grant ids as pairs", vec![]),
            })
        );
        let hash = "ab".repeat(32);
        assert_eq!(
            parse(
                &format!("/verse @flare artefact ref={hash} step 9 built"),
                "island"
            ),
            Ok(Typed::Verse {
                stream: "flare".into(),
                verse: Verse::new(Evidence::Artefact, "step 9 built", vec![[0xab; 32]]),
            })
        );
        // A ref after the text starts is text.
        assert_eq!(
            parse("/verse decision use ref=x here", "island").map(|typed| match typed {
                Typed::Verse { verse, .. } => verse.text,
                _ => String::new(),
            }),
            Ok("use ref=x here".to_owned())
        );
        assert!(parse("/verse opinion it went well", "island").is_err());
        assert!(parse("/verse decision", "island").is_err());
        assert!(parse("/verse artefact ref=abc built", "island").is_err());
        assert!(parse("/dance", "island").is_err());
        assert_eq!(
            parse("dance with me", "island"),
            Ok(Typed::Say {
                text: "dance with me".into()
            })
        );
        assert_eq!(
            parse("/approve #12", "island"),
            Ok(Typed::Approve { event: 12 })
        );
        assert_eq!(
            parse("/decline 3", "island"),
            Ok(Typed::Decline { event: 3 })
        );
        assert!(parse("/approve", "island").is_err());
        assert_eq!(parse("/waiting", "island"), Ok(Typed::Waiting));
        assert_eq!(parse("/quit", "island"), Ok(Typed::Quit));
        assert_eq!(
            parse("  draft the verse   for that ", "island"),
            Ok(Typed::Say {
                text: "draft the verse   for that".into()
            })
        );
        assert_eq!(
            parse("/say /see is a command", "island"),
            Ok(Typed::Say {
                text: "/see is a command".into()
            })
        );
        assert!(parse("/say   ", "island").is_err());
    }
}

#[cfg(test)]
mod shown {
    use super::*;

    #[test]
    fn a_message_and_its_reply_read_as_a_conversation() {
        let said = Event {
            seq: 4,
            at: 1_794_400_000_000_000,
            kind: EventKind::UserMessage,
            payload: serde_json::json!({ "id": "t-1", "client": "laptop", "text": "draft it" }),
        };
        assert!(
            show(&said).ends_with("laptop → agent: draft it"),
            "{}",
            show(&said)
        );
        let reply = Event {
            seq: 5,
            at: 1_794_400_001_000_000,
            kind: EventKind::AgentText,
            payload: serde_json::json!({ "task": 4, "text": "Here:\nline two", "vessel": "ab", "key": "cd" }),
        };
        assert!(
            show(&reply).ends_with("agent (to #4): Here:\n    line two"),
            "{}",
            show(&reply)
        );
    }
}
