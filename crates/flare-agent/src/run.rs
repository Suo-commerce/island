//! One run of the prime (docs/slice-2.md, "One run of the prime"): load
//! what the last Seal holds, pull the next task, work on it with the
//! model and the tools, reply, end the turn; again.
//!
//! What the agent keeps lives in `/work/.flare/`: the task cursor, the
//! task being worked on, and the conversation. Every start rolls `/work`
//! back to the last Seal, so a crash costs at most the turn it hit: a
//! task not finished at the last Seal is worked on again, and its replies
//! carry the same keys, so Jarmo is told once.
//!
//! Forks (docs/slice-2.md, "Forking, step by step"). The tool `fork`
//! writes the children's tasks to `/work/.flare/fork-tasks.json`, ends the
//! turn, asks Taso for one child per task from that Seal, and waits for
//! their notes. While that file is there a fork set is out, and the prime
//! ends no turn: a prime that seals again can no longer promote (Taso's
//! M7). A crash in that window rolls back to the fork's Seal, where the
//! file says what to wait for. `promote` adopts one child and ends the
//! run; anything else gives the set up. A fork child starts on the same
//! Seal ([`Agent::run_child`]), does the task its uid slot gives it, and
//! ends its turn with the task answered: promoted, its files are the
//! prime's, and the prime's next start goes on after that task.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use flare_core::session::{SayRequest, Task};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent_loop::{self, Ended, Host, Model};
use crate::field::{Declared, Field, Forked, Promoted};
use crate::island::{Forks, MAX_NOTE, Note, Progress, Tasks, Whoami};
use crate::message::{Message, window};
use crate::tools::{Spec, specs, specs_with_forks, work_path};

/// A turn ends at the next tool round after this long (docs/slice-2.md,
/// "Turns"), so a crash costs little.
pub const TURN: Duration = Duration::from_secs(120);

/// The most bytes of conversation a request carries.
pub const WINDOW: usize = 200 * 1024;

/// The most messages kept in `/work/.flare/history.json`.
pub const KEEP: usize = 400;

/// The provider of the agent's replies.
pub const SAY: &str = "flare.say";

/// What the agent is told it is when Island has no instructions for it:
/// the same text `scripts/node/install.sh` puts in
/// `/etc/island-socket/instructions.md` the first time.
pub const SYSTEM: &str = include_str!("../../../deploy/server/agent-instructions.md");

/// The stream the agent's verses go to.
pub use crate::tools::OWN_STREAM;

/// What the agent keeps between turns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The last task finished.
    pub cursor: Option<u64>,
    /// The task being worked on.
    pub working: Option<Task>,
    /// Messages said on it so far with `say`.
    pub says: u64,
    /// Verses proposed on it so far.
    #[serde(default)]
    pub verses: u64,
    /// These files are a fork child's: which one. A prime that starts on
    /// them was just promoted to them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<Child>,
    /// What the model is told with its next task: that a child was
    /// promoted, and the conversation since the fork is that child's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted: Option<String>,
}

/// The fork child a Vessel's files were made by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    /// Its name: `<prime>-fork-<slot>`.
    pub name: String,
    /// Its fork set.
    pub set: String,
    /// Its place among the set's children, from 0.
    pub place: usize,
    /// How many children the set had.
    pub of: usize,
    /// The task the set was forked on.
    pub task: u64,
}

/// A fork set, as `/work/.flare/fork-tasks.json` holds it from the Seal
/// the children are forked from: while the file is there, the set is out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkSet {
    /// Its name, which the children's notes carry: the task and the turn
    /// that was sealed for it.
    pub set: String,
    /// The task it was forked on.
    pub task: u64,
    /// The id of the `fork` tool call: its result is the notes for the
    /// prime, and its task for a child.
    pub call: String,
    /// The prime's subject: a child is named `<prime>-fork-<slot>`.
    pub prime: String,
    /// The uid slots the children take, in order: a child finds its task
    /// by its slot's place here.
    pub slots: Vec<u32>,
    /// The tasks, one per child.
    pub tasks: Vec<String>,
    /// The Vessel's fork slots left once this set's children have theirs,
    /// and how many it has in all: a promoted child's reply tells Jarmo.
    #[serde(default)]
    pub left: u64,
    /// See `left`.
    #[serde(default)]
    pub of: u64,
}

impl ForkSet {
    /// The name of the child in place `place`, as Taso names it.
    pub fn child(&self, place: usize) -> String {
        format!("{}-fork-{}", self.prime, self.slots[place])
    }

    /// The children's names, in order.
    pub fn children(&self) -> Vec<String> {
        (0..self.slots.len())
            .map(|place| self.child(place))
            .collect()
    }
}

/// The file a fork set is kept in.
pub const FORK_TASKS: &str = "fork-tasks.json";

