//! Island reference model: what the write path must answer, written to be
//! read, not to be fast.
//!
//! The model is a list of the submissions that are in the log, and one
//! function, [`Model::judge`], that says what must happen to the next one.
//! Everything is worked out from the list each time: a writer's counter is
//! how many entries it has in the list, a retry is "the same as that
//! writer's last entry". There is no cached state to get out of step.
//!
//! It shares no code with `island-log`. It uses `island-core` for the
//! types and for checking a signature; hashes, positions in time, files
//! and crashes are not its business. The harness runs the same
//! submissions through the model and the real appender and fails on any
//! difference.
//!
//! Covered so far (build step 4): the writer's signature, the counter and
//! retries, the genesis and bootstrap rules, and which kind belongs on
//! which stream. Grants join with build step 5, the gate with step 6.

#![warn(missing_docs)]

use island_core::{Body, Kind, Policy, PublicKey, Signature, StreamId, Submission};

/// What must happen to a submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It becomes the entry at `seq`.
    Accepted {
        /// The position it takes.
        seq: u64,
    },
    /// It is an exact resend of its writer's latest entry, at `seq`.
    /// Nothing is added.
    Retry {
        /// The position of the entry it repeats.
        seq: u64,
    },
    /// It is refused and nothing is added.
    Refused(Why),
}

/// Why a submission is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Why {
    /// The writer's signature does not verify.
    BadSignature,
    /// The counter was used before.
    CounterOld,
    /// The counter skips ahead.
    CounterGap,
    /// The counter is the writer's latest, with other content.
    CounterConflict,
    /// The first entry must be a genesis signed by the key it declares.
    NotGenesis,
    /// The genesis declares a root key other than the pinned one.
    RootNotPinned,
    /// The second entry must be a bootstrap signed by root.
    NotBootstrap,
    /// The bootstrap must name exactly one appender that is not root.
    BootstrapAppender,
    /// The bootstrap names an appender other than the one running.
    WrongAppender,
    /// A genesis or bootstrap after the first two entries.
    Misplaced,
    /// A verdict from outside: only the appender writes them, and not
    /// before the gate exists.
    Verdict,
    /// The kind does not belong on that stream.
    Stream,
}

/// The reference model of one log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pin: PublicKey,
    appender: PublicKey,
    log: Vec<Submission>,
}

impl Model {
    /// An empty log, for the pinned root key and the appender that runs
    /// it.
    pub fn new(pin: PublicKey, appender: PublicKey) -> Self {
        Self {
            pin,
            appender,
            log: Vec::new(),
        }
    }

    /// The submissions in the log, in order. Position `i` is entry `i`.
    pub fn log(&self) -> &[Submission] {
        &self.log
    }

    /// How many entries `writer` has: its latest counter.
    pub fn counter_of(&self, writer: &PublicKey) -> u64 {
        self.entries_of(writer).count() as u64
    }

    /// How many entries a stream holds, and the position of its newest.
    pub fn stream(&self, stream: &StreamId) -> Option<(u64, u64)> {
        let positions: Vec<u64> = (0u64..)
            .zip(&self.log)
            .filter(|(_, entry)| &entry.stream == stream)
            .map(|(seq, _)| seq)
            .collect();
        positions.last().map(|last| (positions.len() as u64, *last))
    }

    /// The newest policy entry: its position and its body.
    pub fn policy(&self) -> Option<(u64, &Policy)> {
        (0u64..)
            .zip(&self.log)
            .filter_map(|(seq, entry)| match &entry.body {
                Body::Policy(policy) => Some((seq, policy)),
                _ => None,
            })
            .last()
    }

