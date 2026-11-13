//! Approvals of Taso effects: spike S2's stand-in for slice 2's approval
//! kind (S2 plan, decisions D1 and D2; docs/spec.md, "The review gate
//! lives in the broker").
//!
//! A Vessel's released effect is sent only once an approver has named it.
//! The spec has an approval name "the hash of the exact request bytes";
//! until slice 2 brings a kind of its own, signed with the phone's passkey,
//! the marks are `preference` entries on the `plan` stream:
//!
//! | key       | value                                    | written by    |
//! |-----------|------------------------------------------|---------------|
//! | `approve` | request hash                             | an approver   |
//! | `decline` | request hash                             | an approver   |
//! | `sent`    | `<vessel> <op> <request hash> <approval>` | island-sender |
//!
//! One approval lets one effect out: a `sent` mark consumes the approval
//! it names, so a second identical request needs a second approval. A
//! decline stops every not-yet-sent effect with that request hash, and
//! wins over an approval: it fails closed. Marks count only from the keys
//! [`Marks`] is told to trust, and only once the gate accepted them (they
//! are read from a verified log).

use std::collections::{BTreeMap, BTreeSet};

use island_core::{Body, Hash, PublicKey, Record, StreamId};

/// The stream the marks live on.
pub const STREAM: &str = StreamId::PLAN;
/// The key of an approval.
pub const APPROVE: &str = "approve";
/// The key of a decline.
pub const DECLINE: &str = "decline";
/// The key of a sender's record that it sent an effect.
pub const SENT: &str = "sent";
/// The BLAKE3 derive-key context of a request hash.
pub const REQUEST_DOMAIN: &str = "island.effect-request.v1";

/// The hash an approval names: of the provider and the exact request
/// bytes, as a Vessel declared them to Taso.
pub fn request_hash(provider: &str, request: &[u8]) -> Hash {
    let mut bytes = Vec::with_capacity(8 + provider.len() + request.len());
    bytes.extend_from_slice(&(provider.len() as u64).to_le_bytes());
    bytes.extend_from_slice(provider.as_bytes());
    bytes.extend_from_slice(request);
    blake3::derive_key(REQUEST_DOMAIN, &bytes)
}

/// The value of a `sent` mark.
pub fn sent_value(vessel: &str, op: u64, hash: &Hash, approval: u64) -> String {
    format!("{vessel} {op} {} {approval}", hex(hash))
}

/// One Vessel effect, as the sender met it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Sent {
    /// The entry of the `sent` mark.
    pub entry: u64,
    /// The approval it consumed.
    pub approval: u64,
}

/// What the marks in a log say, so far.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    approvers: BTreeSet<PublicKey>,
    sender: Option<PublicKey>,
    /// Approvals by request hash, oldest first.
    approvals: BTreeMap<Hash, Vec<u64>>,
    /// Declines by request hash: the first one.
    declines: BTreeMap<Hash, u64>,
    /// Sends by (vessel, op).
    sent: BTreeMap<(String, u64), Sent>,
    /// Approvals a send has consumed.
    consumed: BTreeSet<u64>,
}

impl Marks {
    /// Marks that count approvals and declines from `approvers` and
    /// sends from `sender`.
    pub fn new(approvers: impl IntoIterator<Item = PublicKey>, sender: Option<PublicKey>) -> Self {
        Self {
            approvers: approvers.into_iter().collect(),
            sender,
            ..Self::default()
        }
    }

    /// Take in one record of a verified log. Anything that is not a mark
    /// from a trusted key is ignored, and so is a malformed value.
    pub fn note(&mut self, record: &Record) {
        let entry = &record.entry;
        let submission = &entry.submission;
        if submission.stream.as_bytes() != STREAM.as_bytes() {
            return;
        }
        let Body::Preference(preference) = &submission.body else {
            return;
        };
        let from_approver = self.approvers.contains(&submission.writer);
        let from_sender = self.sender == Some(submission.writer);
        match preference.key.as_str() {
            APPROVE if from_approver => {
                if let Some(hash) = unhex(preference.value.trim()) {
                    self.approvals.entry(hash).or_default().push(entry.seq);
                }
            }
            DECLINE if from_approver => {
                if let Some(hash) = unhex(preference.value.trim()) {
                    self.declines.entry(hash).or_insert(entry.seq);
                }
            }
            SENT if from_sender => {
                let parts: Vec<&str> = preference.value.split(' ').collect();
                if let [vessel, op, _hash, approval] = parts[..]
                    && let (Ok(op), Ok(approval)) = (op.parse(), approval.parse())
                {
                    self.consumed.insert(approval);
                    self.sent.entry(((*vessel).to_owned(), op)).or_insert(Sent {
                        entry: entry.seq,
                        approval,
                    });
                }
            }
            _ => {}
        }
    }

    /// Whether this Vessel's op was sent, and by which mark.
    pub fn sent(&self, vessel: &str, op: u64) -> Option<Sent> {
        self.sent.get(&(vessel.to_owned(), op)).copied()
    }

    /// The first decline of this request hash.
    pub fn declined(&self, hash: &Hash) -> Option<u64> {
        self.declines.get(hash).copied()
    }

    /// The oldest approval of this request hash that no send has consumed.
    pub fn approval(&self, hash: &Hash) -> Option<u64> {
        self.approvals
            .get(hash)?
            .iter()
            .copied()
            .find(|seq| !self.consumed.contains(seq))
    }
}

