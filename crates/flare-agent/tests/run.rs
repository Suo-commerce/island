//! The agent's run against a stand-in Warden, a scripted model and tasks
//! from a list: what it declares, records and keeps, across a restart.

use std::sync::Mutex;
use std::time::Duration;

use flare_agent::agent_loop::{Completion, Model};
use flare_agent::field::{Declared, Field, Forked, Promoted};
use flare_agent::island::{Forks, Note, Progress, Tasks, Whoami};
use flare_agent::message::{Assistant, Message, Stop, ToolCall};
use flare_agent::run::{Agent, ForkSet, State, Stepped, Waits, action_key, fork_set, load, save};
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
    /// A fork child's Warden: every action is held.
    hold: bool,
    /// The names Taso gives the next children.
    children: Vec<String>,
    /// Refuse this many forks first, as Taso does until the Seal is
    /// shipped.
    unshipped: usize,
    /// The turns sealed when each fork was asked for.
    forked: Vec<(String, Option<u64>)>,
    /// The children promoted.
    promoted: Vec<String>,
    /// Refuse promotions, with this reason.
    no_promotion: Option<String>,
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
        Ok(if self.hold {
            Declared::Held(self.declared.len() as u64)
        } else {
            Declared::Released(self.declared.len() as u64)
        })
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
    fn fork(&mut self) -> Result<Forked, String> {
        if self.unshipped > 0 {
            self.unshipped -= 1;
            return Ok(Forked::Refused(
                "fork of \"flare-home\" at Last: the Seal at lsn 107 is not shipped yet \
                 (shipped up to lsn 90): a fork of it could be reborn nowhere if this node \
                 were lost"
                    .into(),
            ));
        }
        if self.children.is_empty() {
            return Ok(Forked::Refused(
                "no fork slot left: all 2 of \"flare-home\"'s are taken".into(),
            ));
        }
        let name = self.children.remove(0);
        self.forked.push((name.clone(), self.last_sealed));
        Ok(Forked::Child {
            name,
            seal: 100 + self.last_sealed.unwrap_or(0),
        })
    }
    fn promote(&mut self, child: &str) -> Result<Promoted, String> {
        if let Some(why) = &self.no_promotion {
            return Ok(Promoted::Refused(why.clone()));
        }
        self.promoted.push(child.into());
        Ok(Promoted::Adopted {
            seal: 205,
            ops: vec![31, 32],
        })
    }
}

/// The island socket's side of forking: who this is, and the notes, which
/// come as a list of answers, the last one staying.
struct Socket {
    me: Whoami,
    answers: Vec<Vec<Note>>,
    posted: Vec<(String, String, Progress)>,
    asked: usize,
}

impl Socket {
    fn new(role: &str, slot: u32, forks_left: u64, answers: Vec<Vec<Note>>) -> Self {
        Self {
            me: Whoami {
                vessel: "914c503c980b8f7f1750c664b5743d2c".into(),
                subject: if role == "prime" {
                    "flare-home".into()
                } else {
                    format!("flare-home-fork-{slot}")
                },
                role: role.into(),
                slot,
                prime: "flare-home".into(),
                forks_left,
                fork_slots: vec![11, 12, 13, 14],
                default_model: "scripted".into(),
            },
            answers,
            posted: Vec::new(),
            asked: 0,
        }
    }
}

impl Forks for Socket {
    fn whoami(&mut self) -> Result<Whoami, String> {
        Ok(self.me.clone())
    }
    fn post_note(&mut self, set: &str, text: &str, progress: Progress) -> Result<(), String> {
        self.posted.push((set.into(), text.into(), progress));
        Ok(())
    }
    fn notes(&mut self) -> Result<Vec<Note>, String> {
        self.asked += 1;
        Ok(if self.answers.len() > 1 {
            self.answers.remove(0)
        } else {
            self.answers.first().cloned().unwrap_or_default()
        })
    }
}

fn note_of(slot: u32, set: &str, text: &str, done: bool) -> Note {
    Note {
        child: format!("flare-home-fork-{slot}"),
        slot,
        set: set.into(),
        text: text.into(),
        done,
        failed: false,
    }
}

/// Waits short enough for a test.
fn quick() -> Waits {
    Waits {
        patience: Duration::from_millis(50),
        forks: Duration::from_secs(1),
        unstarted: Duration::from_millis(200),
        poll: Duration::from_millis(10),
        retry: Duration::from_millis(10),
        fork_tries: 5,
        note_tries: 3,
        turn: flare_agent::run::TURN,
    }
}

