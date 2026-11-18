//! What the log amounts to, for the face (`GET /log/state`): its streams,
//! every grant, the policy and its history, the gate's refusals,
//! approvals and dispatches, and what each Vessel was given through the
//! island socket. All of it is replayed from the server's verified
//! replica; nothing in it comes from anywhere else. A Vessel is known by
//! its id, a key by its bytes: the log holds no names for either.

use std::collections::BTreeMap;

use flare_core::session::Place;
use flare_core::{content, hex, invariant, kind_name, refusal};
use island_core::{AccessKind, Answered, Body, Record, Role, StreamId, state_hash};
use island_log::Chain;
use serde::Serialize;

/// The most of any one list the state answers with: the newest.
pub const MAX_STATE_LIST: usize = 100;

/// One stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StreamLine {
    /// Its name.
    pub name: String,
    /// How many entries it holds.
    pub entries: u64,
    /// Its newest entry.
    pub last: u64,
    /// When that was ordered, in microseconds since the epoch.
    pub last_at: u64,
}

/// One grant, in force or ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GrantLine {
    /// Its id: the entry that holds it and its place there, `38.0`.
    pub id: String,
    /// The entry that holds it.
    pub seq: u64,
    /// When that was ordered, in microseconds since the epoch.
    pub at: u64,
    /// Who wrote the entry: the key, in hex.
    pub by: String,
    /// The key that holds the grant, in hex.
    pub holder: String,
    /// The streams it covers.
    pub scope: Vec<String>,
    /// The kinds it may write there.
    pub rights: Vec<&'static str>,
    /// `ordinary`, `root` or `appender`.
    pub role: &'static str,
    /// The grant it replaced by narrowing.
    pub narrows: Option<String>,
    /// The entry that ended it, if it is not in force.
    pub ended_at: Option<u64>,
    /// The grant that replaced it; `None` with `ended_at` set: revoked.
    pub ended_by: Option<String>,
}

/// One `policy` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PolicyLine {
    /// The entry.
    pub seq: u64,
    /// When, in microseconds since the epoch.
    pub at: u64,
    /// The build that judges.
    pub evaluator: String,
    /// The invariants it put in force, as the policy names them.
    pub invariants: Vec<String>,
}

/// One refusal of the gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerdictLine {
    /// The `verdict` entry.
    pub seq: u64,
    /// When, in microseconds since the epoch.
    pub at: u64,
    /// The stream the refused submission was for.
    pub stream: String,
    /// Who wrote the refused submission: the key, in hex.
    pub writer: String,
    /// Its kind.
    pub kind: &'static str,
    /// Why it was refused.
    pub reason: String,
    /// What it said, as `see` shows it.
    pub content: String,
}

/// One approval or decline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApprovalLine {
    /// The `approval` entry.
    pub seq: u64,
    /// When, in microseconds since the epoch.
    pub at: u64,
    /// Who answered: the key, in hex.
    pub by: String,
    /// The request hash it answers, in hex.
    pub request: String,
    /// `approve` or `decline`.
    pub verdict: &'static str,
    /// The dispatch that used it.
    pub used_by: Option<u64>,
}

/// One dispatch: an approved action let out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DispatchLine {
    /// The `dispatch` entry.
    pub seq: u64,
    /// When, in microseconds since the epoch.
    pub at: u64,
    /// Who let it out: the sender's key, in hex.
    pub by: String,
    /// The Vessel whose action it is, in hex.
    pub vessel: String,
    /// The action's op.
    pub op: u64,
    /// The request hash, in hex.
    pub request: String,
    /// The approval it consumed.
    pub approval: u64,
}

/// Calls and bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counted {
    /// How many requests.
    pub calls: u64,
    /// Bytes the Vessel sent.
    pub bytes_in: u64,
    /// Bytes it received.
    pub bytes_out: u64,
}

impl Counted {
    fn add(&mut self, calls: u32, bytes_in: u64, bytes_out: u64) {
        self.calls = self.calls.saturating_add(u64::from(calls));
        self.bytes_in = self.bytes_in.saturating_add(bytes_in);
        self.bytes_out = self.bytes_out.saturating_add(bytes_out);
    }
}