/// What to do with one released effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Already sent (a mark exists): report Applied, send nothing.
    AlreadySent(Sent),
    /// Declined: report NotApplied.
    Decline(u64),
    /// Approved by this entry and not sent: send, then report Applied.
    Send(u64),
    /// Not approved, and its outcome was lost in a Warden restart: report
    /// NotApplied ("lapsed"), so the Vessel gets its effect authority
    /// back and proposes it again (decision D1's lapse rule).
    Lapse,
    /// Not approved yet: leave it on the Open list.
    Wait,
}

/// The decision for one released effect. The order is the rule: a send
/// already made is final; then a decline; then an approval; then the
/// lapse rule; otherwise wait.
pub fn decide(marks: &Marks, vessel: &str, op: u64, hash: &Hash, unknown: bool) -> Decision {
    if let Some(sent) = marks.sent(vessel, op) {
        return Decision::AlreadySent(sent);
    }
    if let Some(seq) = marks.declined(hash) {
        return Decision::Decline(seq);
    }
    if let Some(seq) = marks.approval(hash) {
        return Decision::Send(seq);
    }
    if unknown {
        return Decision::Lapse;
    }
    Decision::Wait
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A 32-byte value from 64 hex digits.
pub fn unhex(text: &str) -> Option<Hash> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0_u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use island_core::{Entry, Preference, Submission};

    const APPROVER: PublicKey = [1; 32];
    const SENDER: PublicKey = [2; 32];
    const STRANGER: PublicKey = [3; 32];

    fn mark(seq: u64, writer: PublicKey, stream: &str, key: &str, value: &str) -> Record {
        Record {
            entry: Entry {
                seq,
                at: seq,
                policy: 2,
                prev: [0; 32],
                submission: Submission {
                    stream: StreamId::from(stream),
                    writer,
                    counter: seq,
                    body: Body::Preference(Preference {
                        key: key.to_owned(),
                        value: value.to_owned(),
                    }),
                },
                sig_writer: [0; 64],
            },
            hash: [0; 32],
            sig_appender: [0; 64],
        }
    }

    fn marks(records: &[Record]) -> Marks {
        let mut m = Marks::new([APPROVER], Some(SENDER));
        for r in records {
            m.note(r);
        }
        m
    }

    #[test]
    fn request_hash_is_stable_and_binds_the_provider() {
        // Changing this vector changes every approval ever written.
        assert_eq!(
            hex(&request_hash("island", b"s2 turn 1 effect 1 (nem)")),
            hex(&blake3::derive_key(
                REQUEST_DOMAIN,
                &[
                    &6_u64.to_le_bytes()[..],
                    b"island",
                    b"s2 turn 1 effect 1 (nem)"
                ]
                .concat()
            ))
        );
        assert_ne!(request_hash("island", b"x"), request_hash("mail", b"x"));
        assert_ne!(request_hash("is", b"landx"), request_hash("island", b"x"));
    }

    #[test]
    fn nothing_approved_waits_and_a_lost_outcome_lapses() {
        let m = marks(&[]);
        let h = request_hash("island", b"r");
        assert_eq!(decide(&m, "v", 1, &h, false), Decision::Wait);
        assert_eq!(decide(&m, "v", 1, &h, true), Decision::Lapse);
    }

    #[test]
    fn an_approval_sends_once() {
        let h = request_hash("island", b"r");
        let approve = mark(10, APPROVER, STREAM, APPROVE, &hex(&h));
        assert_eq!(
            decide(&marks(std::slice::from_ref(&approve)), "v", 1, &h, false),
            Decision::Send(10)
        );
        let sent = mark(11, SENDER, STREAM, SENT, &sent_value("v", 1, &h, 10));
        let m = marks(&[approve, sent]);
        let done = Decision::AlreadySent(Sent {
            entry: 11,
            approval: 10,
        });
        assert_eq!(decide(&m, "v", 1, &h, false), done);
        // After a Warden restart the same op comes back unknown: still sent.
        assert_eq!(decide(&m, "v", 1, &h, true), done);
        // An identical request from another op needs its own approval.
        assert_eq!(decide(&m, "v", 2, &h, false), Decision::Wait);
    }

    #[test]
    fn a_decline_wins_over_an_approval_but_not_over_a_send() {
        let h = request_hash("island", b"r");
        let approve = mark(10, APPROVER, STREAM, APPROVE, &hex(&h));
        let decline = mark(12, APPROVER, STREAM, DECLINE, &hex(&h));
        assert_eq!(
            decide(
                &marks(&[approve.clone(), decline.clone()]),
                "v",
                1,
                &h,
                false
            ),
            Decision::Decline(12)
        );
        let sent = mark(11, SENDER, STREAM, SENT, &sent_value("v", 1, &h, 10));
        assert!(matches!(
            decide(&marks(&[approve, sent, decline]), "v", 1, &h, false),
            Decision::AlreadySent(_)
        ));
    }

    #[test]
    fn marks_count_only_from_trusted_keys_on_the_plan_stream() {
        let h = request_hash("island", b"r");
        let m = marks(&[
            mark(10, STRANGER, STREAM, APPROVE, &hex(&h)),
            mark(11, SENDER, STREAM, APPROVE, &hex(&h)),
            mark(12, APPROVER, "island", APPROVE, &hex(&h)),
            mark(13, APPROVER, STREAM, APPROVE, "not hex"),
            mark(14, STRANGER, STREAM, SENT, &sent_value("v", 1, &h, 99)),
            mark(15, APPROVER, STREAM, SENT, &sent_value("v", 1, &h, 99)),
        ]);
        assert_eq!(decide(&m, "v", 1, &h, false), Decision::Wait);
    }

    #[test]
    fn hex_round_trips() {
        let h = request_hash("p", b"q");
        assert_eq!(unhex(&hex(&h)), Some(h));
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex(&"é".repeat(32)), None);
    }
}