fn tool_results(history: &[Message]) -> Vec<(String, String, bool)> {
    history
        .iter()
        .filter_map(|message| match message {
            Message::Tool {
                name,
                text,
                is_error,
                ..
            } => Some((name.clone(), text.clone(), *is_error)),
            _ => None,
        })
        .collect()
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

struct List(Vec<Task>, Vec<Option<u64>>, Option<String>);

impl Tasks for List {
    fn next(&mut self, after: Option<u64>) -> Result<Option<Task>, String> {
        self.1.push(after);
        Ok(self
            .0
            .iter()
            .find(|task| after.is_none_or(|after| task.id > after))
            .cloned())
    }
    fn instructions(&mut self) -> Option<String> {
        self.2.clone()
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
    let mut tasks = List(vec![task(3, "draft the verse for that")], Vec::new(), None);
    {
        let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
        assert_eq!(agent.turn(), 7, "after the last sealed turn");
        assert_eq!(agent.step(), Ok(Stepped::Done(3)));
        assert_eq!(
            agent.state(),
            &State {
                cursor: Some(3),
                ..State::default()
            }
        );
        assert_eq!(agent.step(), Ok(Stepped::Idle), "no next task");
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
        None,
    );
    let mut warden = Warden {
        last_sealed: Some(7),
        ..Warden::default()
    };
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
    assert_eq!(agent.step(), Ok(Stepped::Done(5)));
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
        ..State::default()
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
    let mut tasks = List(Vec::new(), Vec::new(), None);
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
    agent.waits.patience = Duration::from_secs(10);
    assert_eq!(agent.step(), Ok(Stepped::Done(8)));
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
            ..State::default()
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
    let mut tasks = List(vec![task(0, "hello")], Vec::new(), None);
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
    assert_eq!(agent.turn(), 1);
    assert_eq!(agent.step(), Ok(Stepped::Done(0)));
    drop(agent);
    assert_eq!(
        said(&warden.declared[0]).text,
        "I could not finish this: the script ended"
    );
    assert_eq!(warden.turns, vec![1]);
}

#[test]
fn a_verse_is_proposed_on_flare_and_the_instructions_come_from_island() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let mut warden = Warden::default();
    let mut model = Script(
        vec![
            answer(
                "",
                &[(
                    "verse",
                    serde_json::json!({ "evidence": "decision", "text": "Step 5 verses wait for approval" }),
                )],
            ),
            answer("  I proposed a verse.\n\n", &[]),
        ],
        Vec::new(),
    );
    let mut tasks = List(
        vec![task(9, "write a verse")],
        Vec::new(),
        Some("Be brief.".into()),
    );
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
    assert_eq!(agent.step(), Ok(Stepped::Done(9)));
    drop(agent);
    // The verse: on flare, with its key; then the reply, trimmed.
    assert_eq!(warden.declared[0].0, "island.verse");
    let (stream, verse) =
        island_client::actions::parse_verse_request(&warden.declared[0].1).unwrap();
    assert_eq!(
        (stream.as_str(), verse.text.as_str()),
        ("flare", "Step 5 verses wait for approval")
    );
    assert_eq!(warden.declared[0].2, action_key(9, "verse", 0));
    assert_eq!(said(&warden.declared[1]).text, "I proposed a verse.");
    // The model ran on Island's instructions, and the transcript says so.
    assert!(matches!(&model.1[0][0], Message::System { text } if text == "Be brief."));
    let noted = warden
        .transcript
        .iter()
        .find(|e| e["step"]["kind"] == "instructions")
        .unwrap();
    assert_eq!(noted["step"]["source"], "island-socket");
    assert_eq!(
        noted["step"]["digest"],
        blake3::hash(b"Be brief.").to_hex().as_str()
    );
}

/// The model's first answer on a task: fork two children.
fn fork_two() -> Assistant {
    let mut asked = answer(
        "Two ways to draft this.",
        &[(
            "fork",
            serde_json::json!({ "tasks": ["Draft it plainly, in plain.md", "Draft it as a list, in list.md"] }),
        )],
    );
    asked.calls[0].id = "call-fork".into();
    asked
}

/// The prime up to its fork's Seal, in the work directory: task 5 from
/// turn 7, two children asked for. Returns what the model was told last.
fn forked_prime(
    warden: &mut Warden,
    socket: &mut Socket,
    more: Vec<Assistant>,
) -> (Stepped, Vec<Vec<Message>>) {
    forked_prime_waiting(warden, socket, more, quick())
}

fn forked_prime_waiting(
    warden: &mut Warden,
    socket: &mut Socket,
    more: Vec<Assistant>,
    waits: Waits,
) -> (Stepped, Vec<Vec<Message>>) {
    let mut script = vec![fork_two()];
    script.extend(more);
    let mut model = Script(script, Vec::new());
    let mut tasks = List(vec![task(5, "draft the verse two ways")], Vec::new(), None);
    let mut agent = Agent::new(warden, &mut model, &mut tasks)
        .unwrap()
        .with_forks(socket);
    agent.waits = waits;
    let stepped = agent.step().unwrap();
    drop(agent);
    (stepped, model.1)
}

#[test]
fn the_prime_forks_from_one_seal_reads_the_notes_and_promotes_without_sealing_again() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let mut warden = Warden {
        last_sealed: Some(6),
        children: vec!["flare-home-fork-11".into(), "flare-home-fork-12".into()],
        unshipped: 2,
        ..Warden::default()
    };
    // The notes come in: started, one done, both done. An earlier set's
    // note in the same slot is not this set's.
    let mut socket = Socket::new(
        "prime",
        10,
        4,
        vec![
            vec![note_of(11, "3-2", "an old draft", true)],
            vec![note_of(11, "5-7", "started: Draft it plainly", false)],
            vec![
                note_of(11, "5-7", "Plain draft, in plain.md", true),
                note_of(12, "5-7", "started: Draft it as a list", false),
            ],
            vec![
                note_of(11, "5-7", "Plain draft, in plain.md", true),
                note_of(12, "5-7", "List draft, in list.md", true),
            ],
        ],
    );
    let promote = answer(
        "",
        &[(
            "promote",
            serde_json::json!({ "child": "flare-home-fork-12" }),
        )],
    );
    let (stepped, asked) = forked_prime(&mut warden, &mut socket, vec![promote]);
    assert_eq!(stepped, Stepped::Promoted("flare-home-fork-12".into()));

    // One Seal, for the fork set; both children forked from it, after the
    // refusals while it was not shipped; none after.
    assert_eq!(warden.turns, vec![7]);
    assert_eq!(
        warden.forked,
        vec![
            ("flare-home-fork-11".to_owned(), Some(7)),
            ("flare-home-fork-12".to_owned(), Some(7))
        ]
    );
    assert_eq!(warden.promoted, vec!["flare-home-fork-12".to_owned()]);
    // Nothing was released in the turn that promoted.
    assert!(warden.declared.is_empty());
    // What the model read from fork: both notes, the slots left, the next
    // step.
    let read = tool_results(asked.last().unwrap());
    let (name, text, is_error) = read.last().unwrap();
    assert_eq!((name.as_str(), *is_error), ("fork", false), "{text}");
    assert!(
        text.contains("## flare-home-fork-11 (task 1), done:\nPlain draft, in plain.md"),
        "{text}"
    );
    assert!(
        text.contains("## flare-home-fork-12 (task 2), done:\nList draft, in list.md"),
        "{text}"
    );
    assert!(!text.contains("an old draft"), "{text}");
    assert!(
        text.contains("4 of this Vessel's 4 fork slots are left"),
        "{text}"
    );
    assert!(text.contains("call promote"), "{text}");
    // What the Seal holds: the set, the task in work, and the conversation
    // up to the fork call, which a child and a restarted prime both read.
    let set = fork_set().unwrap().unwrap();
    assert_eq!(
        set,
        ForkSet {
            set: "5-7".into(),
            task: 5,
            call: "call-fork".into(),
            prime: "flare-home".into(),
            slots: vec![11, 12],
            tasks: vec![
                "Draft it plainly, in plain.md".into(),
                "Draft it as a list, in list.md".into()
            ],
            left: 2,
            of: 4,
        }
    );
    let (state, history) = load().unwrap();
    assert_eq!(state.working.as_ref().map(|task| task.id), Some(5));
    assert!(
        matches!(history.last(), Some(Message::Assistant(asked)) if asked.calls[0].name == "fork")
    );
    let kinds: Vec<_> = warden
        .transcript
        .iter()
        .map(|e| e["step"]["kind"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        kinds.contains(&"fork".to_owned()) && kinds.contains(&"promote".to_owned()),
        "{kinds:?}"
    );
}

#[test]
fn a_child_does_its_slots_task_holds_its_actions_and_its_files_make_the_prime_go_on() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    // The prime's fork Seal.
    let mut warden = Warden {
        last_sealed: Some(6),
        children: vec!["flare-home-fork-11".into(), "flare-home-fork-12".into()],
        ..Warden::default()
    };
    let done = vec![
        note_of(11, "5-7", "one", true),
        note_of(12, "5-7", "two", true),
    ];
    let mut socket = Socket::new("prime", 10, 4, vec![done]);
    let promote = answer(
        "",
        &[(
            "promote",
            serde_json::json!({ "child": "flare-home-fork-12" }),
        )],
    );
    forked_prime(&mut warden, &mut socket, vec![promote]);

    // The child in slot 12 starts on that Seal: the second task is its.
    let mut warden = Warden {
        last_sealed: Some(7),
        hold: true,
        ..Warden::default()
    };
    let mut socket = Socket::new("fork", 12, 0, Vec::new());
    let me = socket.me.clone();
    let mut model = Script(
        vec![
            answer("", &[("fork", serde_json::json!({ "tasks": ["again"] }))]),
            answer(
                "",
                &[
                    (
                        "write",
                        serde_json::json!({ "path": "list.md", "content": "- a\n- b" }),
                    ),
                    (
                        "verse",
                        serde_json::json!({ "evidence": "decision", "text": "Drafted as a list" }),
                    ),
                ],
            ),
            answer("The list draft is in list.md.", &[]),
        ],
        Vec::new(),
    );
    let mut tasks = List(Vec::new(), Vec::new(), None);
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks)
        .unwrap()
        .with_forks(&mut socket);
    agent.waits = quick();
    assert_eq!(agent.turn(), 8, "it goes on counting from the fork's Seal");
    agent.run_child(&me).unwrap();
    drop(agent);
    assert!(tasks.1.is_empty(), "a child pulls no task");
    // It was told its task as the result of the fork call, and has no
    // fork tools: asked for one, the loop knows none.
    assert_eq!(
        tool_results(&model.1[1]).last().unwrap(),
        &("fork".to_owned(), "Tool fork not found".to_owned(), true)
    );
    let first = &model.1[0];
    let told = tool_results(first);
    assert_eq!(told.len(), 1);
    assert!(
        told[0].1.contains("flare-home-fork-12, child 2 of 2"),
        "{}",
        told[0].1
    );
    assert!(
        told[0]
            .1
            .contains("Your task:\nDraft it as a list, in list.md"),
        "{}",
        told[0].1
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("list.md")).unwrap(),
        "- a\n- b"
    );
    // Its verse and its answer are declared (and held), with the task's
    // keys; its turn is sealed with the task answered and no fork set.
    assert_eq!(warden.declared.len(), 2);
    assert_eq!(warden.declared[0].0, "island.verse");
    assert_eq!(warden.declared[0].2, action_key(5, "verse", 0));
    assert_eq!(
        said(&warden.declared[1]).text,
        "The list draft is in list.md.\n\n[From flare-home-fork-12, child 2 of 2 forked for this \
         task, promoted. 2 of this Vessel's 4 fork slots are left.]"
    );
    assert_eq!(warden.declared[1].2, action_key(5, "reply", 0));
    assert_eq!(warden.turns, vec![8]);
    assert!(fork_set().unwrap().is_none());
    let (state, _) = load().unwrap();
    assert_eq!((state.cursor, state.working.is_none()), (Some(5), true));
    assert_eq!(
        state
            .child
            .as_ref()
            .map(|c| (c.name.as_str(), c.place, c.of)),
        Some(("flare-home-fork-12", 1, 2))
    );
    // Its notes: that it started, then what it made and holds, done.
    assert_eq!(socket.posted.len(), 2);
    assert_eq!(
        socket.posted[0],
        (
            "5-7".into(),
            "started: Draft it as a list, in list.md".into(),
            Progress::Started
        )
    );
    let (set, text, done) = &socket.posted[1];
    assert_eq!((set.as_str(), *done), ("5-7", Progress::Done));
    assert!(
        text.starts_with("The list draft is in list.md.\n\n[flare-home-fork-12, child 2 of 2."),
        "{text}"
    );
    assert!(text.contains("0 other message(s), 1 verse(s)"), "{text}");

    // Promoted: the prime starts on these files. It is the prime again,
    // goes on after task 5, and the model is told what happened with its
    // next task.
    let mut warden = Warden {
        last_sealed: Some(8),
        ..Warden::default()
    };
    let mut socket = Socket::new("prime", 10, 2, Vec::new());
    let mut model = Script(vec![answer("Yes, the list.", &[])], Vec::new());
    let mut tasks = List(
        vec![
            task(5, "draft the verse two ways"),
            task(9, "which did you pick?"),
        ],
        Vec::new(),
        None,
    );
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks)
        .unwrap()
        .with_forks(&mut socket);
    agent.waits = quick();
    assert_eq!(agent.step(), Ok(Stepped::Done(9)));
    assert!(agent.state().child.is_none() && agent.state().adopted.is_none());
    drop(agent);
    assert_eq!(tasks.1, vec![Some(5)], "after the task the child answered");
    assert_eq!(
        warden.turns,
        vec![9],
        "the next task's turn holds the adoption"
    );
    let asked = model.1[0].last().unwrap();
    assert!(
        matches!(asked, Message::User { text } if text.starts_with("[Flare: on task #5 you forked 2 child(ren) and promoted flare-home-fork-12 (child 2).")
            && text.contains("2 of this Vessel's 4 fork slots are left.")
            && text.ends_with("[Task #9 from laptop]\nwhich did you pick?")),
        "{asked:?}"
    );
    let log = std::fs::read_to_string(dir.path().join(".flare/log.jsonl")).unwrap();
    assert!(log.contains("\"event\":\"promoted\""), "{log}");
}

