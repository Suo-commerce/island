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
//! What sending means in the spike: the `sent` mark this program appends
//! to the log, which is also what makes a send happen once. A real
//! provider (a verse from a Vessel, a mail) is slice 2.

#![warn(missing_docs)]

use std::io::{Read, Write};

use island_client::approval::{self, Decision, Marks};
use island_core::Hash;
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
    /// The decision.
    pub decision: Decision,
}

/// The decision for one effect of `vessel`, from the marks so far.
pub fn plan(marks: &Marks, vessel: &str, effect: OpenEffect) -> Planned {
    let hash = approval::request_hash(&effect.provider, &effect.request);
    let decision = approval::decide(marks, vessel, effect.op, &hash, effect.unknown);
    Planned {
        effect,
        hash,
        decision,
    }
}

/// The outcome to report for a decision, once any send is done.
/// `sent_entry` is the entry of the `sent` mark for [`Decision::Send`].
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

/// The Vessel id a sender socket's name carries: `<id>.sock`.
pub fn vessel_of(socket: &std::path::Path) -> Option<String> {
    let name = socket.file_name()?.to_str()?;
    name.strip_suffix(".sock").map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use taso_core::record::{Consequence, RetryPolicy};
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

    #[test]
    fn the_hash_is_the_approval_modules() {
        let p = plan(&Marks::default(), "v", effect(3, b"req", false));
        assert_eq!(p.hash, approval::request_hash("island", b"req"));
        assert_eq!(p.decision, Decision::Wait);
        assert_eq!(
            plan(&Marks::default(), "v", effect(3, b"req", true)).decision,
            Decision::Lapse
        );
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
        assert_eq!(
            vessel_of(std::path::Path::new("/run/taso/run/senders/ab12.sock")).as_deref(),
            Some("ab12")
        );
        assert_eq!(vessel_of(std::path::Path::new("/run/x")), None);
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
