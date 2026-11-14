//! The sender-history harness (docs/slice-2.md, "Harness and gates").
//!
//! Generated histories of two Vessels: actions declared and redone with
//! the same keys, approvals and declines in any order and number, Warden
//! restarts that mark released actions unknown, and sender passes that
//! die at every step: before the dispatch lands, after it lands but before
//! the answer, right after it (before the report), and around the report.
//! The pass is `island_sender::serve`, the code the service runs; the log
//! keeps the gate's rule (`State::dispatch_refusal`); the Warden is a
//! stand-in with Taso's rules for what this needs.
//!
//! Oracles, after every operation and from the records alone:
//!
//! 1. Every dispatch names an earlier approval of its request that no
//!    earlier dispatch used, with no decline of the request between them,
//!    and no Vessel's action key is dispatched twice.
//! 2. No action is reported applied without a dispatch of its key.
//! 3. No action is reported applied twice.
//! 4. Each decision the pass made is the one a model of the rule, written
//!    here without the sender's code, makes from the log at that moment;
//!    the gate never refuses a dispatch the sender tried.
//! 5. After any history and two clean passes, nothing is lost: no released
//!    action is unknown, every one still released is waiting for an
//!    approval, and every dispatched action is reported applied.

use std::collections::{BTreeMap, BTreeSet};

use island_client::approval::{Marks, effect_key, request_hash};
use island_core::{
    Answered, Approval, Body, Dispatch, Entry, Hash, Record, State, StreamId, Submission, VesselId,
};
use island_sender::{Action, Ended, Ledger, Planned, Released, Step, serve};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use taso_core::record::{Consequence, RetryPolicy};
use taso_field::protocol::{OpenEffect, Outcome};

const VESSELS: [VesselId; 2] = [[0xA0; 16], [0xB0; 16]];
/// Actions per Vessel; actions 0 and 3 make the same request, so one
/// approval meets two actions.
const ACTIONS: u8 = 4;
const PROVIDER: &str = "island";

fn request_bytes(action: u8) -> Vec<u8> {
    format!("request {}", action % 3).into_bytes()
}

fn request_of(action: u8) -> Hash {
    request_hash(PROVIDER, &request_bytes(action))
}

fn raw_key(vessel: usize, action: u8) -> Vec<u8> {
    vec![vessel as u8, action]
}

// ── The log ─────────────────────────────────────────────────────────

#[derive(Default)]
struct Log {
    records: Vec<Record>,
    state: State,
}

impl Log {
    fn record(seq: u64, body: Body) -> Record {
        Record {
            entry: Entry {
                seq,
                at: seq,
                policy: 0,
                prev: [0; 32],
                submission: Submission {
                    stream: StreamId::from(StreamId::APPROVALS),
                    writer: [1; 32],
                    counter: seq,
                    body,
                },
                sig_writer: [0; 64],
            },
            hash: [0; 32],
            sig_appender: [0; 64],
        }
    }

    /// Append through the gate's rule; `Err` is the refusal.
    fn append(&mut self, body: Body) -> Result<u64, String> {
        let seq = self.records.len() as u64;
        match &body {
            Body::Approval(approval) => {
                self.state.approvals.insert(
                    seq,
                    island_core::Given {
                        request: approval.request,
                        verdict: approval.verdict,
                        used_by: None,
                    },
                );
                if approval.verdict == Answered::Decline {
                    self.state.declined.insert(approval.request, seq);
                }
            }
            Body::Dispatch(d) => {
                if let Some(refusal) = self
                    .state
                    .dispatch_refusal(&d.vessel, &d.key, &d.request, d.approval)
                {
                    return Err(format!("{refusal:?}"));
                }
                if let Some(given) = self.state.approvals.get_mut(&d.approval) {
                    given.used_by = Some(seq);
                }
                self.state.dispatched.insert((d.vessel, d.key));
            }
            _ => unreachable!("only approvals and dispatches here"),
        }
        self.records.push(Self::record(seq, body));
        Ok(seq)
    }

    fn marks(&self) -> Marks {
        let mut marks = Marks::new();
        for record in &self.records {
            marks.note(record);
        }
        marks
    }

    fn answers(&self) -> impl Iterator<Item = (u64, &Approval)> {
        self.records
            .iter()
            .filter_map(|r| match &r.entry.submission.body {
                Body::Approval(a) => Some((r.entry.seq, a)),
                _ => None,
            })
    }

