//! What the socket was asked, as `access` entries in the Island log
//! (docs/slice-2.md, "The island socket", rules): batched, one per Vessel
//! and provider at most every `access_every_secs`, with the number of
//! calls, bytes each way, and one digest over the calls' digests.
//!
//! A call's digest ties the entry to the agent's own record of the call:
//!
//! ```text
//! call  = blake3_derive_key("island.inference-call.v1",
//!                           u64le(len request) || request || response)
//! batch = blake3_derive_key("island.access-batch.v1", call₁ || call₂ || …)
//! ```
//!
//! where `request` is the body the agent sent and `response` the body it
//! received, byte for byte, in the order the calls ended. The agent
//! records the call digest with each model message in its Chronicle.

use std::collections::BTreeMap;

use island_client::wire::Refusal;
use island_client::{Broker, Signed, Writer};
use island_core::{Access, AccessKind, Body, Hash, PublicKey, StreamId, VesselId};

/// The call digest's domain.
pub const CALL_DOMAIN: &str = "island.inference-call.v1";
/// The batch digest's domain.
pub const BATCH_DOMAIN: &str = "island.access-batch.v1";

/// A call's digest, computed as the bytes go by.
#[derive(Clone)]
pub struct CallDigest(blake3::Hasher);

impl CallDigest {
    /// Begin with the request.
    pub fn new(request: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key(CALL_DOMAIN);
        hasher.update(&(request.len() as u64).to_le_bytes());
        hasher.update(request);
        Self(hasher)
    }

    /// Some of the response.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The digest.
    pub fn finish(&self) -> Hash {
        *self.0.finalize().as_bytes()
    }
}

/// A whole call's digest at once.
pub fn call_digest(request: &[u8], response: &[u8]) -> Hash {
    let mut digest = CallDigest::new(request);
    digest.update(response);
    digest.finish()
}

/// The digest over calls' digests.
pub fn batch_digest(calls: &[Hash]) -> Hash {
    let mut hasher = blake3::Hasher::new_derive_key(BATCH_DOMAIN);
    for call in calls {
        hasher.update(call);
    }
    *hasher.finalize().as_bytes()
}

/// Calls not yet written, for one Vessel and provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// Calls.
    pub calls: u32,
    /// Bytes the agent sent.
    pub bytes_in: u64,
    /// Bytes the agent received.
    pub bytes_out: u64,
    /// Each call's digest, in the order the calls ended.
    pub digests: Vec<Hash>,
}

/// Every batch waiting, and the entry whose fate is not known.
#[derive(Default)]
pub struct Batches {
    pending: BTreeMap<(VesselId, String), Pending>,
    /// Sent, but the answer was lost: sent again, byte for byte, before
    /// anything else (the broker answers an exact resend with the first
    /// answer).
    unsent: Option<Signed>,
}

impl Batches {
    /// One call ended.
    pub fn add(
        &mut self,
        vessel: VesselId,
        provider: &str,
        bytes_in: u64,
        bytes_out: u64,
        digest: Hash,
    ) {
        let pending = self
            .pending
            .entry((vessel, provider.to_owned()))
            .or_default();
        pending.calls = pending.calls.saturating_add(1);
        pending.bytes_in = pending.bytes_in.saturating_add(bytes_in);
        pending.bytes_out = pending.bytes_out.saturating_add(bytes_out);
        pending.digests.push(digest);
    }

    /// The entries to write now, oldest batch first, and nothing left
    /// pending.
    pub fn take(&mut self) -> Vec<Access> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|((vessel, provider), pending)| Access {
                vessel,
                what: AccessKind::Inference,
                provider,
                calls: pending.calls,
                bytes_in: pending.bytes_in,
                bytes_out: pending.bytes_out,
                digest: batch_digest(&pending.digests),
            })
            .collect()
    }

    /// Put back what could not be written: its counts and digest go out
    /// with the next batch of the same Vessel and provider.
    fn put_back(&mut self, access: Access) {
        let pending = self
            .pending
            .entry((access.vessel, access.provider))
            .or_default();
        // The batch's digest stands for its calls in the next batch: the
        // agent's records still tie in, one level down.
        pending.calls = pending.calls.saturating_add(access.calls);
        pending.bytes_in = pending.bytes_in.saturating_add(access.bytes_in);
        pending.bytes_out = pending.bytes_out.saturating_add(access.bytes_out);
        pending.digests.insert(0, access.digest);
    }

    /// Whether anything waits.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.unsent.is_none()
    }
}