/// What one Vessel was given through the island socket, by the log's
/// `access` entries, and how many actions of its were let out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VesselLine {
    /// The Vessel, in hex.
    pub vessel: String,
    /// Model calls since midnight UTC.
    pub inference_today: Counted,
    /// Model calls ever.
    pub inference: Counted,
    /// Model calls ever, per provider.
    pub providers: BTreeMap<String, Counted>,
    /// Reads of the log or a repo, ever.
    pub reads: Counted,
    /// Tasks pulled, ever.
    pub tasks: Counted,
    /// Fork notes posted or read, ever.
    pub notes: Counted,
    /// Its newest `access` entry.
    pub last_access: Option<u64>,
    /// When that was ordered, in microseconds since the epoch.
    pub last_access_at: Option<u64>,
    /// How many of its actions were dispatched.
    pub dispatched: u64,
}

/// One key that wrote to the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WriterLine {
    /// The key, in hex.
    pub key: String,
    /// How many entries it wrote.
    pub entries: u64,
    /// Its newest entry.
    pub last: u64,
    /// When that was ordered, in microseconds since the epoch.
    pub last_at: u64,
    /// Its latest counter: its next submission carries the one after.
    pub counter: u64,
}

/// What `/log/state` answers with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LogState {
    /// The newest entry's position and hash; `None`: the log is empty.
    pub head: Option<Place>,
    /// How many entries the server's replica holds.
    pub entries: u64,
    /// The root key the replica is verified against, in hex.
    pub root: String,
    /// The appender's key, in hex: every entry's seal is checked
    /// against it.
    pub appender: Option<String>,
    /// The hash of the state the log replays to, in hex.
    pub state_hash: String,
    /// Why the replica could not be brought up to date, if it could not.
    pub problem: Option<String>,
    /// The server's clock, in microseconds since the epoch.
    pub now: u64,
    /// How many entries of each kind.
    pub kinds: BTreeMap<&'static str, u64>,
    /// Every stream.
    pub streams: Vec<StreamLine>,
    /// Every grant ever issued, oldest first.
    pub grants: Vec<GrantLine>,
    /// Every `policy` entry, oldest first; the last is in force.
    pub policy: Vec<PolicyLine>,
    /// How many submissions the gate has refused.
    pub refused: u64,
    /// The newest refusals, newest first.
    pub verdicts: Vec<VerdictLine>,
    /// The newest approvals and declines, newest first.
    pub approvals: Vec<ApprovalLine>,
    /// The newest dispatches, newest first.
    pub dispatches: Vec<DispatchLine>,
    /// Every Vessel the log has an `access` or `dispatch` entry of.
    pub vessels: Vec<VesselLine>,
    /// Every key that wrote, most recent first.
    pub writers: Vec<WriterLine>,
}

fn name(stream: &StreamId) -> String {
    String::from_utf8_lossy(stream.as_bytes()).into_owned()
}

/// The newest of a list that is oldest first, newest first.
fn newest<T>(mut list: Vec<T>) -> Vec<T> {
    list.reverse();
    list.truncate(MAX_STATE_LIST);
    list
}