    fn dispatches(&self) -> impl Iterator<Item = (u64, &Dispatch)> {
        self.records
            .iter()
            .filter_map(|r| match &r.entry.submission.body {
                Body::Dispatch(d) => Some((r.entry.seq, d)),
                _ => None,
            })
    }
}

// ── The model of the rule, written without the sender's code ────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    AlreadySent(u64),
    Send(u64),
    Decline(u64),
    Lapse,
    Wait,
}

/// The decision for one action from the first `upto` records.
fn model(
    log: &Log,
    upto: usize,
    vessel: VesselId,
    key: Hash,
    request: Hash,
    unknown: bool,
) -> Model {
    let records = &log.records[..upto];
    let mut answers = Vec::new();
    let mut used = BTreeSet::new();
    for r in records {
        match &r.entry.submission.body {
            Body::Dispatch(d) if d.vessel == vessel && d.key == key => {
                return Model::AlreadySent(r.entry.seq);
            }
            Body::Dispatch(d) => {
                used.insert(d.approval);
            }
            Body::Approval(a) if a.request == request => answers.push((r.entry.seq, a.verdict)),
            _ => {}
        }
    }
    let last_decline = answers
        .iter()
        .filter(|(_, v)| *v == Answered::Decline)
        .map(|(seq, _)| *seq)
        .max();
    let usable = answers.iter().find(|(seq, v)| {
        *v == Answered::Approve && !used.contains(seq) && last_decline.is_none_or(|d| d < *seq)
    });
    match (usable, last_decline, unknown) {
        (Some((seq, _)), _, _) => Model::Send(*seq),
        (None, Some(seq), _) => Model::Decline(seq),
        (None, None, true) => Model::Lapse,
        (None, None, false) => Model::Wait,
    }
}

// ── The Warden ──────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
enum OpState {
    Open { unknown: bool },
    Applied,
    NotApplied,
}

#[derive(Clone, Debug)]
struct Op {
    vessel: usize,
    action: u8,
    state: OpState,
}

#[derive(Default)]
struct Warden {
    ops: BTreeMap<u64, Op>,
    next: u64,
}

impl Warden {
    fn effect(op: u64, o: &Op) -> OpenEffect {
        let OpState::Open { unknown } = o.state else {
            unreachable!("only open ops are listed");
        };
        OpenEffect {
            op,
            provider: PROVIDER.to_owned(),
            request: request_bytes(o.action),
            retry: RetryPolicy::ProviderDeduplicated {
                key: raw_key(o.vessel, o.action),
            },
            consequence: Consequence::Irreversible,
            authority: b"history".to_vec(),
            unknown,
        }
    }

    fn latest(&self, vessel: usize, action: u8) -> Option<&Op> {
        self.ops
            .values()
            .rev()
            .find(|o| o.vessel == vessel && o.action == action)
    }

    /// Taso's authority rule: a Vessel with an unknown barrier releases
    /// nothing new until it is settled.
    fn has_authority(&self, vessel: usize) -> bool {
        !self
            .ops
            .values()
            .any(|o| o.vessel == vessel && o.state == OpState::Open { unknown: true })
    }
}

// ── Faults and the wrappers that inject them ────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Fault {
    None,
    /// The dispatch never reaches the log; the sender dies.
    LostBeforeLanding,
    /// The dispatch lands; the answer is lost; the sender dies.
    LostAfterLanding,
    /// The sender dies right after the dispatch, before the report.
    StopAfterSend,
    /// The report never reaches the Warden; the sender dies.
    ReportLostBefore,
    /// The report is recorded; the answer is lost; the sender dies.
    ReportLostAfter,
}

struct SimLedger<'a> {
    log: &'a mut Log,
    marks: Marks,
    fault: &'a mut Fault,
}

impl Ledger<&'static str> for SimLedger<'_> {
    fn marks(&self) -> &Marks {
        &self.marks
    }

    fn dispatch(
        &mut self,
        vessel: VesselId,
        planned: &Planned,
        approval: u64,
    ) -> Result<Result<u64, String>, &'static str> {
        let body = Body::Dispatch(Dispatch {
            vessel,
            op: planned.effect.op,
            key: planned.key,
            request: planned.hash,
            approval,
            counter: 0,
        });
        match *self.fault {
            Fault::LostBeforeLanding => {
                *self.fault = Fault::None;
                return Err("lost before landing");
            }
            Fault::LostAfterLanding => {
                *self.fault = Fault::None;
                let _ = self.log.append(body);
                return Err("lost after landing");
            }
            _ => {}
        }
        let answer = self.log.append(body);
        self.marks = self.log.marks();
        Ok(answer)
    }
}