/// The fork set that is out, if one is.
pub fn fork_set() -> Result<Option<ForkSet>, String> {
    let path = flare_file(FORK_TASKS);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn drop_fork_set() {
    let path = flare_file(FORK_TASKS);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("flare-agent: {}: {error}", path.display()),
    }
}

/// Whether the tool call `call` of the conversation's last answer has no
/// result yet.
fn unanswered(history: &[Message], call: &str) -> bool {
    let Some(asked) = history.iter().rposition(
        |message| matches!(message, Message::Assistant(answer) if answer.calls.iter().any(|c| c.id == call)),
    ) else {
        return false;
    };
    !history[asked..]
        .iter()
        .any(|message| matches!(message, Message::Tool { call_id, .. } if call_id == call))
}

/// How long the agent waits for what.
#[derive(Clone, Copy, Debug)]
pub struct Waits {
    /// For a refused `say` during a task, before the model is told.
    pub patience: Duration,
    /// For the children's notes, before `fork` returns what there is.
    pub forks: Duration,
    /// For a child's first note, before it counts as not started.
    pub unstarted: Duration,
    /// Between two looks at the notes.
    pub poll: Duration,
    /// Between two tries of something refused for now: a fork whose Seal
    /// is not shipped yet, a reply while an earlier action's fate is open.
    pub retry: Duration,
    /// How many times a fork is asked for while its Seal is not shipped.
    pub fork_tries: u32,
    /// How many times a child posts its last note before it gives up.
    pub note_tries: u32,
    /// A turn ends at the next tool round after this long ([`TURN`]),
    /// unless a fork set is out.
    pub turn: Duration,
}

impl Default for Waits {
    fn default() -> Self {
        Self {
            patience: Duration::from_secs(60),
            forks: Duration::from_secs(15 * 60),
            unstarted: Duration::from_secs(120),
            poll: Duration::from_secs(2),
            retry: Duration::from_secs(1),
            fork_tries: 30,
            note_tries: 60,
            turn: TURN,
        }
    }
}

/// What a step of the prime came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stepped {
    /// No task came within the island socket's wait.
    Idle,
    /// This task was worked on, answered and sealed.
    Done(u64),
    /// This child was promoted: Taso ends the run, and the next start is
    /// on the child's files.
    Promoted(String),
}

/// An action's key: derived from the task, the purpose and an index,
/// never random, so a turn redone after a crash declares the same keys.
pub fn action_key(task: u64, purpose: &str, index: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("flare.action-key.v1");
    hasher.update(&task.to_le_bytes());
    hasher.update(&(purpose.len() as u64).to_le_bytes());
    hasher.update(purpose.as_bytes());
    hasher.update(&index.to_le_bytes());
    *hasher.finalize().as_bytes()
}

fn flare_file(name: &str) -> PathBuf {
    work_path(&format!(".flare/{name}"))
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Add an event to `/work/.flare/log.jsonl`: what a check on the host
/// reads (the starts, the tasks, the turns). Like everything in `/work`
/// it holds what the last Seal held, and this run's events after it.
pub fn note(event: Value) {
    use std::io::Write;
    let path = flare_file("log.jsonl");
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        })
        .and_then(|mut file| writeln!(file, "{event}"));
    if let Err(error) = written {
        eprintln!("flare-agent: {}: {error}", path.display());
    }
}

/// The state and the conversation, as the last Seal holds them.
pub fn load() -> Result<(State, Vec<Message>), String> {
    let read = |name: &str| -> Result<Option<Vec<u8>>, String> {
        match std::fs::read(flare_file(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{}: {error}", flare_file(name).display())),
        }
    };
    let state = match read("state.json")? {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("state.json: {error}"))?
        }
        None => State::default(),
    };
    let history = match read("history.json")? {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("history.json: {error}"))?
        }
        None => Vec::new(),
    };
    Ok((state, history))
}

/// Keep the state and the conversation for the next Seal.
pub fn save(state: &State, history: &[Message]) -> Result<(), String> {
    let from = history.len().saturating_sub(KEEP);
    write_atomic(
        &flare_file("history.json"),
        &serde_json::to_vec(&history[from..]).map_err(|error| error.to_string())?,
    )?;
    write_atomic(
        &flare_file("state.json"),
        &serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?,
    )
}

/// The agent.
pub struct Agent<'a> {
    field: &'a mut dyn Field,
    model: &'a mut dyn Model,
    tasks: &'a mut dyn Tasks,
    state: State,
    history: Vec<Message>,
    turn: u64,
    sealed_at: Instant,
    tools: Vec<Spec>,
    forks: Option<&'a mut dyn Forks>,
    /// How long it waits for what.
    pub waits: Waits,
}

