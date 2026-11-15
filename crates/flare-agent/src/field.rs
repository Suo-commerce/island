//! The Field socket: the agent's channel to Taso's Warden (docs/slice-2.md,
//! "The Vessel and the agent"). Turns, the transcript, and actions.

use std::os::unix::net::UnixStream;
use std::path::Path;

use taso_core::record::{Consequence, RetryPolicy};
use taso_field::protocol::{EffectResult, Reply, Request, VERSION, recv_reply, send_request};

/// The default path of the Field socket in a Vessel.
pub const SOCKET: &str = "/run/taso/field.sock";

/// What became of a declared action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declared {
    /// Released to the sender, as this op.
    Released(u64),
    /// Held (a fork's actions are), as this op.
    Held(u64),
    /// Refused, and why: for one, while an earlier action's fate is open.
    Refused(String),
}

/// The Warden, as the agent uses it.
pub trait Field {
    /// The last turn its Chronicle has a Seal for.
    fn last_sealed_turn(&self) -> Option<u64>;
    /// Declare an irreversible action, deduplicated by `key`.
    fn declare(&mut self, provider: &str, request: &[u8], key: &[u8]) -> Result<Declared, String>;
    /// Record a transcript entry.
    fn transcript(&mut self, entry: &[u8]) -> Result<(), String>;
    /// End a turn: Taso seals `/work`. The Seal's lsn, if it sealed.
    fn turn_end(&mut self, turn: u64) -> Result<Option<u64>, String>;
}

/// The Field socket.
pub struct Socket {
    stream: UnixStream,
    last_sealed_turn: Option<u64>,
    calls: u64,
}

impl Socket {
    /// Connect and say Hello.
    pub fn connect(path: &Path) -> Result<Self, String> {
        let stream =
            UnixStream::connect(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut socket = Self {
            stream,
            last_sealed_turn: None,
            calls: 0,
        };
        match socket.ask(&Request::Hello { version: VERSION })? {
            Reply::Hello {
                last_sealed_turn, ..
            } => {
                socket.last_sealed_turn = last_sealed_turn;
                Ok(socket)
            }
            other => Err(format!("Hello answered with {other:?}")),
        }
    }

    fn ask(&mut self, request: &Request) -> Result<Reply, String> {
        send_request(&mut self.stream, request).map_err(|error| format!("the Field: {error}"))?;
        match recv_reply(&mut self.stream).map_err(|error| format!("the Field: {error}"))? {
            None => Err("the Warden closed the Field".to_owned()),
            Some(Reply::Error { code, message }) => {
                Err(format!("the Warden refused ({code:?}): {message}"))
            }
            Some(reply) => Ok(reply),
        }
    }
}

impl Field for Socket {
    fn last_sealed_turn(&self) -> Option<u64> {
        self.last_sealed_turn
    }

    fn declare(&mut self, provider: &str, request: &[u8], key: &[u8]) -> Result<Declared, String> {
        self.calls += 1;
        let reply = self.ask(&Request::Effect {
            call: self.calls,
            provider: provider.to_owned(),
            request: request.to_vec(),
            footprint: Vec::new(),
            retry: RetryPolicy::ProviderDeduplicated { key: key.to_vec() },
            consequence: Consequence::Irreversible,
            authority: b"flare-agent".to_vec(),
        })?;
        match reply {
            Reply::Effect { result, .. } => Ok(match result {
                EffectResult::Released { op } => Declared::Released(op),
                EffectResult::Held { op } => Declared::Held(op),
                EffectResult::Refused { reason } => Declared::Refused(reason),
            }),
            other => Err(format!("Effect answered with {other:?}")),
        }
    }

    fn transcript(&mut self, entry: &[u8]) -> Result<(), String> {
        match self.ask(&Request::Transcript {
            entry: entry.to_vec(),
        })? {
            Reply::Recorded { .. } => Ok(()),
            other => Err(format!("Transcript answered with {other:?}")),
        }
    }

    fn turn_end(&mut self, turn: u64) -> Result<Option<u64>, String> {
        match self.ask(&Request::TurnEnd {
            turn,
            quiescent: true,
        })? {
            Reply::TurnEnd { sealed, .. } => Ok(sealed),
            other => Err(format!("TurnEnd answered with {other:?}")),
        }
    }
}
