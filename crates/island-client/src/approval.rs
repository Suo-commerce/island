//! Approvals of Taso effects, read from a verified log (docs/slice-2.md,
//! step 1; docs/spec.md, "The review gate lives in the broker").
//!
//! A Vessel's released effect is let out only once an `approval` entry
//! has approved its request hash ([`request_hash`]). The sender records
//! each effect it lets out as a `dispatch` entry naming the approval it
//! used; both kinds live on the `approvals` stream.
//!
//! The gate holds the rule in the log itself (island-core's
//! `State::dispatch_refusal`): a dispatch must name an approval of its
//! own request that no earlier dispatch used and that no later decline
//! overrides, and a Vessel's action key ([`effect_key`]) is dispatched at
//! most once. [`decide`] makes the sender's choice by the same rule, so
//! that what it tries to write is what the gate accepts.
//!
//! Who may approve and who may dispatch is authority's business: grants
//! of `approval` and `dispatch` on `approvals`. Everything here comes from
//! a log the caller verified, so every entry read was accepted; a refused
//! write is inside a `verdict` and is not read as an approval.

use std::collections::{BTreeMap, BTreeSet};

use island_core::{Answered, Body, Hash, Record, StreamId, VesselId};
pub use island_core::{effect_key, request_hash};

/// The stream approvals and dispatches live on.
pub const STREAM: &str = StreamId::APPROVALS;

/// One dispatch, as the log holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Sent {
    /// The entry of the `dispatch`.
    pub entry: u64,
    /// The approval it used.
    pub approval: u64,
}

/// What the approvals and dispatches in a log say, so far.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    /// Approvals by request hash, oldest first.
    approvals: BTreeMap<Hash, Vec<u64>>,
    /// Declines by request hash: the newest.
    declines: BTreeMap<Hash, u64>,
    /// Dispatches by (vessel, action key).
    sent: BTreeMap<(VesselId, Hash), Sent>,
    /// Approvals a dispatch used.
    used: BTreeSet<u64>,
}

impl Marks {
    /// Nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Take in one record of a verified log; anything that is not an
    /// approval or a dispatch is ignored.
    pub fn note(&mut self, record: &Record) {
        let seq = record.entry.seq;
        match &record.entry.submission.body {
            Body::Approval(approval) => match approval.verdict {
                Answered::Approve => self
                    .approvals
                    .entry(approval.request)
                    .or_default()
                    .push(seq),
                Answered::Decline => {
                    self.declines.insert(approval.request, seq);
                }
            },
            Body::Dispatch(dispatch) => {
                self.used.insert(dispatch.approval);
                self.sent
                    .entry((dispatch.vessel, dispatch.key))
                    .or_insert(Sent {
                        entry: seq,
                        approval: dispatch.approval,
                    });
            }
            _ => {}
        }
    }

    /// Whether this Vessel's action was dispatched, and by which entry.
    pub fn sent(&self, vessel: &VesselId, key: &Hash) -> Option<Sent> {
        self.sent.get(&(*vessel, *key)).copied()
    }

    /// The newest decline of this request hash.
    pub fn declined(&self, request: &Hash) -> Option<u64> {
        self.declines.get(request).copied()
    }

    /// The oldest approval of this request hash that no dispatch used and
    /// no later decline overrides: the one a dispatch may name.
    pub fn approval(&self, request: &Hash) -> Option<u64> {
        let declined = self.declined(request);
        self.approvals
            .get(request)?
            .iter()
            .copied()
            .find(|seq| !self.used.contains(seq) && declined.is_none_or(|at| at < *seq))
    }
}

/// What to do with one released effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Already dispatched: report Applied, send nothing.
    AlreadySent(Sent),
    /// Approved by this entry and not dispatched: dispatch, then report
    /// Applied.
    Send(u64),
    /// Declined, and no approval after the decline: report NotApplied.
    Decline(u64),
    /// Not approved yet: leave it on the Open list, however long, until
    /// Jarmo answers.
    Wait,
}