impl<'a> Agent<'a> {
    /// The agent, carrying on from the last Seal.
    pub fn new(
        field: &'a mut dyn Field,
        model: &'a mut dyn Model,
        tasks: &'a mut dyn Tasks,
    ) -> Result<Self, String> {
        let (state, history) = load()?;
        let turn = field.last_sealed_turn().map_or(1, |turn| turn + 1);
        Ok(Self {
            field,
            model,
            tasks,
            state,
            history,
            turn,
            sealed_at: Instant::now(),
            tools: specs(),
            forks: None,
            waits: Waits::default(),
        })
    }

    /// With the island socket's side of forking: the prime gets the tools
    /// `fork` and `promote`, and a fork child can post its notes.
    pub fn with_forks(mut self, forks: &'a mut dyn Forks) -> Self {
        self.forks = Some(forks);
        self.tools = specs_with_forks();
        self
    }

    /// What it keeps.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Its conversation.
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// The turn it is in.
    pub fn turn(&self) -> u64 {
        self.turn
    }

    /// One task, if one comes within the island socket's wait: worked on,
    /// answered, and sealed.
    pub fn step(&mut self) -> Result<Stepped, String> {
        self.adopted();
        let task = match self.state.working.clone() {
            Some(task) => task,
            None => match self.tasks.next(self.state.cursor)? {
                None => return Ok(Stepped::Idle),
                Some(task) => {
                    let told = format!("[Task #{} from {}]\n{}", task.id, task.client, task.text);
                    self.history.push(Message::User {
                        text: match self.state.adopted.take() {
                            Some(note) => format!("{note}\n\n{told}"),
                            None => told,
                        },
                    });
                    self.state.working = Some(task.clone());
                    self.state.says = 0;
                    self.state.verses = 0;
                    task
                }
            },
        };
        eprintln!("flare-agent: task #{} (turn {})", task.id, self.turn);
        note(json!({ "event": "task", "task": task.id, "turn": self.turn }));
        // A fork set out at the last Seal: this start goes on waiting for
        // its children, as the run that forked them would have.
        let set = match fork_set()? {
            Some(set) if set.task == task.id && unanswered(&self.history, &set.call) => Some(set),
            Some(_) => {
                drop_fork_set();
                None
            }
            None => None,
        };
        let (system, noted) = self.instructions();
        let mut host = TaskHost {
            field: &mut *self.field,
            // Reborrowed by hand: the trait object's own lifetime
            // shortens at a coercion, not inside an Option.
            forks: match &mut self.forks {
                Some(forks) => Some(&mut **forks),
                None => None,
            },
            task: &task,
            state: &mut self.state,
            turn: &mut self.turn,
            sealed_at: &mut self.sealed_at,
            system,
            waits: self.waits,
            child: false,
            promoted: None,
        };
        host.record(noted);
        if let Some(set) = set {
            eprintln!(
                "flare-agent: fork set {} is out: waiting for its notes",
                set.set
            );
            let text = host.await_notes(&set, &set.children(), None);
            let result = Message::Tool {
                call_id: set.call.clone(),
                name: "fork".to_owned(),
                text,
                is_error: false,
            };
            host.record(json!({ "kind": "tool", "message": result }));
            self.history.push(result);
        }
        let ended = agent_loop::run(&mut *self.model, &mut host, &mut self.history, &self.tools);
        let reply = match ended {
            Ended::Promoted => {
                return Ok(Stepped::Promoted(host.promoted.take().unwrap_or_default()));
            }
            Ended::Answered(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
            // Everything was said with `say` already.
            Ended::Answered(_) if host.state.says > 0 => None,
            Ended::Answered(_) => Some("(The model gave no answer.)".to_owned()),
            Ended::Failed(why) => Some(format!("I could not finish this: {why}")),
        };
        // Answered without promoting: the fork set is given up, and the
        // reply is released in a turn that promotes nothing.
        host.give_up("the task was answered without a promotion");
        if let Some(text) = reply {
            host.declare_say(&text, "reply", 0, true)?;
        }
        self.state.cursor = Some(task.id);
        self.state.working = None;
        self.state.says = 0;
        self.state.verses = 0;
        self.end_turn()?;
        Ok(Stepped::Done(task.id))
    }

    /// The instructions as Island has them now, or the agent's own, and
    /// what the transcript says of them: their digest, so each turn says
    /// which it ran on.
    fn instructions(&mut self) -> (Message, Value) {
        let (instructions, source) = match self.tasks.instructions() {
            Some(text) => (text, "island-socket"),
            None => (SYSTEM.to_owned(), "built-in"),
        };
        let digest = blake3::hash(instructions.as_bytes());
        let noted = json!({
            "kind": "instructions",
            "source": source,
            "digest": digest.to_hex().as_str(),
            "bytes": instructions.len(),
        });
        (Message::System { text: instructions }, noted)
    }

    /// The prime, started on a promoted child's files: it is the prime
    /// again, and the model is told so with its next task. No turn ends
    /// here: the next task's Seal holds it, and a start before that Seal
    /// finds the child's files again and does the same.
    fn adopted(&mut self) {
        let Some(child) = self.state.child.take() else {
            return;
        };
        let left = self
            .forks
            .as_deref_mut()
            .and_then(|forks| forks.whoami().ok())
            .map(|me| {
                format!(
                    " {} of this Vessel's {} fork slots are left.",
                    me.forks_left,
                    me.fork_slots.len()
                )
            })
            .unwrap_or_default();
        self.state.adopted = Some(format!(
            "[Flare: on task #{task} you forked {of} child(ren) and promoted {name} (child \
             {place}). You are the prime again, on that child's files: the conversation above, \
             from its fork task on, is that child's. Its answer was sent to Jarmo as the reply \
             to task #{task}; any verse it proposed waits for his approval, and until he \
             answers it, new messages and verses of yours wait too.{left}]",
            task = child.task,
            of = child.of,
            name = child.name,
            place = child.place + 1,
        ));
        drop_fork_set();
        eprintln!(
            "flare-agent: started on the files of the promoted child {} (fork set {})",
            child.name, child.set
        );
        note(json!({
            "event": "promoted", "child": child.name, "set": child.set, "task": child.task,
            "turn": self.turn, "cursor": self.state.cursor,
        }));
    }

    /// A fork child's whole run: the task its slot gives it in the fork
    /// set its files hold, worked on with every action held, the turn
    /// ended, and a note posted for the prime. `Err`: it could not, and
    /// its note says why if a note could be posted.
    pub fn run_child(&mut self, me: &Whoami) -> Result<(), String> {
        let set = fork_set()?.ok_or("these files hold no fork set: nothing to do")?;
        match self.child_task(me, &set) {
            Ok(()) => Ok(()),
            Err(why) => {
                eprintln!("flare-agent: {}: {why}", me.subject);
                let text = format!("This child failed and holds nothing to promote: {why}");
                if let Some(forks) = self.forks.as_deref_mut()
                    && let Err(lost) = forks.post_note(&set.set, &text, Progress::Failed)
                {
                    eprintln!("flare-agent: its note was not posted: {lost}");
                }
                Err(why)
            }
        }
    }

    fn child_task(&mut self, me: &Whoami, set: &ForkSet) -> Result<(), String> {
        let place = set
            .slots
            .iter()
            .position(|&slot| slot == me.slot)
            .ok_or_else(|| {
                format!(
                    "uid slot {} has no task in fork set {}, which expected its children in \
                     slots {:?}",
                    me.slot, set.set, set.slots
                )
            })?;
        let task = match self.state.working.clone() {
            Some(task) if task.id == set.task => task,
            _ => {
                return Err(format!(
                    "the fork's Seal holds no task #{} in work",
                    set.task
                ));
            }
        };
        let name = me.subject.clone();
        // What `fork` returns in a child: its task.
        if unanswered(&self.history, &set.call) {
            self.history.push(Message::Tool {
                call_id: set.call.clone(),
                name: "fork".to_owned(),
                text: format!(
                    "You are now a fork child of this Vessel: {name}, child {} of {}. You \
                     have a copy of the files and of this conversation as they were when fork \
                     was called. Nothing you do reaches the prime's files or Jarmo unless the \
                     prime promotes you: your messages and verses are held, not sent. You \
                     cannot fork.\n\nYour task:\n{}\n\nDo it now, then answer with what you \
                     made. Your answer is the note the prime reads to choose a child, and, if \
                     you are promoted, the reply Jarmo gets to task #{}.",
                    place + 1,
                    set.slots.len(),
                    set.tasks[place],
                    task.id,
                ),
                is_error: false,
            });
        }
        self.state.child = Some(Child {
            name: name.clone(),
            set: set.set.clone(),
            place,
            of: set.slots.len(),
            task: task.id,
        });
        // The task file goes before any Seal of this child, so a promoted
        // child's files carry no fork set.
        drop_fork_set();
        eprintln!(
            "flare-agent: {name}: task {} of fork set {} (turn {})",
            place + 1,
            set.set,
            self.turn
        );
        note(json!({
            "event": "child", "child": name, "set": set.set, "place": place, "turn": self.turn,
        }));
        if let Some(forks) = self.forks.as_deref_mut() {
            let first = set.tasks[place].lines().next().unwrap_or_default();
            let brief: String = first.chars().take(200).collect();
            if let Err(why) =
                forks.post_note(&set.set, &format!("started: {brief}"), Progress::Started)
            {
                eprintln!("flare-agent: the note that it started was not posted: {why}");
            }
        }
        let (system, noted) = self.instructions();
        let mut host = TaskHost {
            field: &mut *self.field,
            // Reborrowed by hand: the trait object's own lifetime
            // shortens at a coercion, not inside an Option.
            forks: match &mut self.forks {
                Some(forks) => Some(&mut **forks),
                None => None,
            },
            task: &task,
            state: &mut self.state,
            turn: &mut self.turn,
            sealed_at: &mut self.sealed_at,
            system,
            waits: self.waits,
            child: true,
            promoted: None,
        };
        host.record(noted);
        // A child has the tools every agent has: it cannot fork.
        let ended = agent_loop::run(&mut *self.model, &mut host, &mut self.history, &specs());
        let (text, failed) = match ended {
            Ended::Answered(text) if !text.trim().is_empty() => (text.trim().to_owned(), false),
            Ended::Answered(_) => ("(The model gave no answer.)".to_owned(), true),
            Ended::Failed(why) => (format!("I could not finish this: {why}"), true),
            Ended::Promoted => return Err("a fork child cannot promote".to_owned()),
        };
        // The reply is sent only if this child is promoted, and then it
        // is the agent's reply to the task: it says where it came from,
        // and what forking it cost.
        let reply = format!(
            "{text}\n\n[From {name}, child {} of {} forked for this task, promoted. {} of this \
             Vessel's {} fork slots are left.]",
            place + 1,
            set.slots.len(),
            set.left,
            set.of,
        );
        host.declare_say(&reply, "reply", 0, false)?;
        let held = format!(
            "[{name}, child {} of {}{}. Held until promoted: this answer as the reply to \
             Jarmo, {} other message(s), {} verse(s).]",
            place + 1,
            set.slots.len(),
            if failed { ", did not finish" } else { "" },
            self.state.says,
            self.state.verses,
        );
        self.state.cursor = Some(task.id);
        self.state.working = None;
        self.state.says = 0;
        self.state.verses = 0;
        self.end_turn()?;
        // The note after the Seal: a child that says it is done can be
        // promoted at once. Cut to the note's size at a character.
        let mut room = MAX_NOTE.saturating_sub(held.len() + 32);
        while !text.is_char_boundary(room.min(text.len())) {
            room -= 1;
        }
        let cut = if room < text.len() { "\n[cut]" } else { "" };
        let note_text = format!("{}{cut}\n\n{held}", &text[..room.min(text.len())]);
        let forks = self
            .forks
            .as_deref_mut()
            .ok_or("no island socket to post the note to")?;
        let mut tries = 0;
        loop {
            match forks.post_note(&set.set, &note_text, Progress::Done) {
                Ok(()) => return Ok(()),
                Err(why) if tries < self.waits.note_tries => {
                    tries += 1;
                    eprintln!("flare-agent: the note was not posted: {why}; trying again");
                    std::thread::sleep(self.waits.poll);
                }
                // Not through `run_child`'s note of failure: the work is
                // sealed and whole, only the note is missing.
                Err(why) => {
                    eprintln!("flare-agent: the note was never posted: {why}");
                    return Ok(());
                }
            }
        }
    }

    /// Save and seal.
    fn end_turn(&mut self) -> Result<(), String> {
        save(&self.state, &self.history)?;
        note(json!({ "event": "turn", "turn": self.turn, "cursor": self.state.cursor }));
        let sealed = self.field.turn_end(self.turn)?;
        eprintln!(
            "flare-agent: turn {} ended, sealed at {sealed:?}",
            self.turn
        );
        self.turn += 1;
        self.sealed_at = Instant::now();
        Ok(())
    }
}

/// The loop's host for one task.
struct TaskHost<'a> {
    field: &'a mut dyn Field,
    forks: Option<&'a mut dyn Forks>,
    task: &'a Task,
    state: &'a mut State,
    turn: &'a mut u64,
    sealed_at: &'a mut Instant,
    system: Message,
    waits: Waits,
    /// Whether this is a fork child: it cannot fork.
    child: bool,
    /// The child this run promoted: the run is over.
    promoted: Option<String>,
}

