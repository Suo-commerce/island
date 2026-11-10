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
//! Covered so far: the writer's signature, the counter and retries, the
//! genesis and bootstrap rules, which kind belongs on which stream, and
//! grants: who may write what, narrowing, revoking, and the two protected
//! grants. The gate joins with build step 6.

#![warn(missing_docs)]

use island_core::{Body, Grant, GrantId, Kind, Policy, PublicKey, Signature, StreamId, Submission};

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
    /// A verdict from outside: only the appender writes them, and not
    /// before the gate exists.
    Verdict,
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

        Outcome::Accepted { seq }
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

    /// Judge a submission and, if it is accepted, add it to the log.
    pub fn submit(&mut self, submission: &Submission, sig_writer: &Signature) -> Outcome {
        let outcome = self.judge(submission, sig_writer);
        if matches!(outcome, Outcome::Accepted { .. }) {
            self.log.push(submission.clone());
        }
        outcome
    }
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
    grant
        .rights
        .iter()
        .all(|kind| matches!(kind, Kind::Policy | Kind::Verse | Kind::Preference))
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
}