    /// `writer`'s entries, oldest first, with their positions.
    fn entries_of<'a>(
        &'a self,
        writer: &'a PublicKey,
    ) -> impl Iterator<Item = (u64, &'a Submission)> {
        (0u64..)
            .zip(&self.log)
            .filter(move |(_, entry)| &entry.writer == writer)
    }

    /// What must happen to `submission`, signed with `sig_writer`. Changes
    /// nothing.
    pub fn judge(&self, submission: &Submission, sig_writer: &Signature) -> Outcome {
        // 1. Who wrote.
        if submission.verify(sig_writer).is_err() {
            return Outcome::Refused(Why::BadSignature);
        }

        // 2. The counter. A writer's entries carry 1, 2, 3, ... so its
        //    latest counter is how many entries it has.
        let used = self.counter_of(&submission.writer);
        if submission.counter != used + 1 {
            let latest = self.entries_of(&submission.writer).last();
            return match latest {
                Some((seq, entry)) if submission.counter == used => {
                    if entry == submission {
                        Outcome::Retry { seq }
                    } else {
                        Outcome::Refused(Why::CounterConflict)
                    }
                }
                _ if submission.counter > used + 1 => Outcome::Refused(Why::CounterGap),
                _ => Outcome::Refused(Why::CounterOld),
            };
        }

        // 3. The first two entries, and nothing like them later.
        let seq = self.log.len() as u64;
        let mut names_other_appender = false;
        match (seq, &submission.body) {
            (0, Body::Genesis(genesis)) => {
                if genesis.root != submission.writer {
                    return Outcome::Refused(Why::NotGenesis);
                }
                if genesis.root != self.pin {
                    return Outcome::Refused(Why::RootNotPinned);
                }
            }
            (0, _) => return Outcome::Refused(Why::NotGenesis),
            (1, Body::Bootstrap(bootstrap)) => {
                if submission.writer != self.pin {
                    return Outcome::Refused(Why::NotBootstrap);
                }
                let appenders: Vec<PublicKey> = bootstrap
                    .grants
                    .iter()
                    .filter(|grant| grant.rights.contains(&Kind::Verdict))
                    .map(|grant| grant.holder)
                    .collect();
                if appenders.len() != 1 || appenders[0] == self.pin {
                    return Outcome::Refused(Why::BootstrapAppender);
                }
                names_other_appender = appenders[0] != self.appender;
            }
            (1, _) => return Outcome::Refused(Why::NotBootstrap),
            (_, Body::Genesis(_) | Body::Bootstrap(_)) => {
                return Outcome::Refused(Why::Misplaced);
            }
            (_, Body::Verdict(_)) => return Outcome::Refused(Why::Verdict),
            _ => {}
        }

        // 4. The stream each kind belongs on.
        if !belongs_on(submission.kind(), &submission.stream) {
            return Outcome::Refused(Why::Stream);
        }

        // 5. The appender this log is run by is the one it names.
        if names_other_appender {
            return Outcome::Refused(Why::WrongAppender);
        }

        Outcome::Accepted { seq }
    }

    /// Judge a submission and, if it is accepted, add it to the log.
    pub fn submit(&mut self, submission: &Submission, sig_writer: &Signature) -> Outcome {
        let outcome = self.judge(submission, sig_writer);
        if matches!(outcome, Outcome::Accepted { .. }) {
            self.log.push(submission.clone());
        }
        outcome
    }
}