#[test]
fn a_prime_restarted_on_its_forks_seal_waits_on_and_forks_nothing_again() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    // What the fork's Seal holds, as the run that forked left it before
    // it was killed while waiting: the set, the task in work, and the
    // conversation up to the fork call. Taso's rollback puts the files
    // back to exactly this.
    let set = ForkSet {
        set: "5-7".into(),
        task: 5,
        call: "call-fork".into(),
        prime: "flare-home".into(),
        slots: vec![11, 12],
        tasks: vec![
            "Draft it plainly, in plain.md".into(),
            "Draft it as a list, in list.md".into(),
        ],
        left: 2,
        of: 4,
    };
    std::fs::create_dir_all(dir.path().join(".flare")).unwrap();
    std::fs::write(
        dir.path().join(".flare/fork-tasks.json"),
        serde_json::to_vec(&set).unwrap(),
    )
    .unwrap();
    let history = vec![
        Message::User {
            text: "[Task #5 from laptop]\ndraft the verse two ways".into(),
        },
        Message::Assistant(fork_two()),
    ];
    save(
        &State {
            working: Some(task(5, "draft the verse two ways")),
            ..State::default()
        },
        &history,
    )
    .unwrap();

    let mut warden = Warden {
        last_sealed: Some(7),
        children: vec!["flare-home-fork-13".into()],
        ..Warden::default()
    };
    let mut socket = Socket::new(
        "prime",
        10,
        2,
        vec![vec![
            note_of(11, "5-7", "Plain draft", true),
            note_of(12, "5-7", "List draft", true),
        ]],
    );
    // This time the model reads the notes and answers without promoting.
    let mut model = Script(
        vec![answer("Neither is good; here is mine.", &[])],
        Vec::new(),
    );
    let mut tasks = List(Vec::new(), Vec::new(), None);
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks)
        .unwrap()
        .with_forks(&mut socket);
    agent.waits = quick();
    assert_eq!(agent.step(), Ok(Stepped::Done(5)));
    drop(agent);
    assert!(warden.forked.is_empty(), "no child is forked twice");
    let read = tool_results(&model.1[0]);
    assert_eq!(read.len(), 1);
    assert!(
        read[0]
            .1
            .contains("## flare-home-fork-12 (task 2), done:\nList draft"),
        "{}",
        read[0].1
    );
    // Answered without promoting: the set is given up before the reply,
    // and the turn ends as usual.
    assert!(fork_set().unwrap().is_none());
    assert_eq!(
        said(&warden.declared[0]).text,
        "Neither is good; here is mine."
    );
    assert_eq!(warden.turns, vec![8]);
    assert!(
        warden
            .transcript
            .iter()
            .any(|e| e["step"]["kind"] == "forks_given_up")
    );
}