impl TaskHost<'_> {
    /// Declare `flare.say`: released, it goes to the session. While an
    /// earlier action's fate is open Taso refuses new ones. That wait is
    /// short for a reply, which needs no approval; when what is open
    /// waits for Jarmo's answer (a verse adopted in a promotion, or one
    /// that was waiting when the Warden restarted) it lasts until he
    /// answers. With `wait_out` the declaration is tried until it is
    /// taken: the task's reply, which a new start would only work out
    /// again at the model's cost. Without, the model is told after
    /// `patience`.
    fn declare_say(
        &mut self,
        text: &str,
        purpose: &str,
        index: u64,
        wait_out: bool,
    ) -> Result<String, String> {
        let request = serde_json::to_vec(&SayRequest {
            task: self.task.id,
            text: text.to_owned(),
        })
        .map_err(|error| error.to_string())?;
        let key = action_key(self.task.id, purpose, index);
        let started = Instant::now();
        let mut told = None;
        loop {
            match self.field.declare(SAY, &request, &key)? {
                Declared::Released(op) => return Ok(format!("Sent (op {op}).")),
                Declared::Held(op) => {
                    return Ok(format!(
                        "Held (op {op}): a fork's message waits for its promotion."
                    ));
                }
                Declared::Refused(why) if wait_out || started.elapsed() < self.waits.patience => {
                    // Said at once, then once a minute: the wait can be
                    // Jarmo's to end.
                    if told.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(60)) {
                        eprintln!(
                            "flare-agent: say refused for now ({} s): {why}",
                            started.elapsed().as_secs()
                        );
                        told = Some(Instant::now());
                    }
                    // Taso records nothing for a refusal, so asking
                    // again costs little; less often once the wait is long.
                    let long = started.elapsed() >= Duration::from_secs(60);
                    std::thread::sleep(self.waits.retry * if long { 5 } else { 1 });
                }
                Declared::Refused(why) => {
                    return Err(format!(
                        "not sent: {why}. An earlier action still waits for its answer \
                         (a verse for Jarmo's approval, for one); until then nothing new is \
                         let out."
                    ));
                }
            }
        }
    }

    /// Give the fork set up, if one is out: its file goes, and the next
    /// Seal makes that final (a prime that sealed again cannot promote).
    fn give_up(&mut self, why: &str) {
        if self.promoted.is_some() {
            return;
        }
        if let Ok(Some(set)) = fork_set() {
            drop_fork_set();
            eprintln!("flare-agent: fork set {} given up: {why}", set.set);
            self.record(json!({ "kind": "forks_given_up", "set": set.set, "why": why }));
        }
    }

    /// Ask Taso for one child from the newest Seal. Taso ships a Seal
    /// right after making it and refuses a fork of one not shipped yet:
    /// that refusal is tried again.
    fn fork_one(&mut self) -> Result<String, String> {
        let mut tries = 0;
        loop {
            match self.field.fork()? {
                Forked::Child { name, seal } => {
                    eprintln!("flare-agent: forked {name} from the Seal at lsn {seal}");
                    return Ok(name);
                }
                Forked::Refused(why)
                    if why.contains("not shipped") && tries < self.waits.fork_tries =>
                {
                    tries += 1;
                    std::thread::sleep(self.waits.retry);
                }
                Forked::Refused(why) => return Err(why),
            }
        }
    }

    /// The fork slots left, in words, if the island socket says.
    fn slots_left(&mut self) -> String {
        match self.forks.as_deref_mut().map(|forks| forks.whoami()) {
            Some(Ok(me)) => {
                let warn = if me.forks_left <= 2 {
                    " Only that many: tell Jarmo, who can give this Vessel more (a config \
                     change)."
                } else {
                    ""
                };
                format!(
                    "{} of this Vessel's {} fork slots are left, for its whole life.{warn}",
                    me.forks_left,
                    me.fork_slots.len()
                )
            }
            _ => "How many fork slots are left is not known now.".to_owned(),
        }
    }

    /// Wait for the notes of `children` in `set`, and say what they are:
    /// the result of `fork`. It returns when every child is done, when
    /// the ones not done never started, or after the longest wait.
    fn await_notes(
        &mut self,
        set: &ForkSet,
        children: &[String],
        unforked: Option<&str>,
    ) -> String {
        let started = Instant::now();
        let mut notes: Vec<Option<Note>> = vec![None; children.len()];
        loop {
            match self.forks.as_deref_mut().map(|forks| forks.notes()) {
                Some(Ok(all)) => {
                    for (child, note) in children.iter().zip(notes.iter_mut()) {
                        *note = all
                            .iter()
                            .find(|n| &n.child == child && n.set == set.set)
                            .cloned();
                    }
                }
                Some(Err(why)) => eprintln!("flare-agent: the notes: {why}"),
                None => break,
            }
            let waited = started.elapsed();
            let waiting = notes.iter().any(|note| match note {
                Some(note) => !note.done,
                None => waited < self.waits.unstarted,
            });
            if !waiting || waited >= self.waits.forks {
                break;
            }
            std::thread::sleep(self.waits.poll);
        }
        let mut text = format!(
            "Forked {} child(ren) from this turn's Seal (fork set {}). Their notes:\n",
            children.len(),
            set.set
        );
        let mut done = 0;
        for (place, (child, note)) in children.iter().zip(&notes).enumerate() {
            text.push_str(&match note {
                Some(note) if note.failed => format!(
                    "\n## {child} (task {}), FAILED, nothing to promote:\n{}\n",
                    place + 1,
                    note.text
                ),
                Some(note) if note.done => {
                    done += 1;
                    format!("\n## {child} (task {}), done:\n{}\n", place + 1, note.text)
                }
                Some(note) => format!(
                    "\n## {child} (task {}), NOT done after {} s; its last word:\n{}\n",
                    place + 1,
                    started.elapsed().as_secs(),
                    note.text
                ),
                None => format!(
                    "\n## {child} (task {}): no note. It did not start, or failed at once.\n",
                    place + 1
                ),
            });
        }
        if let Some(why) = unforked {
            text.push_str(&format!(
                "\nThe other {} task(s) got no child: {why}\n",
                set.slots.len() - children.len()
            ));
        }
        text.push('\n');
        text.push_str(&self.slots_left());
        text.push_str(if done > 0 {
            "\n\nNext: call promote with the name of one child that is done, to adopt its \
             files and its held actions (you are then started again on its files, and its \
             answer is sent to Jarmo as the reply to this task). Or carry on without \
             promoting: any other tool call, or your answer, gives these children up for good."
        } else {
            "\n\nNo child is done, so none can be promoted. Carry on yourself: your next \
             tool call or your answer gives these children up."
        });
        self.record(json!({
            "kind": "fork_notes", "set": set.set, "children": children,
            "done": notes.iter().map(|n| n.as_ref().is_some_and(|n| n.done)).collect::<Vec<_>>(),
            "seconds": started.elapsed().as_secs(),
        }));
        text
    }
}

