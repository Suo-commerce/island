//! The loop: call the model, run the tools it asks for, give it the
//! results, again, until it answers without a tool call.
//!
//! Ported from Pi, `packages/agent/src/agent-loop.ts`
//! (<https://github.com/badlogic/pi-mono>), Copyright (c) 2025 Mario
//! Zechner, MIT License (PI-LICENSE). Kept: the loop itself; an unknown
//! tool, bad arguments and a failing tool answered as error results the
//! model reads; every tool call of an answer cut off at the output limit
//! failed rather than run on arguments that may be cut short; a provider's
//! error ending the loop. Changed: tool calls run one after another, in
//! the order asked (Pi runs them side by side), so the Vessel's files see
//! one change at a time. Left out: steering and follow-up messages (a new
//! message from Jarmo is the next task), compaction, the before and after
//! hooks (the seam where Stormgate would sit stays empty in this slice),
//! and events for a terminal. Added: `fork` and `promote` (docs/slice-2.md,
//! "Forking, step by step"), which are the run's to carry out; a
//! promotion ends the loop, since Taso ends the run for it.

use serde_json::{Value, json};

use crate::message::{Assistant, Message, Stop};
use crate::tools::{Outcome, Spec, run_file_tool};

/// One model call.
#[derive(Clone, Debug)]
pub struct Completion {
    /// The answer.
    pub assistant: Assistant,
    /// The call's digest, the same the island socket's `access` entry
    /// counts.
    pub digest: [u8; 32],
    /// Bytes sent.
    pub bytes_in: usize,
    /// Bytes received.
    pub bytes_out: usize,
}

/// The model.
pub trait Model {
    /// The model's answer to `messages`, with `tools` to call.
    fn complete(&mut self, messages: &[Message], tools: &[Spec]) -> Result<Completion, String>;
}

/// What the loop needs from the run around it.
pub trait Host {
    /// The messages to send: a window of the conversation.
    fn window(&self, history: &[Message]) -> Vec<Message>;
    /// Say something to Jarmo now (the tool `say`).
    fn say(&mut self, text: &str) -> Result<String, String>;
    /// Propose a verse on the agent's own stream (the tool `verse`).
    fn verse(&mut self, verse: island_core::Verse) -> Result<String, String>;
    /// Record a step in the Vessel's transcript.
    fn record(&mut self, entry: Value);
    /// A round of tool calls ended: the run may end a turn here.
    fn round(&mut self, history: &[Message]) -> Result<(), String>;
    /// A tool is about to run (its name is known and its arguments are an
    /// object).
    fn begin(&mut self, _tool: &str) {}
    /// Start one fork child per task and wait for their notes (the tool
    /// `fork`). `call` is the tool call's id and `history` the
    /// conversation up to it: the turn ends here, and a child starts on
    /// what that Seal holds.
    fn fork(
        &mut self,
        _tasks: Vec<String>,
        _call: &str,
        _history: &[Message],
    ) -> Result<String, String> {
        Err("this agent cannot fork".to_owned())
    }
    /// Adopt one fork child (the tool `promote`).
    fn promote(&mut self, _child: &str) -> Result<String, String> {
        Err("this agent cannot promote".to_owned())
    }
    /// Whether the run is over: a promotion ends it, and the loop stops.
    fn over(&self) -> bool {
        false
    }
}

/// How the loop ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The model answered: its text (perhaps empty).
    Answered(String),
    /// It could not go on.
    Failed(String),
    /// A fork child was promoted: Taso ends the run, and the next start is
    /// on the child's files.
    Promoted,
}

/// The most model calls one task may take.
pub const MAX_CALLS: usize = 40;