#[test]
fn anything_but_promote_gives_the_fork_set_up_and_a_refused_promotion_is_told() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let done = vec![
        note_of(11, "5-7", "one", true),
        note_of(12, "5-7", "started", false),
    ];
    // A promotion Taso refuses leaves the set out; a child not done, or
    // not of the set, is not asked for at all. Then a say lets them go.
    let mut warden = Warden {
        last_sealed: Some(6),
        children: vec!["flare-home-fork-11".into(), "flare-home-fork-12".into()],
        no_promotion: Some("this vessel's outbox is full".into()),
        ..Warden::default()
    };
    let mut socket = Socket::new("prime", 10, 4, vec![done]);
    let promote = |child: &str| answer("", &[("promote", serde_json::json!({ "child": child }))]);
    // Turns that would end at every tool round: none does while the set
    // is out.
    let mut waits = quick();
    waits.turn = Duration::ZERO;
    let (stepped, asked) = forked_prime_waiting(
        &mut warden,
        &mut socket,
        vec![
            promote("flare-home-fork-12"),
            promote("flare-home-fork-99"),
            promote("flare-home-fork-11"),
            answer(
                "",
                &[("say", serde_json::json!({ "text": "I will do it myself." }))],
            ),
            promote("flare-home-fork-11"),
            answer("Done by hand.", &[]),
        ],
        waits,
    );
    assert_eq!(stepped, Stepped::Done(5));
    let results: Vec<_> = tool_results(asked.last().unwrap())
        .into_iter()
        .skip(1)
        .collect();
    assert!(
        results[0].2 && results[0].1.contains("has not said it is done"),
        "{:?}",
        results[0]
    );
    assert!(
        results[1].2 && results[1].1.contains("is not a child of fork set 5-7"),
        "{:?}",
        results[1]
    );
    assert!(
        results[2].2
            && results[2]
                .1
                .contains("not promoted: this vessel's outbox is full"),
        "{:?}",
        results[2]
    );
    assert_eq!(results[3], ("say".into(), "Sent (op 1).".into(), false));
    assert!(
        results[4].2 && results[4].1.contains("no fork set is out"),
        "{:?}",
        results[4]
    );
    assert!(warden.promoted.is_empty());
    assert!(fork_set().unwrap().is_none());
    // The fork's Seal; no turn through the three rounds the set was out
    // (a prime that sealed again could promote none of them); then one a
    // round once it was given up, and the task's.
    assert_eq!(warden.turns, vec![7, 8, 9, 10]);
    // The children were waited for until the one not done ran out of time.
    assert!(socket.asked > 2);
}