/// How a flush went.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Flushed {
    /// Entries the log took.
    pub written: Vec<u64>,
    /// Entries the gate refused, with why: they are dropped, and said.
    pub refused: Vec<Refusal>,
}

/// Write what is pending, one entry per Vessel and provider.
pub async fn flush(
    batches: &tokio::sync::Mutex<Batches>,
    broker: &Broker,
    writer: &Writer,
    me: &PublicKey,
) -> Result<Flushed, String> {
    let mut flushed = Flushed::default();
    // First the entry whose answer was lost.
    let unsent = batches.lock().await.unsent.take();
    if let Some(signed) = unsent {
        match broker.append(&signed.submission, &signed.sig_writer).await {
            Ok(Ok((seq, _))) => flushed.written.push(seq),
            Ok(Err(refusal)) => flushed.refused.push(refusal),
            Err(error) => {
                batches.lock().await.unsent = Some(signed);
                return Err(format!("the broker: {error}"));
            }
        }
    }
    let entries = batches.lock().await.take();
    let mut entries = entries.into_iter();
    while let Some(access) = entries.next() {
        let counter = match broker.capabilities(me).await {
            Ok(capabilities) => capabilities.next_counter,
            Err(error) => {
                let mut batches = batches.lock().await;
                batches.put_back(access);
                entries.for_each(|rest| batches.put_back(rest));
                return Err(format!("the broker: {error}"));
            }
        };
        let signed = writer.submission(
            StreamId::from(StreamId::ACCESS),
            counter,
            Body::Access(access),
        );
        match broker.append(&signed.submission, &signed.sig_writer).await {
            Ok(Ok((seq, _))) => flushed.written.push(seq),
            Ok(Err(refusal)) => flushed.refused.push(refusal),
            Err(error) => {
                let mut batches = batches.lock().await;
                batches.unsent = Some(signed);
                entries.for_each(|rest| batches.put_back(rest));
                return Err(format!("the broker: {error}"));
            }
        }
    }
    Ok(flushed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digests_follow_the_written_layout() {
        let mut layout = Vec::new();
        layout.extend(3u64.to_le_bytes());
        layout.extend(b"req");
        layout.extend(b"response");
        assert_eq!(
            call_digest(b"req", b"response"),
            blake3::derive_key(CALL_DOMAIN, &layout)
        );
        // Streamed in pieces, the same.
        let mut streamed = CallDigest::new(b"req");
        streamed.update(b"resp");
        streamed.update(b"onse");
        assert_eq!(streamed.finish(), call_digest(b"req", b"response"));
        let (a, b) = ([1; 32], [2; 32]);
        assert_eq!(
            batch_digest(&[a, b]),
            blake3::derive_key(BATCH_DOMAIN, &[[1u8; 32], [2; 32]].concat())
        );
        assert_ne!(batch_digest(&[a, b]), batch_digest(&[b, a]), "order counts");
    }

    #[test]
    fn calls_batch_per_vessel_and_provider() {
        let mut batches = Batches::default();
        batches.add([1; 16], "local", 10, 100, [1; 32]);
        batches.add([1; 16], "local", 20, 200, [2; 32]);
        batches.add([1; 16], "gpu", 5, 50, [3; 32]);
        batches.add([2; 16], "local", 1, 1, [4; 32]);
        let mut entries = batches.take();
        entries.sort_by(|a, b| (a.vessel, &a.provider).cmp(&(b.vessel, &b.provider)));
        assert_eq!(entries.len(), 3);
        let local = &entries[1];
        assert_eq!((local.calls, local.bytes_in, local.bytes_out), (2, 30, 300));
        assert_eq!(local.digest, batch_digest(&[[1; 32], [2; 32]]));
        assert_eq!(local.what, AccessKind::Inference);
        assert!(batches.is_empty());
    }

    #[test]
    fn a_batch_put_back_goes_out_with_the_next() {
        let mut batches = Batches::default();
        batches.add([1; 16], "local", 10, 100, [1; 32]);
        let first = batches.take().remove(0);
        let first_digest = first.digest;
        batches.add([1; 16], "local", 1, 1, [9; 32]);
        batches.put_back(first);
        let merged = batches.take().remove(0);
        assert_eq!((merged.calls, merged.bytes_in), (2, 11));
        assert_eq!(merged.digest, batch_digest(&[first_digest, [9; 32]]));
    }
}
