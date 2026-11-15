//! The agent's run against a stand-in Warden, a scripted model and tasks
//! from a list: what it declares, records and keeps, across a restart.

use std::sync::Mutex;
use std::time::Duration;

use flare_agent::agent_loop::{Completion, Model};
use flare_agent::field::{Declared, Field};
use flare_agent::island::Tasks;
use flare_agent::message::{Assistant, Message, Stop, ToolCall};
use flare_agent::run::{Agent, State, action_key, load, save};
use flare_agent::tools::{Spec, set_work};
use flare_core::session::{SayRequest, Task};

/// One test at a time changes the work directory.
static WORK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct Warden {
    last_sealed: Option<u64>,
    declared: Vec<(String, Vec<u8>, Vec<u8>)>,
    transcript: Vec<serde_json::Value>,
    turns: Vec<u64>,
    /// Refuse this many declarations first, as Taso does while an
    /// earlier action's fate is open.
    refuse: usize,
}

impl Field for Warden {
    fn last_sealed_turn(&self) -> Option<u64> {
        self.last_sealed
    }
    fn declare(&mut self, provider: &str, request: &[u8], key: &[u8]) -> Result<Declared, String> {
        if self.refuse > 0 {
            self.refuse -= 1;
            return Ok(Declared::Refused(
                "no effect authority: 1 unresolved effect(s)".into(),
            ));
        }
        self.declared
            .push((provider.into(), request.into(), key.into()));
        Ok(Declared::Released(self.declared.len() as u64))
    }
    fn transcript(&mut self, entry: &[u8]) -> Result<(), String> {
        self.transcript.push(serde_json::from_slice(entry).unwrap());
        Ok(())
    }
    fn turn_end(&mut self, turn: u64) -> Result<Option<u64>, String> {
        self.turns.push(turn);
        self.last_sealed = Some(turn);
        Ok(Some(100 + turn))
    }
}

struct Script(Vec<Assistant>, Vec<Vec<Message>>);

impl Model for Script {
    fn complete(&mut self, messages: &[Message], _: &[Spec]) -> Result<Completion, String> {
        self.1.push(messages.to_vec());
        if self.0.is_empty() {
            return Err("the script ended".into());
        }
        Ok(Completion {
            assistant: self.0.remove(0),
            digest: [9; 32],
            bytes_in: 10,
            bytes_out: 20,
        })
    }
}

struct List(Vec<Task>, Vec<Option<u64>>);

impl Tasks for List {
    fn next(&mut self, after: Option<u64>) -> Result<Option<Task>, String> {
        self.1.push(after);
        Ok(self
            .0
            .iter()
            .find(|task| after.is_none_or(|after| task.id > after))
            .cloned())
    }
}

fn task(id: u64, text: &str) -> Task {
    Task {
        id,
        at: 1,
        client: "laptop".into(),
        text: text.into(),
    }
}

fn answer(text: &str, calls: &[(&str, serde_json::Value)]) -> Assistant {
    Assistant {
        text: text.into(),
        reasoning: String::new(),
        calls: calls
            .iter()
            .enumerate()
            .map(|(i, (name, args))| ToolCall {
                id: format!("c{i}"),
                name: (*name).into(),
                arguments: args.to_string(),
            })
            .collect(),
        stop: if calls.is_empty() {
            Stop::Stop
        } else {
            Stop::ToolUse
        },
    }
}

fn said(declared: &(String, Vec<u8>, Vec<u8>)) -> SayRequest {
    assert_eq!(declared.0, "flare.say");
    serde_json::from_slice(&declared.1).unwrap()
}

