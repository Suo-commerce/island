//! `island-sender`: Taso's sender for one Vessel (S2 plan, decision D2).
//!
//! Taso releases a dispatching Vessel's effects on a sender socket that
//! only root can open, `<run_dir>/senders/<vessel-id>.sock`. This program
//! holds that socket for the broker: it lists the released effects,
//! decides each one from the approvals in the verified Island log
//! (island-client's [`approval`](island_client::approval) module), and
//! reports the outcome back to Taso. The broker stays unprivileged and
//! the decision stays in the log; this program only carries.
//!
//! What sending means so far: the `dispatch` entry this program appends
//! to the log, naming the approval it uses. The gate accepts it only by
//! the approval rule, and only once per action key, which is also what
//! makes a send happen once. Real providers (a verse from a Vessel, a
//! mail) come in later steps of slice 2 (docs/slice-2.md).

#![warn(missing_docs)]

use std::collections::BTreeSet;
use std::io::{Read, Write};

use island_client::approval::{self, Decision, Marks};
use island_core::{Hash, VesselId};
use taso_core::record::RetryPolicy;
use taso_field::protocol::{
    OpenEffect, Outcome, ProtocolError, Reply, Request, VERSION, recv_reply, send_request,
};

/// The reason reported for an effect the lapse rule declines.
pub const LAPSED: &str = "lapsed: Warden restarted before approval";

/// A conversation on a Vessel's sender socket.
pub struct SenderConn<S> {
    stream: S,
    /// The last Seal's turn, from the Warden's Hello.
    pub last_sealed_turn: Option<u64>,
}

/// Something the Warden answered that a sender does not expect.
#[derive(Debug)]
pub enum ConnError {
    /// The protocol broke, or the socket closed.
    Protocol(ProtocolError),
    /// The Warden closed the conversation.
    Closed,
    /// An answer of the wrong kind, or a refusal.
    Unexpected(String),
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(error) => write!(f, "sender socket: {error}"),
            Self::Closed => write!(f, "sender socket: the Warden closed it"),
            Self::Unexpected(what) => write!(f, "sender socket: {what}"),
        }
    }
}

impl std::error::Error for ConnError {}

impl From<ProtocolError> for ConnError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl<S: Read + Write> SenderConn<S> {
    /// Say Hello on `stream`.
    pub fn hello(stream: S) -> Result<Self, ConnError> {
        let mut conn = Self {
            stream,
            last_sealed_turn: None,
        };
        match conn.ask(&Request::Hello { version: VERSION })? {
            Reply::Hello {
                last_sealed_turn, ..
            } => {
                conn.last_sealed_turn = last_sealed_turn;
                Ok(conn)
            }
            other => Err(ConnError::Unexpected(format!(
                "Hello answered with {other:?}"
            ))),
        }
    }

    fn ask(&mut self, request: &Request) -> Result<Reply, ConnError> {
        send_request(&mut self.stream, request)?;
        match recv_reply(&mut self.stream)? {
            None => Err(ConnError::Closed),
            Some(Reply::Error { code, message }) => Err(ConnError::Unexpected(format!(
                "refused ({code:?}): {message}"
            ))),
            Some(reply) => Ok(reply),
        }
    }

    /// The released, unsettled effects, in op order.
    pub fn open(&mut self) -> Result<Vec<OpenEffect>, ConnError> {
        match self.ask(&Request::Open)? {
            Reply::Open { effects } => Ok(effects),
            other => Err(ConnError::Unexpected(format!(
                "Open answered with {other:?}"
            ))),
        }
    }

    /// Report an outcome. `Ok(None)`: recorded; `Ok(Some(why))`: Taso
    /// refused the report (not a released, unsettled effect of this
    /// Vessel) and recorded nothing.
    pub fn report(&mut self, op: u64, result: Outcome) -> Result<Option<String>, ConnError> {
        match self.ask(&Request::Outcome { op, result })? {
            Reply::Outcome { refused, .. } => Ok(refused),
            other => Err(ConnError::Unexpected(format!(
                "Outcome answered with {other:?}"
            ))),
        }
    }
}