#[test]
fn a_fork_takes_slots_that_are_left_and_children_that_are_where_they_were_expected() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    // One slot left, two children asked for: refused before any Seal.
    let mut warden = Warden {
        last_sealed: Some(6),
        ..Warden::default()
    };
    let mut socket = Socket::new("prime", 10, 1, Vec::new());
    let (stepped, asked) =
        forked_prime(&mut warden, &mut socket, vec![answer("Then one way.", &[])]);
    assert_eq!(stepped, Stepped::Done(5));
    let read = tool_results(asked.last().unwrap());
    assert!(
        read[0].2
            && read[0]
                .1
                .contains("1 of this Vessel's 4 fork slots are left"),
        "{:?}",
        read[0]
    );
    assert_eq!(warden.turns, vec![7], "only the task's own Seal");
    assert!(warden.forked.is_empty());

    // Taso's slots are not where the socket counted them: the set is
    // given up, and said.
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let mut warden = Warden {
        last_sealed: Some(6),
        children: vec!["flare-home-fork-12".into(), "flare-home-fork-13".into()],
        ..Warden::default()
    };
    let mut socket = Socket::new("prime", 10, 4, Vec::new());
    let (_, asked) = forked_prime(&mut warden, &mut socket, vec![answer("Then one way.", &[])]);
    let read = tool_results(asked.last().unwrap());
    assert!(
        read[0].2
            && read[0].1.contains(
                "Taso made the child flare-home-fork-12 where flare-home-fork-11 was expected"
            ),
        "{:?}",
        read[0]
    );
    assert!(fork_set().unwrap().is_none());

    // No slot at Taso's: nothing forked, the set is not left out.
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let mut warden = Warden {
        last_sealed: Some(6),
        ..Warden::default()
    };
    let mut socket = Socket::new("prime", 10, 4, Vec::new());
    let (_, asked) = forked_prime(&mut warden, &mut socket, vec![answer("Then one way.", &[])]);
    let read = tool_results(asked.last().unwrap());
    assert!(
        read[0].2 && read[0].1.contains("not forked: no fork slot left"),
        "{:?}",
        read[0]
    );
    assert!(fork_set().unwrap().is_none());
}