struct SimWarden<'a> {
    warden: &'a mut Warden,
    vessel: usize,
    fault: &'a mut Fault,
}

impl Released<&'static str> for SimWarden<'_> {
    fn open(&mut self) -> Result<Vec<OpenEffect>, &'static str> {
        Ok(self
            .warden
            .ops
            .iter()
            .filter(|(_, o)| o.vessel == self.vessel && matches!(o.state, OpState::Open { .. }))
            .map(|(op, o)| Warden::effect(*op, o))
            .collect())
    }

    fn report(&mut self, op: u64, result: Outcome) -> Result<Option<String>, &'static str> {
        if *self.fault == Fault::ReportLostBefore {
            *self.fault = Fault::None;
            return Err("report lost before");
        }
        let answer = match self.warden.ops.get_mut(&op) {
            Some(o) if o.vessel == self.vessel && matches!(o.state, OpState::Open { .. }) => {
                o.state = match result {
                    Outcome::Applied { .. } => OpState::Applied,
                    Outcome::NotApplied { .. } => OpState::NotApplied,
                    Outcome::Unknown => OpState::Open { unknown: true },
                };
                None
            }
            _ => Some("not a released, unsettled effect of this Vessel".to_owned()),
        };
        if *self.fault == Fault::ReportLostAfter {
            *self.fault = Fault::None;
            return Err("report lost after");
        }
        Ok(answer)
    }
}

// ── Histories ───────────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Do {
    Declare { vessel: u8, action: u8 },
    Approve { action: u8 },
    Decline { action: u8 },
    Restart { vessel: u8 },
    Pass { faults: [Fault; 2] },
}

fn fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        6 => Just(Fault::None),
        1 => Just(Fault::LostBeforeLanding),
        1 => Just(Fault::LostAfterLanding),
        1 => Just(Fault::StopAfterSend),
        1 => Just(Fault::ReportLostBefore),
        1 => Just(Fault::ReportLostAfter),
    ]
}

fn step() -> impl Strategy<Value = Do> {
    prop_oneof![
        4 => (0u8..2, 0..ACTIONS).prop_map(|(vessel, action)| Do::Declare { vessel, action }),
        3 => (0..ACTIONS).prop_map(|action| Do::Approve { action }),
        1 => (0..ACTIONS).prop_map(|action| Do::Decline { action }),
        1 => (0u8..2).prop_map(|vessel| Do::Restart { vessel }),
        5 => (fault(), fault()).prop_map(|(a, b)| Do::Pass { faults: [a, b] }),
    ]
}

fn history() -> impl Strategy<Value = Vec<Do>> {
    prop::collection::vec(step(), 1..60)
}

#[derive(Default)]
struct World {
    log: Log,
    warden: Warden,
    /// The sender process's memory: the actions the gate refused. A dead
    /// sender's successor starts with none.
    refused: BTreeSet<(VesselId, Hash)>,
    coverage: BTreeMap<String, u32>,
}

impl World {
    fn hit(&mut self, label: impl Into<String>) {
        *self.coverage.entry(label.into()).or_default() += 1;
    }

    fn apply(&mut self, what: &Do) -> Result<(), String> {
        match *what {
            Do::Declare { vessel, action } => {
                let vessel = usize::from(vessel);
                let redo = match self.warden.latest(vessel, action).map(|o| &o.state) {
                    None => false,
                    Some(OpState::NotApplied) => true,
                    // Pending or done: the agent does not declare it again.
                    Some(_) => return Ok(()),
                };
                if !self.warden.has_authority(vessel) {
                    self.hit("declare:refused-no-authority");
                    return Ok(());
                }
                if redo {
                    self.hit("declare:redone-same-key");
                }
                let op = self.warden.next;
                self.warden.next += 1;
                self.warden.ops.insert(
                    op,
                    Op {
                        vessel,
                        action,
                        state: OpState::Open { unknown: false },
                    },
                );
            }
            Do::Approve { action } | Do::Decline { action } => {
                let verdict = if matches!(what, Do::Approve { .. }) {
                    Answered::Approve
                } else {
                    Answered::Decline
                };
                self.log
                    .append(Body::Approval(Approval {
                        request: request_of(action),
                        verdict,
                        proof: None,
                    }))
                    .map_err(|why| format!("an approval was refused: {why}"))?;
            }
            Do::Restart { vessel } => {
                for o in self.warden.ops.values_mut() {
                    if o.vessel == usize::from(vessel) && matches!(o.state, OpState::Open { .. }) {
                        o.state = OpState::Open { unknown: true };
                    }
                }
            }
            Do::Pass { faults } => self.pass(faults)?,
        }
        self.check()
    }