/// One released effect and what to do with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// The effect, as Taso listed it.
    pub effect: OpenEffect,
    /// Its request hash: what an approval names.
    pub hash: Hash,
    /// Its action key: what a dispatch is unique by.
    pub key: Hash,
    /// The decision.
    pub decision: Decision,
}

/// The decision for one effect of `vessel`, from the marks so far.
pub fn plan(marks: &Marks, vessel: &VesselId, effect: OpenEffect) -> Planned {
    let hash = approval::request_hash(&effect.provider, &effect.request);
    let key = action_key(&effect);
    let decision = approval::decide(marks, vessel, &key, &hash, effect.unknown);
    Planned {
        effect,
        hash,
        key,
        decision,
    }
}

/// An effect's action key: its idempotency key when the provider
/// deduplicates by one, and otherwise its op, which Taso never reuses
/// within a Vessel. The same across a Warden restart either way.
pub fn action_key(effect: &OpenEffect) -> Hash {
    match &effect.retry {
        RetryPolicy::ProviderDeduplicated { key } => approval::effect_key(key),
        RetryPolicy::Queryable { .. } | RetryPolicy::UnsafeToRetry => {
            approval::effect_key(format!("taso-op:{}", effect.op).as_bytes())
        }
    }
}

/// The outcome to report for a decision, once any send is done.
/// `sent_entry` is the entry of the `dispatch` for [`Decision::Send`].
pub fn outcome(decision: Decision, sent_entry: Option<u64>) -> Option<Outcome> {
    match decision {
        Decision::Wait => None,
        Decision::Lapse => Some(Outcome::NotApplied {
            reason: LAPSED.to_owned(),
        }),
        Decision::Decline(seq) => Some(Outcome::NotApplied {
            reason: format!("declined at Island entry {seq}"),
        }),
        Decision::AlreadySent(sent) => Some(applied(sent.entry)),
        Decision::Send(_) => sent_entry.map(applied),
    }
}

fn applied(entry: u64) -> Outcome {
    Outcome::Applied {
        receipt: format!("island entry {entry}").into_bytes(),
    }
}

/// The sender sockets in a Warden's senders directory, in name order: the
/// entries named `<32 hex digits>.sock`, with the Vessel id each carries.
/// A directory that is not there (no Vessel started yet) has none.
pub fn sockets_in(dir: &std::path::Path) -> Vec<(std::path::PathBuf, VesselId)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            vessel_of(&path).map(|vessel| (path, vessel))
        })
        .collect();
    out.sort();
    out
}

/// The log side of a pass: the approvals and dispatches read so far, and
/// a way to append a dispatch. `island-sender` reads and appends through
/// the broker; the sender-history harness through a log of its own.
pub trait Ledger<E> {
    /// The approvals and dispatches in the log, as of the last read.
    fn marks(&self) -> &Marks;
    /// Append the dispatch of `planned` under `approval`, then read the log
    /// again. `Ok(Ok(seq))`: accepted at `seq`; `Ok(Err(why))`: the gate
    /// refused it (a verdict); `Err`: not known whether it landed.
    fn dispatch(
        &mut self,
        vessel: VesselId,
        planned: &Planned,
        approval: u64,
    ) -> Result<Result<u64, String>, E>;
}

/// The Warden side of a pass: one Vessel's sender socket.
pub trait Released<E> {
    /// The released, unsettled effects, in op order.
    fn open(&mut self) -> Result<Vec<OpenEffect>, E>;
    /// Report an outcome: `Ok(None)` recorded, `Ok(Some(why))` refused.
    fn report(&mut self, op: u64, result: Outcome) -> Result<Option<String>, E>;
}

impl<S: Read + Write> Released<String> for SenderConn<S> {
    fn open(&mut self) -> Result<Vec<OpenEffect>, String> {
        SenderConn::open(self).map_err(|error| error.to_string())
    }

    fn report(&mut self, op: u64, result: Outcome) -> Result<Option<String>, String> {
        SenderConn::report(self, op, result).map_err(|error| error.to_string())
    }
}