#[test]
fn a_child_whose_slot_has_no_task_says_so_and_a_reply_waits_out_an_open_action() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let mut warden = Warden {
        last_sealed: Some(6),
        children: vec!["flare-home-fork-11".into(), "flare-home-fork-12".into()],
        ..Warden::default()
    };
    let mut socket = Socket::new(
        "prime",
        10,
        4,
        vec![vec![
            note_of(11, "5-7", "x", true),
            note_of(12, "5-7", "y", true),
        ]],
    );
    let promote = answer(
        "",
        &[(
            "promote",
            serde_json::json!({ "child": "flare-home-fork-11" }),
        )],
    );
    forked_prime(&mut warden, &mut socket, vec![promote]);

    // A child in a slot the set did not expect: it fails with a note.
    let mut warden = Warden {
        last_sealed: Some(7),
        hold: true,
        ..Warden::default()
    };
    let mut socket = Socket::new("fork", 14, 0, Vec::new());
    let me = socket.me.clone();
    let mut model = Script(Vec::new(), Vec::new());
    let mut tasks = List(Vec::new(), Vec::new(), None);
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks)
        .unwrap()
        .with_forks(&mut socket);
    agent.waits = quick();
    let failed = agent.run_child(&me).unwrap_err();
    drop(agent);
    assert!(
        failed.contains("uid slot 14 has no task in fork set 5-7"),
        "{failed}"
    );
    assert_eq!(socket.posted.len(), 1);
    assert_eq!(socket.posted[0].2, Progress::Failed);
    assert!(socket.posted[0].1.contains("failed and holds nothing"));
    assert!(warden.turns.is_empty() && warden.declared.is_empty());

    // The task's reply is tried until Taso takes it, however long an
    // earlier action's fate is open: longer than a say's patience.
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let mut warden = Warden {
        refuse: 12,
        ..Warden::default()
    };
    let mut model = Script(vec![answer("Here.", &[])], Vec::new());
    let mut tasks = List(vec![task(0, "hello")], Vec::new(), None);
    let mut agent = Agent::new(&mut warden, &mut model, &mut tasks).unwrap();
    agent.waits = quick();
    agent.waits.patience = Duration::ZERO;
    assert_eq!(agent.step(), Ok(Stepped::Done(0)));
    drop(agent);
    assert_eq!(said(&warden.declared[0]).text, "Here.");
}