/// The streams of docs/spec.md's kinds table, as a table.
fn belongs_on(kind: Kind, stream: &StreamId) -> bool {
    const POLICY: &[u8] = b"policy";
    const AUTHORITY: &[u8] = b"authority";
    const GATE: &[u8] = b"gate";
    const PLAN: &[u8] = b"plan";

    let stream = stream.as_bytes();
    match kind {
        Kind::Policy => stream == POLICY,
        Kind::Grant | Kind::Revoke | Kind::Genesis | Kind::Bootstrap => stream == AUTHORITY,
        Kind::Verdict => stream == GATE,
        // A project: any stream that is not one of the system's.
        Kind::Verse => ![POLICY, AUTHORITY, GATE, PLAN].contains(&stream),
        // A project, or the plan.
        Kind::Preference => ![POLICY, AUTHORITY, GATE].contains(&stream),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use island_core::{
        Bootstrap, Evidence, Genesis, Grant, SigningKey, Verse, public_key, sign_hash,
    };

    use super::*;

    fn key(byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[byte; 32])
    }

    fn root() -> SigningKey {
        key(0x52)
    }

    fn appender() -> PublicKey {
        public_key(&key(0x41))
    }

    fn verse(writer: &SigningKey, counter: u64, text: &str) -> Submission {
        Submission {
            stream: StreamId::from("island"),
            writer: public_key(writer),
            counter,
            body: Body::Verse(Verse::new(Evidence::Decision, text, vec![])),
        }
    }

    fn genesis() -> Submission {
        Submission {
            stream: StreamId::from("authority"),
            writer: public_key(&root()),
            counter: 1,
            body: Body::Genesis(Genesis {
                root: public_key(&root()),
            }),
        }
    }

    fn bootstrap(appenders: &[PublicKey]) -> Submission {
        Submission {
            stream: StreamId::from("authority"),
            writer: public_key(&root()),
            counter: 2,
            body: Body::Bootstrap(Bootstrap {
                grants: appenders
                    .iter()
                    .map(|holder| Grant {
                        holder: *holder,
                        scope: BTreeSet::from([StreamId::from("gate")]),
                        rights: BTreeSet::from([Kind::Verdict]),
                        narrows: None,
                    })
                    .collect(),
            }),
        }
    }

    fn submit(model: &mut Model, submission: &Submission, key: &SigningKey) -> Outcome {
        model.submit(submission, &sign_hash(key, &submission.hash()))
    }

    fn started() -> Model {
        let mut model = Model::new(public_key(&root()), appender());
        assert_eq!(
            submit(&mut model, &genesis(), &root()),
            Outcome::Accepted { seq: 0 }
        );
        assert_eq!(
            submit(&mut model, &bootstrap(&[appender()]), &root()),
            Outcome::Accepted { seq: 1 }
        );
        model
    }

    #[test]
    fn a_writer_counts_and_a_resend_repeats_its_latest_entry() {
        let mut model = started();
        let writer = key(1);
        assert_eq!(
            submit(&mut model, &verse(&writer, 1, "a"), &writer),
            Outcome::Accepted { seq: 2 }
        );
        assert_eq!(
            submit(&mut model, &verse(&writer, 2, "b"), &writer),
            Outcome::Accepted { seq: 3 }
        );

        assert_eq!(
            submit(&mut model, &verse(&writer, 2, "b"), &writer),
            Outcome::Retry { seq: 3 }
        );
        assert_eq!(
            submit(&mut model, &verse(&writer, 2, "other"), &writer),
            Outcome::Refused(Why::CounterConflict)
        );
        assert_eq!(
            submit(&mut model, &verse(&writer, 1, "a"), &writer),
            Outcome::Refused(Why::CounterOld)
        );
        assert_eq!(
            submit(&mut model, &verse(&writer, 0, "zero"), &writer),
            Outcome::Refused(Why::CounterOld)
        );
        assert_eq!(
            submit(&mut model, &verse(&writer, 4, "early"), &writer),
            Outcome::Refused(Why::CounterGap)
        );
        // Nothing refused or repeated was added.
        assert_eq!(model.log().len(), 4);
        assert_eq!(model.counter_of(&public_key(&writer)), 2);
        assert_eq!(model.stream(&StreamId::from("island")), Some((2, 3)));
        assert_eq!(model.stream(&StreamId::from("nowhere")), None);
    }

    #[test]
    fn a_signature_by_anyone_else_is_refused_first() {
        let mut model = started();
        let (writer, other) = (key(1), key(2));
        // Wrong signature and wrong counter: the signature is what counts.
        assert_eq!(
            submit(&mut model, &verse(&writer, 9, "a"), &other),
            Outcome::Refused(Why::BadSignature)
        );
    }

    #[test]
    fn the_first_two_entries_are_genesis_and_bootstrap() {
        let pin = public_key(&root());
        let mut model = Model::new(pin, appender());
        let writer = key(1);

        assert_eq!(
            submit(&mut model, &verse(&writer, 1, "a"), &writer),
            Outcome::Refused(Why::NotGenesis)
        );
        let mut other_root = genesis();
        other_root.writer = public_key(&writer);
        other_root.body = Body::Genesis(Genesis {
            root: public_key(&writer),
        });
        assert_eq!(
            submit(&mut model, &other_root, &writer),
            Outcome::Refused(Why::RootNotPinned)
        );
        assert_eq!(
            submit(&mut model, &genesis(), &root()),
            Outcome::Accepted { seq: 0 }
        );

        assert_eq!(
            submit(&mut model, &verse(&writer, 1, "a"), &writer),
            Outcome::Refused(Why::NotBootstrap)
        );
        for appenders in [vec![], vec![appender(), [7; 32]], vec![pin]] {
            assert_eq!(
                submit(&mut model, &bootstrap(&appenders), &root()),
                Outcome::Refused(Why::BootstrapAppender)
            );
        }
        assert_eq!(
            submit(&mut model, &bootstrap(&[[7; 32]]), &root()),
            Outcome::Refused(Why::WrongAppender)
        );
        assert_eq!(
            submit(&mut model, &bootstrap(&[appender()]), &root()),
            Outcome::Accepted { seq: 1 }
        );

        let mut again = genesis();
        again.counter = 3;
        assert_eq!(
            submit(&mut model, &again, &root()),
            Outcome::Refused(Why::Misplaced)
        );
    }

    #[test]
    fn kinds_belong_on_their_streams() {
        let mut model = started();
        let writer = key(1);
        for stream in ["policy", "authority", "gate", "plan"] {
            let mut misplaced = verse(&writer, 1, "a");
            misplaced.stream = StreamId::from(stream);
            assert_eq!(
                submit(&mut model, &misplaced, &writer),
                Outcome::Refused(Why::Stream),
                "a verse on {stream}"
            );
        }
        let mut policy = verse(&root(), 3, "");
        policy.body = Body::Policy(Policy::default());
        assert_eq!(
            submit(&mut model, &policy, &root()),
            Outcome::Refused(Why::Stream)
        );
        policy.stream = StreamId::from("policy");
        assert_eq!(
            submit(&mut model, &policy, &root()),
            Outcome::Accepted { seq: 2 }
        );
        assert_eq!(model.policy(), Some((2, &Policy::default())));
    }
}
