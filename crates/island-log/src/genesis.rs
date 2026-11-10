//! The two submissions that start a log (docs/spec.md, "Genesis").
//!
//! Both are signed by the root key. Entry 0 declares it; entry 1 holds the
//! initial grants:
//!
//! - the root capability: `policy`, `grant` and `revoke` entries on the
//!   `policy` and `authority` streams, which is all the root key is ever
//!   used for after genesis;
//! - the appender's: `verdict` entries on the `gate` stream. Its holder is
//!   the key every `sig_appender` is checked against.

use std::collections::BTreeSet;

use island_core::{Body, Bootstrap, Genesis, Grant, Kind, PublicKey, StreamId, Submission};

/// Entry 0: the root key declares itself. Counter 1 of the root key.
pub fn genesis(root: PublicKey) -> Submission {
    Submission {
        stream: StreamId::from(StreamId::AUTHORITY),
        writer: root,
        counter: 1,
        body: Body::Genesis(Genesis { root }),
    }
}

/// Entry 1: the initial grants. Counter 2 of the root key.
pub fn bootstrap(root: PublicKey, appender: PublicKey) -> Submission {
    let streams = |names: &[&str]| -> BTreeSet<StreamId> {
        names.iter().map(|name| StreamId::from(*name)).collect()
    };
    Submission {
        stream: StreamId::from(StreamId::AUTHORITY),
        writer: root,
        counter: 2,
        body: Body::Bootstrap(Bootstrap {
            grants: vec![
                Grant {
                    holder: root,
                    scope: streams(&[StreamId::AUTHORITY, StreamId::POLICY]),
                    rights: [Kind::Policy, Kind::Grant, Kind::Revoke].into(),
                    narrows: None,
                },
                Grant {
                    holder: appender,
                    scope: streams(&[StreamId::GATE]),
                    rights: [Kind::Verdict].into(),
                    narrows: None,
                },
            ],
        }),
    }
}