#[test]
fn a_child_that_failed_is_not_waited_for_and_not_promoted() {
    let _one = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_work(dir.path());
    let mut warden = Warden {
        last_sealed: Some(6),
        children: vec!["flare-home-fork-11".into(), "flare-home-fork-12".into()],
        ..Warden::default()
    };
    let mut failed = note_of(
        12,
        "5-7",
        "This child failed and holds nothing to promote: x",
        true,
    );
    failed.failed = true;
    let mut socket = Socket::new(
        "prime",
        10,
        4,
        vec![vec![note_of(11, "5-7", "Plain draft", true), failed]],
    );
    let promote = |child: &str| answer("", &[("promote", serde_json::json!({ "child": child }))]);
    let (stepped, asked) = forked_prime(
        &mut warden,
        &mut socket,
        vec![promote("flare-home-fork-12"), promote("flare-home-fork-11")],
    );
    assert_eq!(stepped, Stepped::Promoted("flare-home-fork-11".into()));
    let read = tool_results(asked.last().unwrap());
    assert!(
        read[0]
            .1
            .contains("## flare-home-fork-12 (task 2), FAILED, nothing to promote:"),
        "{}",
        read[0].1
    );
    assert!(
        read[1].2 && read[1].1.contains("has not said it is done"),
        "{:?}",
        read[1]
    );
    assert_eq!(warden.promoted, vec!["flare-home-fork-11".to_owned()]);
    assert_eq!(
        socket.asked, 3,
        "one look at the notes, and one per promote"
    );
}
