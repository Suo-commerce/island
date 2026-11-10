//! A writer: one key, building and signing its submissions.

use island_core::{
    Body, Preference, PublicKey, Signature, SigningKey, StreamId, Submission, Verse, public_key,
    sign_hash,
};

/// A submission with its writer's signature: everything the broker needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    /// The submission.
    pub submission: Submission,
    /// The writer's signature over its hash.
    pub sig_writer: Signature,
}

/// One writer key. The key stays here: what leaves is signed submissions.
#[derive(Clone)]
pub struct Writer {
    key: SigningKey,
}

impl Writer {
    /// A writer that signs with `key`.
    pub const fn new(key: SigningKey) -> Self {
        Self { key }
    }

    /// The writer's public key: its name in the log.
    pub fn public(&self) -> PublicKey {
        public_key(&self.key)
    }

    /// A signed submission of `body` to `stream`. `counter` is the
    /// writer's next number, which `my_capabilities` tells.
    pub fn submission(&self, stream: StreamId, counter: u64, body: Body) -> Signed {
        let submission = Submission {
            stream,
            writer: self.public(),
            counter,
            body,
        };
        let sig_writer = sign_hash(&self.key, &submission.hash());
        Signed {
            submission,
            sig_writer,
        }
    }

    /// A signed verse.
    pub fn verse(&self, stream: &str, counter: u64, verse: Verse) -> Signed {
        self.submission(StreamId::from(stream), counter, Body::Verse(verse))
    }

    /// A signed preference.
    pub fn preference(&self, stream: &str, counter: u64, key: &str, value: &str) -> Signed {
        let preference = Preference {
            key: key.to_owned(),
            value: value.to_owned(),
        };
        self.submission(
            StreamId::from(stream),
            counter,
            Body::Preference(preference),
        )
    }
}