/// Run the loop on `history`, which grows with every message.
pub fn run(
    model: &mut dyn Model,
    host: &mut dyn Host,
    history: &mut Vec<Message>,
    tools: &[Spec],
) -> Ended {
    // A run restarted after its model had answered: the answer stands.
    if let Some(Message::Assistant(last)) = history.last()
        && last.calls.is_empty()
    {
        return match &last.stop {
            Stop::Error(why) => Ended::Failed(why.clone()),
            _ => Ended::Answered(last.text.clone()),
        };
    }
    for _ in 0..MAX_CALLS {
        let messages = host.window(history);
        let completion = match model.complete(&messages, tools) {
            Ok(completion) => completion,
            Err(why) => {
                host.record(json!({ "kind": "model_error", "error": why }));
                return Ended::Failed(why);
            }
        };
        let answer = completion.assistant;
        host.record(json!({
            "kind": "model",
            "digest": hex(&completion.digest),
            "bytes_in": completion.bytes_in,
            "bytes_out": completion.bytes_out,
            "message": answer,
        }));
        history.push(Message::Assistant(answer.clone()));
        if let Stop::Error(why) = &answer.stop {
            return Ended::Failed(why.clone());
        }
        if answer.calls.is_empty() {
            return Ended::Answered(answer.text);
        }
        for (index, call) in answer.calls.iter().enumerate() {
            let last = index + 1 == answer.calls.len();
            let outcome = if answer.stop == Stop::Length {
                Outcome::error(format!(
                    "Tool call \"{}\" was not executed: the response hit the output token limit, \
                     so its arguments may be truncated. Re-issue the tool call with complete \
                     arguments.",
                    call.name
                ))
            } else {
                call_tool(host, tools, call, last, history)
            };
            let result = Message::Tool {
                call_id: call.id.clone(),
                name: call.name.clone(),
                text: outcome.text,
                is_error: outcome.is_error,
            };
            host.record(json!({ "kind": "tool", "message": result }));
            history.push(result);
            if host.over() {
                return Ended::Promoted;
            }
        }
        if let Err(why) = host.round(history) {
            return Ended::Failed(why);
        }
    }
    Ended::Failed(format!("stopped after {MAX_CALLS} model calls on one task"))
}

fn call_tool(
    host: &mut dyn Host,
    tools: &[Spec],
    call: &crate::message::ToolCall,
    last: bool,
    history: &[Message],
) -> Outcome {
    let (name, arguments) = (call.name.as_str(), call.arguments.as_str());
    if !tools.iter().any(|tool| tool.name == name) {
        return Outcome::error(format!("Tool {name} not found"));
    }
    let args: Value = match serde_json::from_str(arguments) {
        Ok(args @ Value::Object(_)) => args,
        _ => {
            return Outcome::error(format!(
                "Validation failed for tool \"{name}\": the arguments are not a JSON object: {arguments}"
            ));
        }
    };
    if name == "fork" {
        // The turn ends inside this call, and the children start on what
        // that Seal holds: a call after it in the same message would be
        // theirs to run too.
        if !last {
            return Outcome::error(
                "fork was not run: call it alone, as the last tool call of a message, since \
                 your turn ends there and the children start from it. Call it again.",
            );
        }
        return match fork_tasks(&args) {
            Ok(tasks) => {
                host.begin(name);
                match host.fork(tasks, &call.id, history) {
                    Ok(done) => Outcome::text(done),
                    Err(why) => Outcome::error(why),
                }
            }
            Err(why) => Outcome::error(why),
        };
    }
    host.begin(name);
    if name == "promote" {
        return match args.get("child").and_then(Value::as_str) {
            Some(child) => match host.promote(child.trim()) {
                Ok(done) => Outcome::text(done),
                Err(why) => Outcome::error(why),
            },
            None => {
                Outcome::error("Validation failed for tool arguments: `child` must be a string")
            }
        };
    }
    if name == "verse" {
        return match verse_of(&args) {
            Ok(verse) => match host.verse(verse) {
                Ok(done) => Outcome::text(done),
                Err(why) => Outcome::error(why),
            },
            Err(why) => Outcome::error(why),
        };
    }
    if name == "say" {
        return match args.get("text").and_then(Value::as_str) {
            Some(text) => match host.say(text) {
                Ok(done) => Outcome::text(done),
                Err(why) => Outcome::error(why),
            },
            None => Outcome::error("Validation failed for tool arguments: `text` must be a string"),
        };
    }
    run_file_tool(name, &args).unwrap_or_else(|| Outcome::error(format!("Tool {name} not found")))
}

/// The tasks the arguments of `fork` give: one per child.
fn fork_tasks(args: &Value) -> Result<Vec<String>, String> {
    use crate::tools::{MAX_CHILDREN, MAX_FORK_TASK};
    let tasks: Vec<String> = match args.get("tasks") {
        Some(Value::Array(tasks)) => tasks
            .iter()
            .map(|task| {
                task.as_str()
                    .map(|task| task.trim().to_owned())
                    .filter(|task| !task.is_empty())
                    .ok_or("Validation failed for tool arguments: each of `tasks` must be a string with something in it")
            })
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(
                "Validation failed for tool arguments: `tasks` must be a list of strings, one \
                 task per child"
                    .to_owned(),
            );
        }
    };
    if tasks.is_empty() || tasks.len() > MAX_CHILDREN {
        return Err(format!(
            "fork takes 1 to {MAX_CHILDREN} tasks, one child each, not {}",
            tasks.len()
        ));
    }
    if let Some(long) = tasks.iter().find(|task| task.len() > MAX_FORK_TASK) {
        return Err(format!(
            "a child's task is at most {MAX_FORK_TASK} bytes; one of these is {}. Put the \
             material in a file first: the children get your files.",
            long.len()
        ));
    }
    Ok(tasks)
}

