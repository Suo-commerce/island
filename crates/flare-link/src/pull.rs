//! Pulling: bring the replica up to the broker's head, verifying every
//! record on the way (docs/spec.md, "The replica pull").

use std::fmt;

use island_client::{Error, Plain, codec};
use island_core::Hash;

use crate::replica::{Refused, Replica, ReplicaError};

/// What a pull did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pulled {
    /// How many records it added.
    pub added: u64,
    /// The replica's head afterwards.
    pub head: Option<(u64, Hash)>,
}

/// Why a pull added nothing.
#[derive(Debug)]
pub enum PullError {
    /// The broker could not be reached. The replica is as it was.
    Broker(Error),
    /// The broker served something the replica will not take. Nothing was
    /// added. This is what tampering looks like.
    Refused(Refused),
    /// The broker's log is shorter than the replica, or differs from it
    /// at a position both hold: it was rolled back, rewritten, or is
    /// another log. Nothing was added.
    Diverged(String),
    /// The replica itself could not be written.
    Replica(ReplicaError),
}

impl fmt::Display for PullError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Broker(error) => write!(f, "{error}"),
            Self::Refused(refused) => write!(
                f,
                "REFUSED: the broker served a record that does not verify, at {refused}. \
                 Nothing was added to the replica."
            ),
            Self::Diverged(why) => write!(f, "REFUSED: {why}. Nothing was added to the replica."),
            Self::Replica(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PullError {}

/// Pull everything after the replica's head from the broker.
///
/// The broker's word is taken for nothing. Every record is verified
/// against the replica before any is written, the batch is refused whole
/// on the first bad one, and the broker's head must be on the same chain
/// as the replica.
pub async fn pull(replica: &mut Replica, broker: &Plain) -> Result<Pulled, PullError> {
    let head = broker.head().await.map_err(PullError::Broker)?;
    let remote_hash: Hash = codec::unb64_fixed(&head.hash).ok_or_else(|| {
        PullError::Broker(Error::Protocol("/head: the hash is not 32 bytes".into()))
    })?;
    let local = replica.head();

    if let Some((seq, hash)) = local {
        if head.seq < seq {
            return Err(PullError::Diverged(format!(
                "the broker's log ends at entry {}, before the replica's head at entry {seq}",
                head.seq
            )));
        }
        if head.seq == seq {
            if hash != remote_hash {
                return Err(PullError::Diverged(format!(
                    "the broker's entry {seq} is not the replica's"
                )));
            }
            return Ok(Pulled {
                added: 0,
                head: local,
            });
        }
    }

    let batch = broker
        .log_after(local.map(|(seq, _)| seq))
        .await
        .map_err(PullError::Broker)?;

    let first = local.map_or(0, |(seq, _)| seq + 1);
    if let Some(out_of_place) = batch
        .iter()
        .zip(first..)
        .find(|(record, seq)| record.entry.seq != *seq)
    {
        // Chain::apply would say so too; this names the gap plainly.
        return Err(PullError::Refused(Refused {
            seq: out_of_place.1,
            why: format!(
                "the broker served entry {} in its place",
                out_of_place.0.entry.seq
            ),
        }));
    }

    // The records must reach the head the broker named, and that head
    // must be the entry they hold at its position. Records written since
    // may follow it.
    let at_head = usize::try_from(head.seq - first)
        .ok()
        .and_then(|index| batch.get(index));
    match at_head {
        Some(record) if record.hash == remote_hash => {}
        Some(_) => {
            return Err(PullError::Diverged(format!(
                "the broker's /head names an entry {} that is not the one its /log serves",
                head.seq
            )));
        }
        None => {
            return Err(PullError::Diverged(format!(
                "the broker's /log stops before entry {}, the head it names",
                head.seq
            )));
        }
    }
    let added = batch.len() as u64;
    match replica.append(batch).map_err(PullError::Replica)? {
        Ok(()) => Ok(Pulled {
            added,
            head: replica.head(),
        }),
        Err(refused) => Err(PullError::Refused(refused)),
    }
}