#[test]
fn a_task_is_worked_answered_and_sealed_and_the_next_start_goes_on_after_it() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());

    let mut warden = Warden {
        last_sealed: Some(6),
        ..Warden::default()
    };
    let mut model = Script(
        vec![
            answer(
                "",
                &[
                    (
                        "write",
                        serde_json::json!({ "path": "notes/draft.md", "content": "draft" }),
                    ),
                    ("say", serde_json::json!({ "text": "Drafting now." })),
                ],
            ),
            answer("The draft is in notes/draft.md.", &[]),
        ],
        Vec::new(),
    );
    let mut tasks = List(vec![task(3, "draft the verse for that")], Vec::new());
    {
        let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
        assert_eq!(agent.turn(), 7, "after the last sealed turn");
        assert_eq!(agent.step(), Ok(Some(3)));
        assert_eq!(
            agent.state(),
            &State {
                cursor: Some(3),
                working: None,
                says: 0
            }
        );
        assert_eq!(agent.step(), Ok(None), "no next task");
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes/draft.md")).unwrap(),
        "draft"
    );
    // The say during the task, then the answer, each with its own key.
    assert_eq!(warden.declared.len(), 2);
    assert_eq!(
        said(&warden.declared[0]),
        SayRequest {
            task: 3,
            text: "Drafting now.".into()
        }
    );
    assert_eq!(warden.declared[0].2, action_key(3, "say", 0));
    assert_eq!(
        said(&warden.declared[1]),
        SayRequest {
            task: 3,
            text: "The draft is in notes/draft.md.".into()
        }
    );
    assert_eq!(warden.declared[1].2, action_key(3, "reply", 0));
    assert_eq!(warden.turns, vec![7]);
    // Each model message is in the transcript with the call's digest.
    let digests: Vec<_> = warden
        .transcript
        .iter()
        .filter(|e| e["step"]["kind"] == "model")
        .map(|e| e["step"]["digest"].clone())
        .collect();
    assert_eq!(digests, vec![serde_json::json!("09".repeat(32)); 2]);
    assert!(
        warden
            .transcript
            .iter()
            .all(|e| e["task"] == 3 && e["turn"] == 7)
    );
    // The task told to the model says who asked.
    assert!(
        matches!(&model.1[0][1], Message::User { text } if text == "[Task #3 from laptop]\ndraft the verse for that")
    );
    assert!(matches!(&model.1[0][0], Message::System { .. }));

    // The next start carries on after task 3, with the conversation.
    let mut model = Script(vec![answer("Yes.", &[])], Vec::new());
    let mut tasks = List(
        vec![task(3, "draft"), task(5, "and is it done?")],
        Vec::new(),
    );
    let mut warden = Warden {
        last_sealed: Some(7),
        ..Warden::default()
    };
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
    assert_eq!(agent.step(), Ok(Some(5)));
    drop(agent);
    assert_eq!(tasks.1, vec![Some(3)]);
    let asked = &model.1[0];
    assert!(
        asked
            .iter()
            .any(|m| matches!(m, Message::User { text } if text.contains("draft the verse")))
    );
    assert_eq!(warden.declared[0].2, action_key(5, "reply", 0));
}

#[test]
fn a_task_cut_by_a_crash_is_worked_again_with_the_same_keys() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    // What the last Seal held: task 8 begun, one say already declared.
    let state = State {
        cursor: Some(4),
        working: Some(task(8, "check the repo")),
        says: 1,
    };
    let history = vec![Message::User {
        text: "[Task #8 from laptop]\ncheck the repo".into(),
    }];
    save(&state, &history).unwrap();

    let mut warden = Warden {
        last_sealed: Some(11),
        refuse: 2,
        ..Warden::default()
    };
    let mut model = Script(
        vec![
            answer("", &[("say", serde_json::json!({ "text": "still at it" }))]),
            answer("All good.", &[]),
        ],
        Vec::new(),
    );
    let mut tasks = List(Vec::new(), Vec::new());
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
    agent.patience = Duration::from_secs(10);
    assert_eq!(agent.step(), Ok(Some(8)));
    drop(agent);
    assert!(tasks.1.is_empty(), "the begun task, not a new one");
    // Refused twice while an action was open, then released: once.
    assert_eq!(warden.declared.len(), 2);
    assert_eq!(
        warden.declared[0].2,
        action_key(8, "say", 1),
        "the say after the one already made"
    );
    assert_eq!(warden.declared[1].2, action_key(8, "reply", 0));
    let (state, _) = load().unwrap();
    assert_eq!(
        state,
        State {
            cursor: Some(8),
            working: None,
            says: 0
        }
    );
}

#[test]
fn a_model_that_fails_still_gets_a_reply_to_jarmo() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let mut warden = Warden::default();
    let mut model = Script(Vec::new(), Vec::new());
    let mut tasks = List(vec![task(0, "hello")], Vec::new());
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
    assert_eq!(agent.turn(), 1);
    assert_eq!(agent.step(), Ok(Some(0)));
    drop(agent);
    assert_eq!(
        said(&warden.declared[0]).text,
        "I could not finish this: the script ended"
    );
    assert_eq!(warden.turns, vec![1]);
}