/// The verse the arguments of `verse` describe.
fn verse_of(args: &Value) -> Result<island_core::Verse, String> {
    use island_core::Evidence;
    let evidence = match args.get("evidence").and_then(Value::as_str) {
        Some("decision") => Evidence::Decision,
        Some("artefact") => Evidence::Artefact,
        Some("measurement") => Evidence::Measurement,
        Some("closed-question" | "closed_question" | "closed question") => Evidence::ClosedQuestion,
        other => {
            return Err(format!(
                "Validation failed for tool arguments: `evidence` must be decision, artefact, \
                 measurement or closed-question, not {other:?}"
            ));
        }
    };
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or(
            "Validation failed for tool arguments: `text` must be a string with something in it",
        )?;
    let refs = match args.get("refs") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(refs)) => refs
            .iter()
            .map(|r| {
                r.as_str()
                    .and_then(island_client::approval::unhex::<32>)
                    .ok_or_else(|| format!("a ref is 64 hex digits, not {r}"))
            })
            .collect::<Result<_, _>>()?,
        Some(other) => return Err(format!("`refs` must be a list of hashes, not {other}")),
    };
    Ok(island_core::Verse::new(evidence, text.trim(), refs))
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolCall;
    use crate::tools::{specs, specs_with_forks};

    /// Answers from a list.
    pub struct Scripted(pub Vec<Assistant>);

    impl Model for Scripted {
        fn complete(&mut self, _: &[Message], _: &[Spec]) -> Result<Completion, String> {
            if self.0.is_empty() {
                return Err("the script ended".to_owned());
            }
            Ok(Completion {
                assistant: self.0.remove(0),
                digest: [7; 32],
                bytes_in: 1,
                bytes_out: 2,
            })
        }
    }

    #[derive(Default)]
    struct Recorder {
        said: Vec<String>,
        records: Vec<Value>,
        rounds: usize,
    }

    impl Host for Recorder {
        fn window(&self, history: &[Message]) -> Vec<Message> {
            history.to_vec()
        }
        fn say(&mut self, text: &str) -> Result<String, String> {
            self.said.push(text.to_owned());
            Ok("Sent.".to_owned())
        }
        fn verse(&mut self, verse: island_core::Verse) -> Result<String, String> {
            self.said.push(format!("verse: {}", verse.text));
            Ok("Proposed.".to_owned())
        }
        fn record(&mut self, entry: Value) {
            self.records.push(entry);
        }
        fn round(&mut self, _: &[Message]) -> Result<(), String> {
            self.rounds += 1;
            Ok(())
        }
    }

    fn answer(text: &str, calls: &[(&str, &str)], stop: Stop) -> Assistant {
        Assistant {
            text: text.to_owned(),
            reasoning: String::new(),
            calls: calls
                .iter()
                .enumerate()
                .map(|(i, (name, arguments))| ToolCall {
                    id: format!("c{i}"),
                    name: (*name).to_owned(),
                    arguments: (*arguments).to_owned(),
                })
                .collect(),
            stop,
        }
    }

    fn results(history: &[Message]) -> Vec<(String, bool)> {
        history
            .iter()
            .filter_map(|message| match message {
                Message::Tool { text, is_error, .. } => Some((text.clone(), *is_error)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tools_run_until_the_model_answers() {
        let mut model = Scripted(vec![
            answer(
                "",
                &[
                    ("say", r#"{"text":"on it"}"#),
                    ("nope", "{}"),
                    ("bash", "not json"),
                ],
                Stop::ToolUse,
            ),
            answer("Done.", &[], Stop::Stop),
        ]);
        let mut host = Recorder::default();
        let mut history = vec![Message::User { text: "go".into() }];
        let ended = run(&mut model, &mut host, &mut history, &specs());
        assert_eq!(ended, Ended::Answered("Done.".into()));
        assert_eq!(host.said, ["on it"]);
        let results = results(&history);
        assert_eq!(results[0], ("Sent.".into(), false));
        assert_eq!(results[1], ("Tool nope not found".into(), true));
        assert!(
            results[2].1
                && results[2]
                    .0
                    .starts_with("Validation failed for tool \"bash\"")
        );
        assert_eq!(host.rounds, 1);
        // Two model calls and three results recorded, each call with its digest.
        assert_eq!(host.records.len(), 5);
        assert_eq!(host.records[0]["digest"], hex(&[7; 32]));
    }

    #[test]
    fn calls_cut_at_the_output_limit_are_not_run() {
        let mut model = Scripted(vec![
            answer("", &[("say", r#"{"text":"half"}"#)], Stop::Length),
            answer("ok", &[], Stop::Stop),
        ]);
        let mut host = Recorder::default();
        let mut history = Vec::new();
        assert_eq!(
            run(&mut model, &mut host, &mut history, &specs()),
            Ended::Answered("ok".into())
        );
        assert!(host.said.is_empty());
        assert!(
            results(&history)[0]
                .0
                .contains("hit the output token limit")
        );
    }

    #[test]
    fn a_failing_model_ends_the_loop_and_an_answer_stands() {
        let mut host = Recorder::default();
        let mut history = Vec::new();
        let ended = run(
            &mut Scripted(vec![answer("", &[], Stop::Error("overloaded".into()))]),
            &mut host,
            &mut history,
            &specs(),
        );
        assert_eq!(ended, Ended::Failed("overloaded".into()));
        assert_eq!(
            run(&mut Scripted(Vec::new()), &mut host, &mut vec![], &specs()),
            Ended::Failed("the script ended".into())
        );
        // Restarted after the answer: not asked again.
        let mut history = vec![Message::Assistant(answer("Here.", &[], Stop::Stop))];
        assert_eq!(
            run(&mut Scripted(Vec::new()), &mut host, &mut history, &specs()),
            Ended::Answered("Here.".into())
        );
    }

    #[test]
    fn a_verse_is_read_from_its_arguments() {
        let mut model = Scripted(vec![
            answer(
                "",
                &[
                    (
                        "verse",
                        r#"{"evidence":"decision","text":" keep it small "}"#,
                    ),
                    ("verse", r#"{"evidence":"opinion","text":"x"}"#),
                    (
                        "verse",
                        r#"{"evidence":"artefact","text":"x","refs":["zz"]}"#,
                    ),
                ],
                Stop::ToolUse,
            ),
            answer("ok", &[], Stop::Stop),
        ]);
        let mut host = Recorder::default();
        let mut history = Vec::new();
        run(&mut model, &mut host, &mut history, &specs());
        assert_eq!(host.said, ["verse: keep it small"]);
        let results = results(&history);
        assert!(results[1].1 && results[1].0.contains("`evidence` must be"));
        assert!(results[2].1 && results[2].0.contains("64 hex digits"));
    }

    #[test]
    fn fork_is_run_only_as_the_last_call_with_its_tasks_and_is_the_runs_to_do() {
        let two = r#"{"tasks":["one way"," another "]}"#;
        let mut model = Scripted(vec![
            answer(
                "",
                &[("fork", two), ("say", r#"{"text":"after"}"#)],
                Stop::ToolUse,
            ),
            answer("", &[("fork", r#"{"tasks":[]}"#)], Stop::ToolUse),
            answer("", &[("fork", r#"{"tasks":"one"}"#)], Stop::ToolUse),
            answer("", &[("promote", "{}"), ("fork", two)], Stop::ToolUse),
            answer("ok", &[], Stop::Stop),
        ]);
        let mut host = Recorder::default();
        let mut history = Vec::new();
        run(&mut model, &mut host, &mut history, &specs_with_forks());
        let results = results(&history);
        // Not the last call of its message: not run, and the say after it is.
        assert!(
            results[0].1 && results[0].0.contains("call it alone"),
            "{results:?}"
        );
        assert_eq!(results[1], ("Sent.".into(), false));
        assert!(
            results[2].1 && results[2].0.contains("1 to 4 tasks"),
            "{results:?}"
        );
        assert!(
            results[3].1 && results[3].0.contains("list of strings"),
            "{results:?}"
        );
        assert!(
            results[4].1 && results[4].0.contains("`child` must be a string"),
            "{results:?}"
        );
        // Well formed and last: handed to the run, which here cannot fork.
        assert_eq!(results[5], ("this agent cannot fork".into(), true));
        // An agent without the fork tools is not offered them.
        let mut history = Vec::new();
        run(
            &mut Scripted(vec![
                answer("", &[("fork", two)], Stop::ToolUse),
                answer("ok", &[], Stop::Stop),
            ]),
            &mut host,
            &mut history,
            &specs(),
        );
        assert_eq!(
            super::tests::results(&history)[0],
            ("Tool fork not found".into(), true)
        );
    }

    #[test]
    fn a_task_cannot_call_forever() {
        let calls: Vec<Assistant> = (0..MAX_CALLS + 1)
            .map(|_| answer("", &[("say", r#"{"text":"again"}"#)], Stop::ToolUse))
            .collect();
        let mut host = Recorder::default();
        let ended = run(&mut Scripted(calls), &mut host, &mut Vec::new(), &specs());
        assert!(matches!(ended, Ended::Failed(why) if why.contains("40 model calls")));
    }
}