    fn pass(&mut self, faults: [Fault; 2]) -> Result<(), String> {
        for (vessel, planned) in faults.into_iter().enumerate() {
            // Each fault goes to the side it happens on; it fires once, at
            // the first chance, and the sender dies with it.
            let on_report = matches!(planned, Fault::ReportLostBefore | Fault::ReportLostAfter);
            let mut ledger_fault = if on_report { Fault::None } else { planned };
            let mut report_fault = if on_report { planned } else { Fault::None };
            let mut steps: Vec<Step> = Vec::new();
            let marks = self.log.marks();
            let ended = serve(
                &mut SimLedger {
                    log: &mut self.log,
                    marks,
                    fault: &mut ledger_fault,
                },
                &mut SimWarden {
                    warden: &mut self.warden,
                    vessel,
                    fault: &mut report_fault,
                },
                VESSELS[vessel],
                &mut self.refused,
                planned == Fault::StopAfterSend,
                |step| steps.push(step.clone()),
            );
            let fired = match planned {
                Fault::None => false,
                Fault::StopAfterSend => ended == Ok(Ended::StoppedAfterSend),
                _ if on_report => report_fault == Fault::None,
                _ => ledger_fault == Fault::None,
            };
            if fired {
                self.hit(format!("fault:{planned:?}"));
            }
            for step in &steps {
                self.check_step(step)?;
            }
            if ended != Ok(Ended::Done) {
                // The sender died: its successor remembers nothing.
                self.refused.clear();
            }
        }
        Ok(())
    }

    /// Oracle 4 for one step: the model agrees, from the log as it was
    /// when the step was decided.
    fn check_step(&mut self, step: &Step) -> Result<(), String> {
        let effect = &step.planned.effect;
        let RetryPolicy::ProviderDeduplicated { key } = &effect.retry else {
            unreachable!("every effect here has a key");
        };
        let key = effect_key(key);
        let vessel = VESSELS[usize::from(key_vessel(effect))];
        let request = request_hash(&effect.provider, &effect.request);
        self.hit(format!("action:{}", step.action.name()));
        let (upto, got) = match &step.action {
            Action::Sent { entry, approval } => (*entry as usize, Model::Send(*approval)),
            Action::Wait => (self.log.records.len(), Model::Wait),
            Action::Lapse => (self.log.records.len(), Model::Lapse),
            Action::Decline(seq) => (self.log.records.len(), Model::Decline(*seq)),
            Action::AlreadySent(seq) => (self.log.records.len(), Model::AlreadySent(*seq)),
            Action::SendRefused(why) => {
                return Err(format!(
                    "oracle 4: the gate refused a dispatch the sender tried: {why}"
                ));
            }
            Action::HeldAfterRefusal => {
                return Err("oracle 4: an action held after a refusal".to_owned());
            }
        };
        let want = model(&self.log, upto, vessel, key, request, effect.unknown);
        if got != want {
            return Err(format!(
                "oracle 4: op {} decided {got:?}, the model says {want:?}",
                effect.op
            ));
        }
        Ok(())
    }