impl LogState {
    /// The state of a verified log: its records and the chain they
    /// replay to. `now` is the server's clock, for "today".
    pub fn of(records: &[Record], chain: &Chain, problem: Option<String>, now: u64) -> Self {
        const DAY: u64 = 86_400_000_000;
        let today = now - now % DAY;
        let state = chain.state();
        let ordered = |seq: u64| {
            usize::try_from(seq)
                .ok()
                .and_then(|index| records.get(index))
                .map(|record| (record.entry.at, record.entry.submission.writer))
        };

        let mut kinds: BTreeMap<&'static str, u64> = BTreeMap::new();
        let mut policy = Vec::new();
        let mut verdicts = Vec::new();
        let mut dispatches = Vec::new();
        let mut vessels: BTreeMap<String, VesselLine> = BTreeMap::new();
        let mut writers: BTreeMap<String, WriterLine> = BTreeMap::new();
        let vessel_of = |vessels: &mut BTreeMap<String, VesselLine>, id: String| {
            vessels.entry(id.clone()).or_insert_with(|| VesselLine {
                vessel: id,
                ..VesselLine::default()
            });
        };
        for record in records {
            let entry = &record.entry;
            let submission = &entry.submission;
            *kinds.entry(kind_name(submission.kind())).or_default() += 1;
            let key = hex(&submission.writer);
            let writer = writers.entry(key.clone()).or_insert_with(|| WriterLine {
                key,
                entries: 0,
                last: entry.seq,
                last_at: entry.at,
                counter: state
                    .writers
                    .get(&submission.writer)
                    .map_or(0, |last| last.counter),
            });
            writer.entries += 1;
            writer.last = entry.seq;
            writer.last_at = entry.at;
            match &submission.body {
                Body::Policy(body) => policy.push(PolicyLine {
                    seq: entry.seq,
                    at: entry.at,
                    evaluator: body.evaluator.clone(),
                    invariants: body.invariants.iter().map(invariant).collect(),
                }),
                Body::Verdict(body) => verdicts.push(VerdictLine {
                    seq: entry.seq,
                    at: entry.at,
                    stream: name(&body.rejected.stream),
                    writer: hex(&body.rejected.writer),
                    kind: kind_name(body.rejected.kind()),
                    reason: refusal(&body.reason),
                    content: content(&body.rejected.body),
                }),
                Body::Dispatch(body) => {
                    let vessel = hex(&body.vessel);
                    vessel_of(&mut vessels, vessel.clone());
                    if let Some(line) = vessels.get_mut(&vessel) {
                        line.dispatched += 1;
                    }
                    dispatches.push(DispatchLine {
                        seq: entry.seq,
                        at: entry.at,
                        by: hex(&submission.writer),
                        vessel,
                        op: body.op,
                        request: hex(&body.request),
                        approval: body.approval,
                    });
                }
                Body::Access(body) => {
                    let vessel = hex(&body.vessel);
                    vessel_of(&mut vessels, vessel.clone());
                    let Some(line) = vessels.get_mut(&vessel) else {
                        continue;
                    };
                    line.last_access = Some(entry.seq);
                    line.last_access_at = Some(entry.at);
                    let (calls, sent, got) = (body.calls, body.bytes_in, body.bytes_out);
                    match body.what {
                        AccessKind::Inference => {
                            line.inference.add(calls, sent, got);
                            if entry.at >= today {
                                line.inference_today.add(calls, sent, got);
                            }
                            line.providers
                                .entry(body.provider.clone())
                                .or_default()
                                .add(calls, sent, got);
                        }
                        AccessKind::Read => line.reads.add(calls, sent, got),
                        AccessKind::Task => line.tasks.add(calls, sent, got),
                        AccessKind::Note => line.notes.add(calls, sent, got),
                    }
                }
                _ => {}
            }
        }

        let grants = state
            .grants
            .iter()
            .filter_map(|(id, held)| {
                let (at, by) = ordered(id.entry)?;
                Some(GrantLine {
                    id: id.to_string(),
                    seq: id.entry,
                    at,
                    by: hex(&by),
                    holder: hex(&held.grant.holder),
                    scope: held.grant.scope.iter().map(name).collect(),
                    rights: held
                        .grant
                        .rights
                        .iter()
                        .map(|kind| kind_name(*kind))
                        .collect(),
                    role: match held.role {
                        Role::Ordinary => "ordinary",
                        Role::Root => "root",
                        Role::Appender => "appender",
                    },
                    narrows: held.grant.narrows.map(|id| id.to_string()),
                    ended_at: held.ended.map(|ended| ended.at),
                    ended_by: held
                        .ended
                        .and_then(|ended| ended.by)
                        .map(|id| id.to_string()),
                })
            })
            .collect();
        let approvals = state
            .approvals
            .iter()
            .rev()
            .take(MAX_STATE_LIST)
            .filter_map(|(seq, given)| {
                let (at, by) = ordered(*seq)?;
                Some(ApprovalLine {
                    seq: *seq,
                    at,
                    by: hex(&by),
                    request: hex(&given.request),
                    verdict: match given.verdict {
                        Answered::Approve => "approve",
                        Answered::Decline => "decline",
                    },
                    used_by: given.used_by,
                })
            })
            .collect();
        let streams = state
            .streams
            .iter()
            .map(|(id, stream)| StreamLine {
                name: name(id),
                entries: stream.entries,
                last: stream.last,
                last_at: ordered(stream.last).map_or(0, |(at, _)| at),
            })
            .collect();
        let mut writers: Vec<WriterLine> = writers.into_values().collect();
        writers.sort_by_key(|writer| std::cmp::Reverse(writer.last));
        Self {
            head: chain.head().map(|head| Place {
                seq: head.seq,
                hash: hex(&head.hash),
            }),
            entries: records.len() as u64,
            root: hex(&chain.root()),
            appender: state
                .grants
                .values()
                .find(|held| held.role == Role::Appender && held.in_force())
                .map(|held| hex(&held.grant.holder)),
            state_hash: hex(&state_hash(state)),
            problem,
            now,
            kinds,
            streams,
            grants,
            policy,
            refused: verdicts.len() as u64,
            verdicts: newest(verdicts),
            approvals,
            dispatches: newest(dispatches),
            vessels: vessels.into_values().collect(),
            writers,
        }
    }
}