impl Host for TaskHost<'_> {
    fn window(&self, history: &[Message]) -> Vec<Message> {
        window(&self.system, history, WINDOW)
    }

    fn say(&mut self, text: &str) -> Result<String, String> {
        let index = self.state.says;
        self.state.says += 1;
        self.declare_say(text, "say", index, false)
    }

    fn verse(&mut self, verse: island_core::Verse) -> Result<String, String> {
        let index = self.state.verses;
        self.state.verses += 1;
        let request = island_client::actions::verse_request(OWN_STREAM, &verse);
        let key = action_key(self.task.id, "verse", index);
        match self
            .field
            .declare(island_client::actions::VERSE, &request, &key)?
        {
            Declared::Released(op) => Ok(format!(
                "Proposed (op {op}): it waits for Jarmo's approval in Flare. Tell him in your \
                 answer what it says and why."
            )),
            Declared::Held(op) => Ok(format!(
                "Held (op {op}): a fork's verse waits for its promotion."
            )),
            Declared::Refused(why) => Err(format!("not proposed: {why}")),
        }
    }

    fn record(&mut self, entry: Value) {
        // After a promotion the Vessel takes no more records: the run ends.
        if self.promoted.is_some() {
            return;
        }
        let mut entry = json!({ "turn": *self.turn, "task": self.task.id, "step": entry });
        let mut bytes = serde_json::to_vec(&entry).unwrap_or_default();
        // A transcript entry stays small; the conversation is in /work.
        if bytes.len() > 1024 * 1024 {
            entry["step"]["message"] = json!(format!("({} bytes, not kept here)", bytes.len()));
            bytes = serde_json::to_vec(&entry).unwrap_or_default();
        }
        if let Err(why) = self.field.transcript(&bytes) {
            eprintln!("flare-agent: transcript: {why}");
        }
    }

    fn round(&mut self, history: &[Message]) -> Result<(), String> {
        // While a fork set is out the prime ends no turn: one more Seal
        // and it could not promote any of the children (Taso's M7).
        if self.sealed_at.elapsed() < self.waits.turn || fork_set().is_ok_and(|set| set.is_some()) {
            return Ok(());
        }
        save(self.state, history)?;
        note(
            json!({ "event": "turn", "turn": *self.turn, "cursor": self.state.cursor, "mid_task": self.task.id }),
        );
        let sealed = self.field.turn_end(*self.turn)?;
        eprintln!(
            "flare-agent: turn {} ended mid-task, sealed at {sealed:?}",
            *self.turn
        );
        *self.turn += 1;
        *self.sealed_at = Instant::now();
        Ok(())
    }

    fn begin(&mut self, tool: &str) {
        // After `fork` returned, the next step is `promote` or it is not:
        // anything else lets the children go.
        if tool != "promote" {
            self.give_up(&format!("the model went on with {tool}"));
        }
    }

    fn fork(
        &mut self,
        tasks: Vec<String>,
        call: &str,
        history: &[Message],
    ) -> Result<String, String> {
        if self.child {
            return Err("a fork child cannot fork".to_owned());
        }
        let Some(forks) = self.forks.as_deref_mut() else {
            return Err("forking is not set up for this agent".to_owned());
        };
        let me = forks.whoami().map_err(|why| {
            format!("not forked: the island socket did not say which fork slots are left: {why}")
        })?;
        let free = me.free_slots();
        if tasks.len() > free.len() {
            return Err(format!(
                "not forked: {} task(s), but {} of this Vessel's {} fork slots are left. A \
                 slot is used once and never comes back; Jarmo can give the Vessel more (a \
                 config change). Fork fewer children, or do the work yourself.",
                tasks.len(),
                free.len(),
                me.fork_slots.len()
            ));
        }
        let set = ForkSet {
            set: format!("{}-{}", self.task.id, *self.turn),
            task: self.task.id,
            call: call.to_owned(),
            prime: me.subject.clone(),
            slots: free[..tasks.len()].to_vec(),
            left: (free.len() - tasks.len()) as u64,
            of: me.fork_slots.len() as u64,
            tasks,
        };
        // The tasks and the conversation go into the Seal the children
        // are forked from.
        write_atomic(
            &flare_file(FORK_TASKS),
            &serde_json::to_vec_pretty(&set).map_err(|error| error.to_string())?,
        )?;
        let sealed = save(self.state, history).and_then(|()| {
            note(json!({
                "event": "turn", "turn": *self.turn, "cursor": self.state.cursor,
                "mid_task": self.task.id, "fork_set": set.set,
            }));
            self.field.turn_end(*self.turn)
        });
        let sealed = match sealed {
            Ok(sealed) => sealed,
            Err(why) => {
                drop_fork_set();
                return Err(format!("not forked: the turn could not be ended: {why}"));
            }
        };
        eprintln!(
            "flare-agent: turn {} ended for fork set {}, sealed at {sealed:?}",
            *self.turn, set.set
        );
        *self.turn += 1;
        *self.sealed_at = Instant::now();
        self.record(json!({
            "kind": "fork", "set": set.set, "slots": set.slots, "sealed": sealed,
            "tasks": set.tasks.len(),
        }));
        // One child per task, each from that same Seal.
        let mut children = Vec::new();
        let mut unforked = None;
        for place in 0..set.slots.len() {
            match self.fork_one() {
                Ok(name) if name == set.child(place) => children.push(name),
                Ok(name) => {
                    // The slots were not as the island socket counted
                    // them: the children cannot find their tasks. They
                    // tell the socket who they are, and it counts anew.
                    drop_fork_set();
                    std::thread::sleep(self.waits.poll);
                    return Err(format!(
                        "forking went wrong: Taso made the child {name} where {} was \
                         expected, so the children cannot find their tasks, and the fork set \
                         is given up. {} slot(s) were used. The slots are counted anew now: \
                         call fork again if it is still worth it.",
                        set.child(place),
                        children.len() + 1,
                    ));
                }
                Err(why) => {
                    unforked = Some(why);
                    break;
                }
            }
        }
        if children.is_empty() {
            drop_fork_set();
            return Err(format!(
                "not forked: {}",
                unforked.unwrap_or_else(|| "no child was made".to_owned())
            ));
        }
        Ok(self.await_notes(&set, &children, unforked.as_deref()))
    }

    fn promote(&mut self, child: &str) -> Result<String, String> {
        let set = fork_set()?.ok_or(
            "no fork set is out: fork first, and promote one of its children before any \
             other tool call",
        )?;
        if !set.children().iter().any(|name| name == child) {
            return Err(format!(
                "{child:?} is not a child of fork set {}: they are {}",
                set.set,
                set.children().join(", ")
            ));
        }
        let forks = self
            .forks
            .as_deref_mut()
            .ok_or("forking is not set up for this agent")?;
        let done = forks
            .notes()
            .map_err(|why| format!("not promoted: the notes could not be read: {why}"))?
            .iter()
            .any(|note| note.child == child && note.set == set.set && note.done && !note.failed);
        if !done {
            return Err(format!(
                "not promoted: {child} has not said it is done, and only a finished child is \
                 promoted"
            ));
        }
        self.record(json!({ "kind": "promote", "set": set.set, "child": child }));
        match self.field.promote(child)? {
            Promoted::Adopted { seal, ops } => {
                eprintln!(
                    "flare-agent: promoted {child} at its Seal (lsn {seal}): {} action(s) adopted; \
                     the run ends here",
                    ops.len()
                );
                self.promoted = Some(child.to_owned());
                Ok(format!(
                    "Promoted {child}: {} held action(s) adopted. This run ends now.",
                    ops.len()
                ))
            }
            // Recorded and not finished: Taso ends the run, and finishes
            // the promotion at the next start.
            Promoted::Refused(why) if why.contains("is recorded") => {
                eprintln!("flare-agent: the promotion of {child} is recorded: {why}");
                self.promoted = Some(child.to_owned());
                Ok(format!("The promotion of {child} is recorded: {why}"))
            }
            Promoted::Refused(why) => Err(format!("not promoted: {why}")),
        }
    }

    fn over(&self) -> bool {
        self.promoted.is_some()
    }
}