    /// Oracles 1 to 3, from the records alone.
    fn check(&self) -> Result<(), String> {
        let answers: BTreeMap<u64, &Approval> = self.log.answers().collect();
        let mut used = BTreeSet::new();
        let mut sent = BTreeSet::new();
        for (seq, d) in self.log.dispatches() {
            let ok = matches!(answers.get(&d.approval), Some(a) if a.request == d.request && a.verdict == Answered::Approve);
            if !ok {
                return Err(format!(
                    "oracle 1: dispatch {seq} names no approval of its request"
                ));
            }
            if !used.insert(d.approval) {
                return Err(format!("oracle 1: dispatch {seq} uses a used approval"));
            }
            if answers
                .range(d.approval + 1..seq)
                .any(|(_, a)| a.request == d.request && a.verdict == Answered::Decline)
            {
                return Err(format!("oracle 1: dispatch {seq} comes after a decline"));
            }
            if !sent.insert((d.vessel, d.key)) {
                return Err(format!("oracle 1: dispatch {seq} sends an action twice"));
            }
        }
        let mut applied = BTreeSet::new();
        for (op, o) in &self.warden.ops {
            if o.state != OpState::Applied {
                continue;
            }
            let key = (VESSELS[o.vessel], effect_key(&raw_key(o.vessel, o.action)));
            if !sent.contains(&key) {
                return Err(format!("oracle 2: op {op} applied without a dispatch"));
            }
            if !applied.insert(key) {
                return Err(format!(
                    "oracle 3: op {op} applies an action applied before"
                ));
            }
        }
        Ok(())
    }

    /// Oracle 5, after two clean passes.
    fn settle(&mut self) -> Result<(), String> {
        self.pass([Fault::None; 2])?;
        self.pass([Fault::None; 2])?;
        self.check()?;
        let sent: BTreeSet<_> = self
            .log
            .dispatches()
            .map(|(_, d)| (d.vessel, d.key))
            .collect();
        for (op, o) in &self.warden.ops {
            let vessel = VESSELS[o.vessel];
            let key = effect_key(&raw_key(o.vessel, o.action));
            match o.state {
                OpState::Open { unknown: true } => {
                    return Err(format!("oracle 5: op {op} is still unknown"));
                }
                OpState::Open { unknown: false } => {
                    let want = model(
                        &self.log,
                        self.log.records.len(),
                        vessel,
                        key,
                        request_of(o.action),
                        false,
                    );
                    if want != Model::Wait {
                        return Err(format!(
                            "oracle 5: op {op} is still released, the model says {want:?}"
                        ));
                    }
                }
                _ => {}
            }
        }
        for (vessel, key) in sent {
            let applied = self.warden.ops.values().any(|o| {
                o.state == OpState::Applied
                    && VESSELS[o.vessel] == vessel
                    && effect_key(&raw_key(o.vessel, o.action)) == key
            });
            if !applied {
                return Err("oracle 5: a dispatched action is not reported applied".to_owned());
            }
        }
        Ok(())
    }
}

fn key_vessel(effect: &OpenEffect) -> u8 {
    match &effect.retry {
        RetryPolicy::ProviderDeduplicated { key } => key[0],
        _ => unreachable!(),
    }
}

fn run(history: &[Do]) -> Result<BTreeMap<String, u32>, String> {
    let mut world = World::default();
    for (i, what) in history.iter().enumerate() {
        world
            .apply(what)
            .map_err(|why| format!("at step {i} ({what:?}): {why}"))?;
    }
    world
        .settle()
        .map_err(|why| format!("after the history: {why}"))?;
    Ok(world.coverage)
}

proptest! {
    #![proptest_config(Config { cases: 512, ..Config::default() })]

    #[test]
    fn every_history_keeps_the_rule(history in history()) {
        if let Err(why) = run(&history) {
            prop_assert!(false, "{}", why);
        }
    }
}

/// The histories must actually reach what the oracles are about.
#[test]
fn the_generator_reaches_every_outcome() {
    let mut runner = TestRunner::new_with_rng(
        Config::default(),
        TestRng::from_seed(RngAlgorithm::ChaCha, &[7; 32]),
    );
    let mut coverage: BTreeMap<String, u32> = BTreeMap::new();
    for _ in 0..400 {
        let history = history()
            .new_tree(&mut runner)
            .expect("a history")
            .current();
        for (label, n) in run(&history).expect("every history passes") {
            *coverage.entry(label).or_default() += n;
        }
    }
    println!("{coverage:#?}");
    let required = [
        "action:wait",
        "action:lapse",
        "action:decline",
        "action:already-sent",
        "action:send",
        "declare:redone-same-key",
        "declare:refused-no-authority",
        "fault:LostBeforeLanding",
        "fault:LostAfterLanding",
        "fault:StopAfterSend",
        "fault:ReportLostBefore",
        "fault:ReportLostAfter",
    ];
    let missing: Vec<_> = required
        .iter()
        .filter(|label| !coverage.contains_key(**label))
        .collect();
    assert!(missing.is_empty(), "never reached: {missing:?}");
}
