//! Island reference model: what the write path must answer, written to be
//! read, not to be fast.
//!
//! The model is a list of the submissions that are in the log, and one
//! function, [`Model::judge`], that says what must happen to the next one.
//! Everything is worked out from the list each time: a writer's counter is
//! how many of its submissions the list holds, a retry is "the same as the
//! last of them". There is no cached state to get out of step.
//!
//! It shares no code with `island-log`. It uses `island-core` for the
//! types and for checking a signature; hashes, positions in time, files
//! and crashes are not its business. The harness runs the same
//! submissions through the model and the real appender and fails on any
//! difference.
//!
//! Covered: the writer's signature, the counter and retries, the genesis
//! and bootstrap rules, which kind belongs on which stream, grants (who
//! may write what, narrowing, revoking, the two protected grants), and the
//! gate: the three invariants, the policy ratchet, the rule that a
//! dispatch lets out only an approved action once, and the verdict entry a
//! refusal leaves.

#![warn(missing_docs)]

use island_core::{
    Answered, Body, Dispatch, Grant, GrantId, Invariant, Kind, Policy, PublicKey, Refusal,
    Signature, StreamId, Submission, Verdict, Verse,
};

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
    /// The gate refuses it. A verdict holding it becomes the entry at
    /// `seq`, written by the appender, and its writer's counter is used.
    Verdict {
        /// The position the verdict takes.
        seq: u64,
        /// What the gate refuses it for.
        reason: Refusal,
    },
    /// It is an exact resend of its writer's latest submission, which the
    /// gate refused; the verdict is at `seq`. Nothing is added.
    VerdictRetry {
        /// The position of the verdict.
        seq: u64,
        /// What the gate refused it for.
        reason: Refusal,
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
    /// The bootstrap must hold exactly one root capability.
    BootstrapRoot,
    /// No grant in force lets the writer write that kind to that stream.
    NoGrant,
    /// A narrowing or a revoke names a grant that does not exist.
    UnknownGrant,
    /// A narrowing or a revoke names a grant no longer in force.
    EndedGrant,
    /// A narrowing names another holder.
    OtherHolder,
    /// A narrowing that adds a stream or a kind.
    Wider,
    /// A grant carrying a kind grants may not carry.
    Ungrantable,
    /// A narrowing or revoke of a protected grant that is not allowed.
    Guarded,
    /// The bootstrap names an appender other than the one running.
    WrongAppender,
    /// A genesis or bootstrap after the first two entries.
    Misplaced,
    /// A verdict from outside: only the appender's own gate writes them.
    Verdict,
    /// The appender's key writing anything but a verdict.
    AppenderWrites,
    /// The kind does not belong on that stream.
    Stream,
}

/// Whether a grant is one of the two every log starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// An ordinary grant.
    None,
    /// The root capability: never revoked, never narrowed below the right
    /// to write grants and revokes on the authority stream.
    Root,
    /// The appender's grant: never narrowed, never revoked.
    Appender,
}