/// What a pass did with one effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Not approved: left waiting.
    Wait,
    /// Lost in a Warden restart and never approved: reported not applied.
    Lapse,
    /// Declined at this entry: reported not applied.
    Decline(u64),
    /// Dispatched before, at this entry: reported applied.
    AlreadySent(u64),
    /// Dispatched now, at this entry, under the approval named.
    Sent {
        /// The dispatch.
        entry: u64,
        /// The approval it used.
        approval: u64,
    },
    /// The gate refused the dispatch: nothing reported, not tried again by
    /// this process.
    SendRefused(String),
    /// Refused earlier in this process: left as it is.
    HeldAfterRefusal,
}

impl Action {
    /// Its name in the sender's output.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Lapse => "lapse",
            Self::Decline(_) => "decline",
            Self::AlreadySent(_) => "already-sent",
            Self::Sent { .. } => "send",
            Self::SendRefused(_) => "send-refused",
            Self::HeldAfterRefusal => "held-after-refusal",
        }
    }
}

/// One effect of a pass: what was decided, what was done, how the report
/// went (`None`: nothing reported; `Some(None)`: recorded; `Some(Some(why))`:
/// refused).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The effect and its decision.
    pub planned: Planned,
    /// What was done.
    pub action: Action,
    /// The report.
    pub reported: Option<Option<String>>,
}

/// How a pass ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// Every listed effect was handled.
    Done,
    /// Stopped right after a dispatch, before its report (fault injection).
    StoppedAfterSend,
}

/// One pass over one Vessel's released effects: decide each from the
/// ledger's marks, dispatch what is approved, report what is settled.
/// `refused` holds the actions whose dispatch the gate refused in this
/// process; `each` sees every step as it ends. With `stop_after_send` the
/// pass ends right after the first dispatch, before its report.
pub fn serve<E>(
    ledger: &mut impl Ledger<E>,
    warden: &mut impl Released<E>,
    vessel: VesselId,
    refused: &mut BTreeSet<(VesselId, Hash)>,
    stop_after_send: bool,
    mut each: impl FnMut(&Step),
) -> Result<Ended, E> {
    for effect in warden.open()? {
        let planned = plan(ledger.marks(), &vessel, effect);
        let (action, sent_entry) = match planned.decision {
            Decision::Wait => (Action::Wait, None),
            Decision::Lapse => (Action::Lapse, None),
            Decision::Decline(seq) => (Action::Decline(seq), None),
            Decision::AlreadySent(sent) => (Action::AlreadySent(sent.entry), None),
            Decision::Send(_) if refused.contains(&(vessel, planned.key)) => {
                (Action::HeldAfterRefusal, None)
            }
            Decision::Send(approval) => match ledger.dispatch(vessel, &planned, approval)? {
                Ok(entry) => (Action::Sent { entry, approval }, Some(entry)),
                Err(why) => {
                    refused.insert((vessel, planned.key));
                    (Action::SendRefused(why), None)
                }
            },
        };
        let mut step = Step {
            planned,
            action,
            reported: None,
        };
        if stop_after_send && sent_entry.is_some() {
            each(&step);
            return Ok(Ended::StoppedAfterSend);
        }
        let held = matches!(
            step.action,
            Action::SendRefused(_) | Action::HeldAfterRefusal
        );
        if let Some(result) = outcome(step.planned.decision, sent_entry).filter(|_| !held) {
            step.reported = Some(warden.report(step.planned.effect.op, result)?);
        }
        each(&step);
    }
    Ok(Ended::Done)
}