/// The decision for one released effect. The order is the rule: a
/// dispatch already made is final; then an approval the gate would take;
/// then a decline; otherwise wait.
///
/// Taso's `unknown` flag plays no part. It says an outcome may already
/// exist: after a Warden restart, and also whenever an earlier listing
/// handed the effect out, which a sender polling its socket does every
/// pass (Taso `effects.rs`, `hand_out`). Whether the action was let out is
/// what the log's dispatches say, and nothing else. An unknown action
/// that is not approved waits like any other: until it is answered, the
/// Vessel releases nothing new (Taso's barrier), which is the point. Up to
/// step 2 an unknown, unapproved action was reported not applied
/// ("lapsed"); with a polling sender that cancelled every action half a
/// second after it was proposed.
pub fn decide(marks: &Marks, vessel: &VesselId, key: &Hash, request: &Hash) -> Decision {
    if let Some(sent) = marks.sent(vessel, key) {
        return Decision::AlreadySent(sent);
    }
    if let Some(seq) = marks.approval(request) {
        return Decision::Send(seq);
    }
    if let Some(seq) = marks.declined(request) {
        return Decision::Decline(seq);
    }
    Decision::Wait
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `N` bytes from `2N` hex digits.
pub fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.is_ascii() {
        return None;
    }
    let mut out = [0_u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use island_core::{Approval, Dispatch, Entry, Preference, Submission};

    const V: VesselId = [7; 16];

    fn record(seq: u64, stream: &str, body: Body) -> Record {
        Record {
            entry: Entry {
                seq,
                at: seq,
                policy: 2,
                prev: [0; 32],
                submission: Submission {
                    stream: StreamId::from(stream),
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

    fn answer(seq: u64, request: Hash, verdict: Answered) -> Record {
        record(
            seq,
            STREAM,
            Body::Approval(Approval {
                request,
                verdict,
                proof: None,
            }),
        )
    }

    fn dispatch(seq: u64, key: Hash, request: Hash, approval: u64) -> Record {
        record(
            seq,
            STREAM,
            Body::Dispatch(Dispatch {
                vessel: V,
                op: seq,
                key,
                request,
                approval,
                counter: 0,
            }),
        )
    }

    fn marks(records: &[Record]) -> Marks {
        let mut m = Marks::new();
        for r in records {
            m.note(r);
        }
        m
    }

    #[test]
    fn nothing_approved_waits_even_unknown() {
        let m = marks(&[]);
        let (h, k) = (request_hash("island", b"r"), effect_key(b"k"));
        assert_eq!(decide(&m, &V, &k, &h), Decision::Wait);
    }

    #[test]
    fn an_approval_sends_once() {
        let (h, k) = (request_hash("island", b"r"), effect_key(b"k"));
        let approve = answer(10, h, Answered::Approve);
        assert_eq!(
            decide(&marks(std::slice::from_ref(&approve)), &V, &k, &h),
            Decision::Send(10)
        );
        let m = marks(&[approve, dispatch(11, k, h, 10)]);
        let done = Decision::AlreadySent(Sent {
            entry: 11,
            approval: 10,
        });
        // Again, as after a Warden restart: the dispatch decides.
        assert_eq!(decide(&m, &V, &k, &h), done);
        // An identical request with another key needs its own approval.
        assert_eq!(decide(&m, &V, &effect_key(b"k2"), &h), Decision::Wait);
        // The same key from another Vessel is another action.
        assert_eq!(decide(&m, &[8; 16], &k, &h), Decision::Wait);
    }

    #[test]
    fn a_decline_overrides_earlier_approvals_only() {
        let (h, k) = (request_hash("island", b"r"), effect_key(b"k"));
        let approve = answer(10, h, Answered::Approve);
        let decline = answer(12, h, Answered::Decline);
        let m = marks(&[approve.clone(), decline.clone()]);
        assert_eq!(decide(&m, &V, &k, &h), Decision::Decline(12));
        // Approved again after the decline: the new approval counts.
        let again = answer(13, h, Answered::Approve);
        assert_eq!(
            decide(
                &marks(&[approve.clone(), decline.clone(), again]),
                &V,
                &k,
                &h
            ),
            Decision::Send(13)
        );
        // A dispatch already made stands.
        assert!(matches!(
            decide(
                &marks(&[approve, dispatch(11, k, h, 10), decline]),
                &V,
                &k,
                &h
            ),
            Decision::AlreadySent(_)
        ));
    }

    #[test]
    fn only_approvals_and_dispatches_count() {
        let (h, k) = (request_hash("island", b"r"), effect_key(b"k"));
        let m = marks(&[record(
            10,
            StreamId::PLAN,
            Body::Preference(Preference {
                key: "approve".into(),
                value: hex(&h),
            }),
        )]);
        assert_eq!(decide(&m, &V, &k, &h), Decision::Wait);
    }

    #[test]
    fn hex_round_trips() {
        let h = request_hash("p", b"q");
        assert_eq!(unhex::<32>(&hex(&h)), Some(h));
        assert_eq!(unhex::<16>(&hex(&[3; 16])), Some([3; 16]));
        assert_eq!(unhex::<32>("zz"), None);
        assert_eq!(unhex::<32>(&"é".repeat(32)), None);
    }
}