/// A grant as the log holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    /// Its id: the entry that holds it, and its place there.
    pub id: GrantId,
    /// The grant.
    pub grant: Grant,
    /// Whether it is protected.
    pub guard: Guard,
    /// Whether it is in force: not revoked, not replaced.
    pub live: bool,
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

    /// How many of `writer`'s submissions the log holds: its latest
    /// counter. One the gate refused counts; it is held in its verdict.
    pub fn counter_of(&self, writer: &PublicKey) -> u64 {
        self.answered(writer).count() as u64
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

    /// Every grant the log has ever held, oldest first: read off the
    /// bootstrap, `grant` and `revoke` entries, in order.
    pub fn grants(&self) -> Vec<Lease> {
        let mut leases: Vec<Lease> = Vec::new();
        for (seq, entry) in (0u64..).zip(&self.log) {
            match &entry.body {
                Body::Bootstrap(bootstrap) => {
                    for (index, grant) in (0u32..).zip(&bootstrap.grants) {
                        leases.push(Lease {
                            id: GrantId::new(seq, index),
                            grant: grant.clone(),
                            guard: self.guard_of(grant),
                            live: true,
                        });
                    }
                }
                Body::Grant(grant) => {
                    // A narrowing ends the grant it names and is protected
                    // as that grant was.
                    let mut guard = Guard::None;
                    let old = grant.narrows;
                    if let Some(old) = leases.iter_mut().find(|lease| Some(lease.id) == old) {
                        old.live = false;
                        guard = old.guard;
                    }
                    leases.push(Lease {
                        id: GrantId::new(seq, 0),
                        grant: grant.clone(),
                        guard,
                        live: true,
                    });
                }
                Body::Revoke(revoke) => {
                    if let Some(old) = leases.iter_mut().find(|lease| lease.id == revoke.grant) {
                        old.live = false;
                    }
                }
                _ => {}
            }
        }
        leases
    }

    /// What a grant in the bootstrap entry is.
    fn guard_of(&self, grant: &Grant) -> Guard {
        if grant.rights.contains(&Kind::Verdict) {
            Guard::Appender
        } else if grant.holder == self.pin && can_grant(grant) {
            Guard::Root
        } else {
            Guard::None
        }
    }

    /// `writer`'s submissions that are in the log, oldest first: its
    /// entries, and the ones the gate refused, which their verdicts hold.
    /// Each comes with the position of the entry that holds it, and the
    /// refusal if there was one.
    fn answered<'a>(
        &'a self,
        writer: &'a PublicKey,
    ) -> impl Iterator<Item = (u64, &'a Submission, Option<Refusal>)> {
        (0u64..)
            .zip(&self.log)
            .filter_map(move |(seq, entry)| match &entry.body {
                _ if &entry.writer == writer => Some((seq, entry, None)),
                Body::Verdict(verdict) if &verdict.rejected.writer == writer => {
                    Some((seq, &*verdict.rejected, Some(verdict.reason)))
                }
                _ => None,
            })
    }

    /// What the gate refuses `submission` for, if anything: a verse must
    /// keep every invariant of the policy in force, and a policy must be
    /// no weaker than it.
    fn gate(&self, submission: &Submission) -> Option<Refusal> {
        let nothing = Policy::default();
        let in_force = self.policy().map_or(&nothing, |(_, policy)| policy);
        match &submission.body {
            Body::Verse(verse) => in_force
                .invariants
                .iter()
                .find(|invariant| breaks(verse, invariant))
                .map(|invariant| Refusal::Invariant(*invariant)),
            Body::Policy(new) => weakens(in_force, new).then_some(Refusal::WeakerPolicy),
            Body::Dispatch(dispatch) => self.dispatch_refusal(dispatch),
            _ => None,
        }
    }

    /// What is wrong with a dispatch, read off the log: an action of that
    /// Vessel and key already let out; then an approval that is not an
    /// `approve` of this very request at the position named; then one an
    /// earlier dispatch used; then a decline of the request after it.
    fn dispatch_refusal(&self, dispatch: &Dispatch) -> Option<Refusal> {
        let dispatches = self.log.iter().filter_map(|entry| match &entry.body {
            Body::Dispatch(earlier) => Some(earlier),
            _ => None,
        });
        if dispatches
            .clone()
            .any(|earlier| earlier.vessel == dispatch.vessel && earlier.key == dispatch.key)
        {
            return Some(Refusal::AlreadyDispatched);
        }
        let named = usize::try_from(dispatch.approval)
            .ok()
            .and_then(|position| self.log.get(position));
        let approves = matches!(
            named.map(|entry| &entry.body),
            Some(Body::Approval(approval))
                if approval.verdict == Answered::Approve && approval.request == dispatch.request
        );
        if !approves {
            return Some(Refusal::Unapproved);
        }
        if dispatches
            .clone()
            .any(|earlier| earlier.approval == dispatch.approval)
        {
            return Some(Refusal::ApprovalUsed);
        }
        let declined_later = (0u64..).zip(&self.log).any(|(seq, entry)| {
            seq > dispatch.approval
                && matches!(&entry.body, Body::Approval(approval)
                    if approval.verdict == Answered::Decline && approval.request == dispatch.request)
        });
        declined_later.then_some(Refusal::Declined)
    }

    /// What must happen to `submission`, signed with `sig_writer`. Changes
    /// nothing.
    pub fn judge(&self, submission: &Submission, sig_writer: &Signature) -> Outcome {
        // 1. Who wrote.
        if submission.verify(sig_writer).is_err() {
            return Outcome::Refused(Why::BadSignature);
        }

        // 2. The counter. A writer's submissions carry 1, 2, 3, ... so its
        //    latest counter is how many of them the log holds.
        let used = self.counter_of(&submission.writer);
        if submission.counter != used + 1 {
            let latest = self.answered(&submission.writer).last();
            return match latest {
                Some((seq, sent, refusal)) if submission.counter == used => match refusal {
                    _ if sent != submission => Outcome::Refused(Why::CounterConflict),
                    None => Outcome::Retry { seq },
                    Some(reason) => Outcome::VerdictRetry { seq, reason },
                },
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
                let guards: Vec<Guard> = bootstrap
                    .grants
                    .iter()
                    .map(|grant| self.guard_of(grant))
                    .collect();
                if guards.iter().filter(|guard| **guard == Guard::Root).count() != 1 {
                    return Outcome::Refused(Why::BootstrapRoot);
                }
                let ordinary = bootstrap.grants.iter().zip(&guards);
                if ordinary
                    .filter(|(_, guard)| **guard == Guard::None)
                    .any(|(grant, _)| !only_grantable(grant))
                {
                    return Outcome::Refused(Why::Ungrantable);
                }
                names_other_appender = appenders[0] != self.appender;
            }
            (1, _) => return Outcome::Refused(Why::NotBootstrap),
            (_, Body::Genesis(_) | Body::Bootstrap(_)) => {
                return Outcome::Refused(Why::Misplaced);
            }
            (_, Body::Verdict(_)) => return Outcome::Refused(Why::Verdict),
            // The appender's key orders and records refusals. It writes
            // nothing else, whatever grants name it.
            _ if seq >= 2 && submission.writer == self.appender => {
                return Outcome::Refused(Why::AppenderWrites);
            }
            _ => {}
        }

        // 4. The stream each kind belongs on.
        if !belongs_on(submission.kind(), &submission.stream) {
            return Outcome::Refused(Why::Stream);
        }

        // 5. Grants. The first two entries come before any grant.
        if seq >= 2
            && let Err(why) = self.authority(submission)
        {
            return Outcome::Refused(why);
        }

        // 6. The appender this log is run by is the one it names.
        if names_other_appender {
            return Outcome::Refused(Why::WrongAppender);
        }

        // 7. The gate. What it refuses still takes the position: as a
        //    verdict.
        match self.gate(submission) {
            Some(reason) if seq >= 2 => Outcome::Verdict { seq, reason },
            _ => Outcome::Accepted { seq },
        }
    }

    /// Whether authority lets `submission` in: a grant in force covers
    /// it, and if it is itself a grant or a revoke, what it does is
    /// allowed.
    fn authority(&self, submission: &Submission) -> Result<(), Why> {
        let leases = self.grants();
        let covered = leases.iter().any(|lease| {
            lease.live
                && lease.grant.holder == submission.writer
                && lease.grant.scope.contains(&submission.stream)
                && lease.grant.rights.contains(&submission.kind())
        });
        if !covered {
            return Err(Why::NoGrant);
        }

        let named = |id: GrantId| match leases.iter().find(|lease| lease.id == id) {
            None => Err(Why::UnknownGrant),
            Some(lease) if !lease.live => Err(Why::EndedGrant),
            Some(lease) => Ok(lease),
        };
        match &submission.body {
            Body::Grant(grant) => match grant.narrows {
                // A new grant.
                None => {
                    if !only_grantable(grant) {
                        return Err(Why::Ungrantable);
                    }
                }
                // A narrowing: the same holder, and nothing added.
                Some(id) => {
                    let old = named(id)?;
                    if old.guard == Guard::Appender {
                        return Err(Why::Guarded);
                    }
                    if grant.holder != old.grant.holder {
                        return Err(Why::OtherHolder);
                    }
                    let added_stream = grant.scope.iter().any(|s| !old.grant.scope.contains(s));
                    let added_kind = grant.rights.iter().any(|k| !old.grant.rights.contains(k));
                    if added_stream || added_kind {
                        return Err(Why::Wider);
                    }
                    if old.guard == Guard::Root && !can_grant(grant) {
                        return Err(Why::Guarded);
                    }
                }
            },
            Body::Revoke(revoke) => {
                let old = named(revoke.grant)?;
                if old.guard != Guard::None {
                    return Err(Why::Guarded);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Judge a submission and add what the answer adds to the log: the
    /// submission if it is accepted, a verdict holding it if the gate
    /// refuses it, and otherwise nothing.
    pub fn submit(&mut self, submission: &Submission, sig_writer: &Signature) -> Outcome {
        let outcome = self.judge(submission, sig_writer);
        match outcome {
            Outcome::Accepted { .. } => self.log.push(submission.clone()),
            Outcome::Verdict { reason, .. } => self.log.push(Submission {
                stream: StreamId::from("gate"),
                writer: self.appender,
                counter: self.counter_of(&self.appender) + 1,
                body: Body::Verdict(Verdict {
                    rejected: Box::new(submission.clone()),
                    sig_writer: *sig_writer,
                    reason,
                }),
            }),
            _ => {}
        }
        outcome
    }
}

/// Whether a verse breaks an invariant.
fn breaks(verse: &Verse, invariant: &Invariant) -> bool {
    // The evidence kinds are bytes 0 to 3; an artefact is 1.
    match invariant {
        Invariant::VerseNamesEvidence => verse.evidence > 3,
        Invariant::VerseTextMax { bytes } => verse.text.len() as u64 > u64::from(*bytes),
        Invariant::ArtefactHasRef => verse.evidence == 1 && verse.refs.is_empty(),
    }
}

/// Whether `new` gives up anything `old` requires: an invariant that is
/// gone, or a text bound that is gone or higher.
fn weakens(old: &Policy, new: &Policy) -> bool {
    old.invariants.iter().any(|required| match required {
        Invariant::VerseTextMax { bytes: old_bound } => !new
            .invariants
            .iter()
            .any(|kept| matches!(kept, Invariant::VerseTextMax { bytes } if bytes <= old_bound)),
        other => !new.invariants.contains(other),
    })
}

/// Whether a grant lets its holder write grants and revokes on the
/// authority stream: what the root capability must always keep.
fn can_grant(grant: &Grant) -> bool {
    grant.rights.contains(&Kind::Grant)
        && grant.rights.contains(&Kind::Revoke)
        && grant.scope.contains(&StreamId::from("authority"))
}

/// Whether a grant carries only the kinds an ordinary grant may.
fn only_grantable(grant: &Grant) -> bool {
    grant.rights.iter().all(|kind| {
        matches!(
            kind,
            Kind::Policy
                | Kind::Verse
                | Kind::Preference
                | Kind::Receipt
                | Kind::Access
                | Kind::Approval
                | Kind::Dispatch
        )
    })
}

/// The streams of docs/spec.md's kinds table, as a table.
fn belongs_on(kind: Kind, stream: &StreamId) -> bool {
    const POLICY: &[u8] = b"policy";
    const AUTHORITY: &[u8] = b"authority";
    const GATE: &[u8] = b"gate";
    const PLAN: &[u8] = b"plan";
    const REPOS: &[u8] = b"repos";
    const ACCESS: &[u8] = b"access";
    const APPROVALS: &[u8] = b"approvals";
    const SYSTEM: [&[u8]; 6] = [POLICY, AUTHORITY, GATE, REPOS, ACCESS, APPROVALS];

    let stream = stream.as_bytes();
    match kind {
        Kind::Policy => stream == POLICY,
        Kind::Grant | Kind::Revoke | Kind::Genesis | Kind::Bootstrap => stream == AUTHORITY,
        Kind::Verdict => stream == GATE,
        Kind::Receipt => stream == REPOS,
        Kind::Access => stream == ACCESS,
        Kind::Approval | Kind::Dispatch => stream == APPROVALS,
        // A project: any stream that is not one of the system's.
        Kind::Verse => !SYSTEM.contains(&stream) && stream != PLAN,
        // A project, or the plan.
        Kind::Preference => !SYSTEM.contains(&stream),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use island_core::{
        Bootstrap, Evidence, Genesis, Revoke, SigningKey, Verse, public_key, sign_hash,
    };

    use super::*;

    fn key(byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[byte; 32])
    }

    fn root() -> SigningKey {
        key(0x52)
    }

    fn pin() -> PublicKey {
        public_key(&root())
    }

    fn appender() -> PublicKey {
        public_key(&key(0x41))
    }

    fn grant(holder: PublicKey, scope: &[&str], rights: &[Kind]) -> Grant {
        Grant {
            holder,
            scope: scope.iter().map(|name| StreamId::from(*name)).collect(),
            rights: rights.iter().copied().collect(),
            narrows: None,
        }
    }

    fn root_capability() -> Grant {
        grant(
            pin(),
            &["authority", "policy"],
            &[Kind::Policy, Kind::Grant, Kind::Revoke],
        )
    }

    fn verse_on(stream: &str, writer: &SigningKey, counter: u64, text: &str) -> Submission {
        Submission {
            stream: StreamId::from(stream),
            writer: public_key(writer),
            counter,
            body: Body::Verse(Verse::new(Evidence::Decision, text, vec![])),
        }
    }

    fn verse(writer: &SigningKey, counter: u64, text: &str) -> Submission {
        verse_on("island", writer, counter, text)
    }

    fn by_root(counter: u64, stream: &str, body: Body) -> Submission {
        Submission {
            stream: StreamId::from(stream),
            writer: pin(),
            counter,
            body,
        }
    }

    fn genesis() -> Submission {
        by_root(1, "authority", Body::Genesis(Genesis { root: pin() }))
    }

    fn bootstrap(grants: Vec<Grant>) -> Submission {
        by_root(2, "authority", Body::Bootstrap(Bootstrap { grants }))
    }

    fn verdict(holder: PublicKey) -> Grant {
        grant(holder, &["gate"], &[Kind::Verdict])
    }

    fn submit(model: &mut Model, submission: &Submission, key: &SigningKey) -> Outcome {
        model.submit(submission, &sign_hash(key, &submission.hash()))
    }

    /// A started log in which writer `key(1)` may write verses and
    /// preferences on `island`. That grant is 1.2.
    fn started() -> Model {
        let mut model = Model::new(pin(), appender());
        assert_eq!(
            submit(&mut model, &genesis(), &root()),
            Outcome::Accepted { seq: 0 }
        );
        let grants = vec![
            root_capability(),
            verdict(appender()),
            grant(
                public_key(&key(1)),
                &["island"],
                &[Kind::Verse, Kind::Preference],
            ),
        ];
        assert_eq!(
            submit(&mut model, &bootstrap(grants), &root()),
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
        let mut model = Model::new(pin(), appender());
        let writer = key(1);

        assert_eq!(
            submit(&mut model, &verse(&writer, 1, "a"), &writer),
            Outcome::Refused(Why::NotGenesis)
        );
        let other_root = Submission {
            stream: StreamId::from("authority"),
            writer: public_key(&writer),
            counter: 1,
            body: Body::Genesis(Genesis {
                root: public_key(&writer),
            }),
        };
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
        for appenders in [vec![], vec![appender(), [7; 32]], vec![pin()]] {
            let mut grants = vec![root_capability()];
            grants.extend(appenders.into_iter().map(verdict));
            assert_eq!(
                submit(&mut model, &bootstrap(grants), &root()),
                Outcome::Refused(Why::BootstrapAppender)
            );
        }
        // No root capability, or two.
        for grants in [
            vec![verdict(appender())],
            vec![root_capability(), root_capability(), verdict(appender())],
            vec![
                grant(pin(), &["authority"], &[Kind::Grant]),
                verdict(appender()),
            ],
        ] {
            assert_eq!(
                submit(&mut model, &bootstrap(grants), &root()),
                Outcome::Refused(Why::BootstrapRoot)
            );
        }
        // A third grant is an ordinary one.
        let granting = grant(public_key(&writer), &["authority"], &[Kind::Grant]);
        assert_eq!(
            submit(
                &mut model,
                &bootstrap(vec![root_capability(), verdict(appender()), granting]),
                &root()
            ),
            Outcome::Refused(Why::Ungrantable)
        );
        assert_eq!(
            submit(
                &mut model,
                &bootstrap(vec![root_capability(), verdict([7; 32])]),
                &root()
            ),
            Outcome::Refused(Why::WrongAppender)
        );
        assert_eq!(
            submit(
                &mut model,
                &bootstrap(vec![root_capability(), verdict(appender())]),
                &root()
            ),
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
            assert_eq!(
                submit(&mut model, &verse_on(stream, &writer, 1, "a"), &writer),
                Outcome::Refused(Why::Stream),
                "a verse on {stream}"
            );
        }
        let policy = Body::Policy(Policy::default());
        assert_eq!(
            submit(&mut model, &by_root(3, "island", policy.clone()), &root()),
            Outcome::Refused(Why::Stream)
        );
        assert_eq!(
            submit(&mut model, &by_root(3, "policy", policy), &root()),
            Outcome::Accepted { seq: 2 }
        );
        assert_eq!(model.policy(), Some((2, &Policy::default())));
    }

    #[test]
    fn a_write_needs_a_grant_and_grants_narrow_and_end() {
        let mut model = started();
        let (writer, stranger) = (key(1), key(2));
        let authority = |counter, body| by_root(counter, "authority", body);

        // Outside the grant: another stream, another writer.
        assert_eq!(
            submit(&mut model, &verse_on("flare", &writer, 1, "a"), &writer),
            Outcome::Refused(Why::NoGrant)
        );
        assert_eq!(
            submit(&mut model, &verse(&stranger, 1, "a"), &stranger),
            Outcome::Refused(Why::NoGrant)
        );
        // Only root grants.
        let by_writer = Submission {
            writer: public_key(&writer),
            counter: 1,
            ..authority(
                0,
                Body::Grant(grant(public_key(&stranger), &["island"], &[Kind::Verse])),
            )
        };
        assert_eq!(
            submit(&mut model, &by_writer, &writer),
            Outcome::Refused(Why::NoGrant)
        );

        // A new grant for the stranger, at entry 2.
        let fresh = grant(public_key(&stranger), &["island", "flare"], &[Kind::Verse]);
        assert_eq!(
            submit(&mut model, &authority(3, Body::Grant(fresh)), &root()),
            Outcome::Accepted { seq: 2 }
        );
        let fresh_id = GrantId::new(2, 0);

        // Narrowing: nothing added, same holder, a grant in force.
        let narrowing = |holder: &SigningKey, scope: &[&str], id| {
            let mut narrow = grant(public_key(holder), scope, &[Kind::Verse]);
            narrow.narrows = Some(id);
            Body::Grant(narrow)
        };
        for (body, why) in [
            (
                narrowing(&stranger, &["island", "taso"], fresh_id),
                Why::Wider,
            ),
            (narrowing(&writer, &["island"], fresh_id), Why::OtherHolder),
            (
                narrowing(&stranger, &["island"], GrantId::new(9, 0)),
                Why::UnknownGrant,
            ),
            (narrowing(&key(0x41), &[], GrantId::new(1, 1)), Why::Guarded),
        ] {
            assert_eq!(
                submit(&mut model, &authority(4, body), &root()),
                Outcome::Refused(why)
            );
        }
        assert_eq!(
            submit(
                &mut model,
                &authority(4, narrowing(&stranger, &["island"], fresh_id)),
                &root()
            ),
            Outcome::Accepted { seq: 3 }
        );
        let narrow_id = GrantId::new(3, 0);
        assert_eq!(
            submit(
                &mut model,
                &authority(5, narrowing(&stranger, &[], fresh_id)),
                &root()
            ),
            Outcome::Refused(Why::EndedGrant)
        );
        assert_eq!(
            submit(&mut model, &verse_on("flare", &stranger, 1, "a"), &stranger),
            Outcome::Refused(Why::NoGrant)
        );
        assert_eq!(
            submit(&mut model, &verse(&stranger, 1, "a"), &stranger),
            Outcome::Accepted { seq: 4 }
        );

        // Revoke: the next submission is refused.
        let revoke = |id| Body::Revoke(Revoke { grant: id });
        assert_eq!(
            submit(&mut model, &authority(5, revoke(narrow_id)), &root()),
            Outcome::Accepted { seq: 5 }
        );
        assert_eq!(
            submit(&mut model, &verse(&stranger, 2, "b"), &stranger),
            Outcome::Refused(Why::NoGrant)
        );
        assert_eq!(
            submit(&mut model, &authority(6, revoke(narrow_id)), &root()),
            Outcome::Refused(Why::EndedGrant)
        );

        let live: BTreeSet<GrantId> = model
            .grants()
            .into_iter()
            .filter(|lease| lease.live)
            .map(|lease| lease.id)
            .collect();
        assert_eq!(
            live,
            BTreeSet::from([GrantId::new(1, 0), GrantId::new(1, 1), GrantId::new(1, 2)])
        );
    }

    #[test]
    fn the_two_protected_grants_stay() {
        let mut model = started();
        let authority = |counter, body| by_root(counter, "authority", body);
        let (root_id, appender_id) = (GrantId::new(1, 0), GrantId::new(1, 1));

        for id in [root_id, appender_id] {
            assert_eq!(
                submit(
                    &mut model,
                    &authority(3, Body::Revoke(Revoke { grant: id })),
                    &root()
                ),
                Outcome::Refused(Why::Guarded)
            );
        }
        // Root may give up the policy right, not the right to grant.
        let narrowed = |rights: &[Kind], id| {
            let mut narrow = grant(pin(), &["authority", "policy"], rights);
            narrow.narrows = Some(id);
            Body::Grant(narrow)
        };
        assert_eq!(
            submit(
                &mut model,
                &authority(3, narrowed(&[Kind::Policy, Kind::Grant], root_id)),
                &root()
            ),
            Outcome::Refused(Why::Guarded)
        );
        assert_eq!(
            submit(
                &mut model,
                &authority(3, narrowed(&[Kind::Grant, Kind::Revoke], root_id)),
                &root()
            ),
            Outcome::Accepted { seq: 2 }
        );
        // The capability moved to 2.0 and is protected there.
        let moved = GrantId::new(2, 0);
        assert_eq!(
            submit(
                &mut model,
                &authority(4, Body::Revoke(Revoke { grant: moved })),
                &root()
            ),
            Outcome::Refused(Why::Guarded)
        );
        assert_eq!(
            submit(
                &mut model,
                &by_root(4, "policy", Body::Policy(Policy::default())),
                &root()
            ),
            Outcome::Refused(Why::NoGrant)
        );
    }

    fn policy(counter: u64, invariants: &[Invariant]) -> Submission {
        let body = Body::Policy(Policy {
            evaluator: "test".to_owned(),
            invariants: invariants.to_vec(),
        });
        by_root(counter, "policy", body)
    }

    /// A verse by `key(1)` with any evidence byte and any number of refs.
    fn raw(counter: u64, evidence: u8, text: &str, refs: usize) -> Submission {
        let mut submission = verse(&key(1), counter, text);
        submission.body = Body::Verse(Verse {
            evidence,
            text: text.to_owned(),
            refs: vec![[9; 32]; refs],
        });
        submission
    }

    const NAMES: Invariant = Invariant::VerseNamesEvidence;
    const HAS_REF: Invariant = Invariant::ArtefactHasRef;

    const fn max(bytes: u32) -> Invariant {
        Invariant::VerseTextMax { bytes }
    }

    #[test]
    fn the_gate_refuses_with_a_verdict_that_uses_the_counter() {
        let mut model = started();
        let writer = key(1);
        // Before any policy, nothing is refused.
        assert_eq!(
            submit(&mut model, &raw(1, 200, "anything", 0), &writer),
            Outcome::Accepted { seq: 2 }
        );
        assert_eq!(
            submit(&mut model, &policy(3, &[NAMES, max(10), HAS_REF]), &root()),
            Outcome::Accepted { seq: 3 }
        );

        // Each invariant refuses what it names; the first broken one is
        // the reason.
        let cases = [
            (raw(2, 4, "x", 0), Refusal::Invariant(NAMES)),
            (raw(3, 0, "eleven char", 0), Refusal::Invariant(max(10))),
            (raw(4, 1, "x", 0), Refusal::Invariant(HAS_REF)),
            (raw(5, 9, "eleven char", 0), Refusal::Invariant(NAMES)),
        ];
        for ((refused, reason), seq) in cases.iter().zip(4u64..) {
            assert_eq!(
                submit(&mut model, refused, &writer),
                Outcome::Verdict {
                    seq,
                    reason: *reason
                }
            );
        }
        assert_eq!(
            submit(&mut model, &raw(6, 1, "ten chars!", 1), &writer),
            Outcome::Accepted { seq: 8 }
        );

        // The verdict is the appender's entry on the gate stream, holding
        // the whole submission; both counters moved.
        let entry = &model.log()[4];
        assert_eq!(entry.stream, StreamId::from("gate"));
        assert_eq!((entry.writer, entry.counter), (appender(), 1));
        let Body::Verdict(verdict) = &entry.body else {
            panic!("entry 4 is not a verdict");
        };
        assert_eq!(*verdict.rejected, cases[0].0);
        assert_eq!(verdict.rejected.verify(&verdict.sig_writer), Ok(()));
        assert_eq!(model.counter_of(&appender()), 4);
        assert_eq!(model.counter_of(&public_key(&writer)), 6);
        assert_eq!(model.stream(&StreamId::from("gate")), Some((4, 7)));
        assert_eq!(model.stream(&StreamId::from("island")), Some((2, 8)));
    }

    #[test]
    fn a_refused_submission_resent_gets_its_refusal_again() {
        let mut model = started();
        let writer = key(1);
        submit(&mut model, &policy(3, &[NAMES]), &root());
        let refused = raw(1, 4, "x", 0);
        let reason = Refusal::Invariant(NAMES);
        assert_eq!(
            submit(&mut model, &refused, &writer),
            Outcome::Verdict { seq: 3, reason }
        );
        assert_eq!(
            submit(&mut model, &refused, &writer),
            Outcome::VerdictRetry { seq: 3, reason }
        );
        assert_eq!(
            submit(&mut model, &raw(1, 0, "x", 0), &writer),
            Outcome::Refused(Why::CounterConflict)
        );
        assert_eq!(model.log().len(), 4);
        // Once the writer has moved on, the refused one is an old counter.
        assert_eq!(
            submit(&mut model, &raw(2, 0, "x", 0), &writer),
            Outcome::Accepted { seq: 4 }
        );
        assert_eq!(
            submit(&mut model, &refused, &writer),
            Outcome::Refused(Why::CounterOld)
        );
    }

    #[test]
    fn a_policy_only_tightens() {
        let mut model = started();
        assert_eq!(
            submit(&mut model, &policy(3, &[NAMES, max(100)]), &root()),
            Outcome::Accepted { seq: 2 }
        );
        // Dropping an invariant, raising the bound, dropping the bound.
        let weaker = [&[max(100)][..], &[NAMES, max(101)], &[NAMES], &[]];
        for (invariants, counter) in weaker.into_iter().zip(4u64..) {
            assert_eq!(
                submit(&mut model, &policy(counter, invariants), &root()),
                Outcome::Verdict {
                    seq: counter - 1,
                    reason: Refusal::WeakerPolicy
                },
                "{invariants:?}"
            );
        }
        assert_eq!(model.policy().map(|(seq, _)| seq), Some(2));
        // The same again, a lower bound, one more invariant: all allowed,
        // in any order.
        for (invariants, counter) in [
            &[NAMES, max(100)][..],
            &[max(40), NAMES],
            &[HAS_REF, NAMES, max(40)],
        ]
        .into_iter()
        .zip(8u64..)
        {
            assert_eq!(
                submit(&mut model, &policy(counter, invariants), &root()),
                Outcome::Accepted { seq: counter - 1 },
                "{invariants:?}"
            );
        }
        assert_eq!(model.policy().map(|(seq, _)| seq), Some(9));
    }

    #[test]
    fn the_appender_key_writes_nothing_itself() {
        let appender_key = key(0x41);
        let mut model = Model::new(pin(), appender());
        submit(&mut model, &genesis(), &root());
        // Even with a grant to write verses.
        let grants = vec![
            root_capability(),
            verdict(appender()),
            grant(appender(), &["island"], &[Kind::Verse]),
            grant(public_key(&key(1)), &["island"], &[Kind::Verse]),
        ];
        submit(&mut model, &bootstrap(grants), &root());
        assert_eq!(
            submit(&mut model, &verse(&appender_key, 1, "a"), &appender_key),
            Outcome::Refused(Why::AppenderWrites)
        );

        // A verdict from outside is refused, whoever signs it.
        submit(&mut model, &policy(3, &[NAMES]), &root());
        let rejected = raw(1, 4, "x", 0);
        for outsider in [key(1), key(0x41)] {
            let forged = Submission {
                stream: StreamId::from("gate"),
                writer: public_key(&outsider),
                counter: 1,
                body: Body::Verdict(Verdict {
                    sig_writer: sign_hash(&key(1), &rejected.hash()),
                    rejected: Box::new(rejected.clone()),
                    reason: Refusal::Invariant(NAMES),
                }),
            };
            assert_eq!(
                submit(&mut model, &forged, &outsider),
                Outcome::Refused(Why::Verdict)
            );
        }
        // The real one, and a resend of the appender's own submission.
        assert_eq!(
            submit(&mut model, &rejected, &key(1)),
            Outcome::Verdict {
                seq: 3,
                reason: Refusal::Invariant(NAMES)
            }
        );
        let own = model.log()[3].clone();
        assert_eq!(
            submit(&mut model, &own, &appender_key),
            Outcome::Retry { seq: 3 }
        );
    }
}