/// The Vessel id a sender socket's name carries: `<32 hex digits>.sock`.
pub fn vessel_of(socket: &std::path::Path) -> Option<VesselId> {
    let name = socket.file_name()?.to_str()?;
    approval::unhex(name.strip_suffix(".sock")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use taso_core::record::Consequence;
    use taso_field::protocol::{recv_request, send_reply};

    fn effect(op: u64, request: &[u8], unknown: bool) -> OpenEffect {
        OpenEffect {
            op,
            provider: "island".to_owned(),
            request: request.to_vec(),
            retry: RetryPolicy::ProviderDeduplicated {
                key: vec![op as u8],
            },
            consequence: Consequence::Irreversible,
            authority: b"s2".to_vec(),
            unknown,
        }
    }

    const V: VesselId = [0xAB; 16];

    #[test]
    fn the_hash_is_the_approval_modules() {
        let p = plan(&Marks::default(), &V, effect(3, b"req", false));
        assert_eq!(p.hash, approval::request_hash("island", b"req"));
        assert_eq!(p.key, approval::effect_key(&[3]));
        assert_eq!(p.decision, Decision::Wait);
        assert_eq!(
            plan(&Marks::default(), &V, effect(3, b"req", true)).decision,
            Decision::Lapse
        );
    }

    #[test]
    fn an_effect_without_a_key_is_keyed_by_its_op() {
        let mut e = effect(9, b"req", false);
        e.retry = RetryPolicy::UnsafeToRetry;
        assert_eq!(action_key(&e), approval::effect_key(b"taso-op:9"));
        e.retry = RetryPolicy::Queryable { probe: vec![1] };
        assert_eq!(action_key(&e), approval::effect_key(b"taso-op:9"));
    }

    #[test]
    fn outcomes_follow_decisions() {
        assert_eq!(outcome(Decision::Wait, None), None);
        assert_eq!(
            outcome(Decision::Lapse, None),
            Some(Outcome::NotApplied {
                reason: LAPSED.to_owned()
            })
        );
        assert_eq!(
            outcome(Decision::Send(9), None),
            None,
            "no report before the send is in the log"
        );
        assert_eq!(
            outcome(Decision::Send(9), Some(12)),
            Some(Outcome::Applied {
                receipt: b"island entry 12".to_vec()
            })
        );
    }

    #[test]
    fn vessel_from_socket_name() {
        let name = format!("/run/taso/run/senders/{}.sock", "ab".repeat(16));
        assert_eq!(vessel_of(std::path::Path::new(&name)), Some(V));
        assert_eq!(
            vessel_of(std::path::Path::new("/run/taso/run/senders/ab12.sock")),
            None
        );
        assert_eq!(vessel_of(std::path::Path::new("/run/x")), None);
    }

    #[test]
    fn the_senders_directory_lists_only_vessel_sockets() {
        let dir = std::env::temp_dir().join(format!("island-sender-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            format!("{}.sock", "cd".repeat(16)),
            format!("{}.sock", "ab".repeat(16)),
            "ab12.sock".to_owned(),
            format!("{}.lock", "ab".repeat(16)),
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let found: Vec<VesselId> = sockets_in(&dir).into_iter().map(|(_, v)| v).collect();
        assert_eq!(found, vec![[0xAB; 16], [0xCD; 16]]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(sockets_in(&dir).is_empty());
    }

    /// The conversation against a stand-in Warden on a socket pair.
    #[tokio::test]
    async fn conversation() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let warden = tokio::task::spawn_blocking(move || {
            let mut s = theirs;
            let mut seen = Vec::new();
            while let Some(request) = recv_request(&mut s).unwrap() {
                let reply = match &request {
                    Request::Hello { .. } => Reply::Hello {
                        version: VERSION,
                        last_sealed_turn: Some(2),
                    },
                    Request::Open => Reply::Open {
                        effects: vec![effect(1, b"a", true)],
                    },
                    Request::Outcome { op, .. } if *op == 1 => Reply::Outcome {
                        op: 1,
                        refused: None,
                    },
                    Request::Outcome { op, .. } => Reply::Outcome {
                        op: *op,
                        refused: Some("not released".to_owned()),
                    },
                    other => panic!("unexpected {other:?}"),
                };
                seen.push(request);
                send_reply(&mut s, &reply).unwrap();
            }
            seen
        });
        let mut conn = SenderConn::hello(ours).unwrap();
        assert_eq!(conn.last_sealed_turn, Some(2));
        let open = conn.open().unwrap();
        assert_eq!(open, vec![effect(1, b"a", true)]);
        let lapse = outcome(Decision::Lapse, None).unwrap();
        assert_eq!(conn.report(1, lapse.clone()).unwrap(), None);
        assert_eq!(
            conn.report(7, lapse).unwrap().as_deref(),
            Some("not released")
        );
        drop(conn);
        let seen = warden.await.unwrap();
        assert_eq!(seen.len(), 4);
        assert!(
            matches!(&seen[2], Request::Outcome { op: 1, result: Outcome::NotApplied { reason } } if reason == LAPSED)
        );
    }
}
