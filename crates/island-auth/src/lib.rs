//! Island authority: who may write what, read from the log itself
//! (docs/spec.md, "Write path and gate", step 3).
//!
//! There are no tokens. A writer's signature is its credential, and its
//! authority is whatever grants in force name its key. Grant state is
//! built from three kinds of entries: the bootstrap entry, `grant` and
//! `revoke`.
//!
//! - A grant covers a write if its holder is the writer, its scope holds
//!   the stream and its rights hold the kind.
//! - A grant with `narrows` replaces the grant it names: same holder,
//!   scope and rights subsets of it. The old grant ends; the new one
//!   takes its role.
//! - Widening is only ever a new grant.
//! - A `revoke` ends a grant from its own entry on.
//!
//! Two grants are protected, by role. The root capability can never be
//! revoked, and a narrowing of it must keep the right to write grants and
//! revokes on the authority stream, so a log can never be left with nobody
//! able to grant. The appender's grant can be neither narrowed nor
//! revoked: its holder is the key every entry's seal is checked against.
//!
//! Only the root capability carries the right to write grants. An
//! ordinary grant may carry `policy`, `verse`, `preference`, and from
//! slice 2 `receipt`, `access`, `approval` and `dispatch`; nothing else,
//! so nobody but root can ever grant.
//!
//! Pure: values in, values out.

#![warn(missing_docs)]

use std::fmt;

use island_core::{
    Body, Ended, Grant, GrantId, Held, Kind, PublicKey, Revoke, Role, State, StreamId, Submission,
};

/// The kinds an ordinary grant may carry.
pub const GRANTABLE: [Kind; 7] = [
    Kind::Policy,
    Kind::Verse,
    Kind::Preference,
    Kind::Receipt,
    Kind::Access,
    Kind::Approval,
    Kind::Dispatch,
];

/// Why authority says no. Nothing is written to the log for any of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// No grant in force lets this writer write this kind to this stream.
    NoGrant,
    /// The grant named by `narrows` or `revoke` does not exist.
    UnknownGrant(GrantId),
    /// The grant named has already been revoked or replaced.
    Ended(GrantId),
    /// A narrowing names another holder than the grant it replaces.
    OtherHolder,
    /// A narrowing's scope or rights are not subsets of the grant it
    /// replaces: a widening.
    Wider,
    /// The grant carries a kind an ordinary grant may not.
    Ungrantable(Kind),
    /// The grant named is protected: the appender's, or the root
    /// capability being revoked or narrowed below the right to grant.
    Guarded(GrantId),
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoGrant => {
                f.write_str("no grant in force lets this key write that kind to that stream")
            }
            Self::UnknownGrant(id) => write!(f, "there is no grant {id}"),
            Self::Ended(id) => write!(f, "grant {id} is no longer in force"),
            Self::OtherHolder => {
                f.write_str("a narrowing must keep the holder of the grant it replaces")
            }
            Self::Wider => f.write_str(
                "a narrowing may only remove streams and kinds; widening takes a new grant",
            ),
            Self::Ungrantable(kind) => write!(
                f,
                "a grant may not carry the right to write {} entries",
                format!("{kind:?}").to_lowercase()
            ),
            Self::Guarded(id) => write!(f, "grant {id} is protected"),
        }
    }
}

impl std::error::Error for Denied {}

/// Why a bootstrap entry's grants do not start a log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapError {
    /// Not exactly one grant carries the `verdict` right, or root holds
    /// it.
    Appender,
    /// Not exactly one grant held by root carries the right to write
    /// grants and revokes on the authority stream.
    Root,
    /// Another of its grants carries a kind an ordinary grant may not.
    Ungrantable(Kind),
}

fn is_root_capability(grant: &Grant, root: &PublicKey) -> bool {
    grant.holder == *root && keeps_granting(grant)
}

/// Whether a grant lets its holder write grants and revokes on the
/// authority stream.
fn keeps_granting(grant: &Grant) -> bool {
    grant.rights.contains(&Kind::Grant)
        && grant.rights.contains(&Kind::Revoke)
        && grant.scope.contains(&StreamId::from(StreamId::AUTHORITY))
}

fn ungrantable(grant: &Grant) -> Option<Kind> {
    grant
        .rights
        .iter()
        .copied()
        .find(|kind| !GRANTABLE.contains(kind))
}

/// The role of each grant in a bootstrap entry signed by `root`, in order.
///
/// Exactly one grant is the appender's: the one with the `verdict` right,
/// held by a key other than root. Exactly one is the root capability.
/// Any others are ordinary grants.
pub fn bootstrap(grants: &[Grant], root: &PublicKey) -> Result<Vec<Role>, BootstrapError> {
    let appender = |grant: &Grant| grant.rights.contains(&Kind::Verdict);
    let mut appenders = grants.iter().filter(|grant| appender(grant));
    match (appenders.next(), appenders.next()) {
        (Some(grant), None) if grant.holder != *root => {}
        _ => return Err(BootstrapError::Appender),
    }
    if grants
        .iter()
        .filter(|grant| is_root_capability(grant, root))
        .count()
        != 1
    {
        return Err(BootstrapError::Root);
    }
    grants
        .iter()
        .map(|grant| {
            if appender(grant) {
                Ok(Role::Appender)
            } else if is_root_capability(grant, root) {
                Ok(Role::Root)
            } else {
                match ungrantable(grant) {
                    Some(kind) => Err(BootstrapError::Ungrantable(kind)),
                    None => Ok(Role::Ordinary),
                }
            }
        })
        .collect()
}

/// Whether a grant in force lets `writer` write `kind` to `stream`.
pub fn covers(state: &State, writer: &PublicKey, stream: &StreamId, kind: Kind) -> bool {
    state.grants.values().any(|held| {
        held.in_force()
            && held.grant.holder == *writer
            && held.grant.scope.contains(stream)
            && held.grant.rights.contains(&kind)
    })
}

/// Decide whether authority lets `submission` into the log as the next
/// entry after `state`.
///
/// For any kind: a grant in force must cover the write. For a `grant` or
/// a `revoke`, what it does to the grant state must be allowed as well.
/// The genesis and bootstrap entries are not judged here; they come
/// before any grant exists and have rules of their own.
pub fn authorize(state: &State, submission: &Submission) -> Result<(), Denied> {
    if !covers(
        state,
        &submission.writer,
        &submission.stream,
        submission.kind(),
    ) {
        return Err(Denied::NoGrant);
    }
    match &submission.body {
        Body::Grant(grant) => check_grant(state, grant),
        Body::Revoke(revoke) => check_revoke(state, revoke),
        _ => Ok(()),
    }
}

fn in_force(state: &State, id: GrantId) -> Result<&Held, Denied> {
    let held = state.grants.get(&id).ok_or(Denied::UnknownGrant(id))?;
    if held.in_force() {
        Ok(held)
    } else {
        Err(Denied::Ended(id))
    }
}

fn check_grant(state: &State, grant: &Grant) -> Result<(), Denied> {
    let Some(id) = grant.narrows else {
        // A new grant: the only way authority ever widens.
        return ungrantable(grant).map_or(Ok(()), |kind| Err(Denied::Ungrantable(kind)));
    };
    let old = in_force(state, id)?;
    if old.role == Role::Appender {
        return Err(Denied::Guarded(id));
    }
    if grant.holder != old.grant.holder {
        return Err(Denied::OtherHolder);
    }
    if !grant.scope.is_subset(&old.grant.scope) || !grant.rights.is_subset(&old.grant.rights) {
        return Err(Denied::Wider);
    }
    if old.role == Role::Root && !keeps_granting(grant) {
        return Err(Denied::Guarded(id));
    }
    Ok(())
}

fn check_revoke(state: &State, revoke: &Revoke) -> Result<(), Denied> {
    let held = in_force(state, revoke.grant)?;
    if held.role == Role::Ordinary {
        Ok(())
    } else {
        Err(Denied::Guarded(revoke.grant))
    }
}

/// Take an accepted entry into the grant state. `seq` is its position.
///
/// The entry must already have passed [`bootstrap`] (for the bootstrap
/// entry) or [`authorize`] (for any other); what does not fit is ignored
/// instead of applied halfway.
pub fn apply(state: &mut State, seq: u64, submission: &Submission) {
    match &submission.body {
        Body::Bootstrap(body) => {
            let Ok(roles) = bootstrap(&body.grants, &submission.writer) else {
                return;
            };
            for ((index, grant), role) in (0u32..).zip(&body.grants).zip(roles) {
                state.grants.insert(
                    GrantId::new(seq, index),
                    Held {
                        grant: grant.clone(),
                        role,
                        ended: None,
                    },
                );
            }
        }
        Body::Grant(grant) => {
            let id = GrantId::new(seq, 0);
            let mut role = Role::Ordinary;
            if let Some(old) = grant.narrows.and_then(|old| state.grants.get_mut(&old)) {
                old.ended = Some(Ended {
                    at: seq,
                    by: Some(id),
                });
                role = old.role;
            }
            state.grants.insert(
                id,
                Held {
                    grant: grant.clone(),
                    role,
                    ended: None,
                },
            );
        }
        Body::Revoke(revoke) => {
            if let Some(held) = state.grants.get_mut(&revoke.grant) {
                held.ended = Some(Ended { at: seq, by: None });
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use island_core::{Bootstrap, Evidence, Verse};

    use super::*;

    const ROOT: PublicKey = [0x52; 32];
    const APPENDER: PublicKey = [0x41; 32];
    const DAILY: PublicKey = [0x44; 32];
    const OTHER: PublicKey = [0x4f; 32];

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
            ROOT,
            &["authority", "policy"],
            &[Kind::Policy, Kind::Grant, Kind::Revoke],
        )
    }

    fn appender_grant() -> Grant {
        grant(APPENDER, &["gate"], &[Kind::Verdict])
    }

    fn on(stream: &str, writer: PublicKey, body: Body) -> Submission {
        Submission {
            stream: StreamId::from(stream),
            writer,
            counter: 1,
            body,
        }
    }

    fn verse(stream: &str, writer: PublicKey) -> Submission {
        on(
            stream,
            writer,
            Body::Verse(Verse::new(Evidence::Decision, "a verse", vec![])),
        )
    }

    /// The state after a bootstrap entry at seq 1.
    fn started() -> State {
        let mut state = State::default();
        let bootstrap = on(
            "authority",
            ROOT,
            Body::Bootstrap(Bootstrap {
                grants: vec![root_capability(), appender_grant()],
            }),
        );
        apply(&mut state, 1, &bootstrap);
        state
    }

    /// Authorize, then apply at `seq`, as the log does.
    fn accept(state: &mut State, seq: u64, submission: &Submission) -> Result<(), Denied> {
        authorize(state, submission)?;
        apply(state, seq, submission);
        Ok(())
    }

    fn root_grants(grant: Grant) -> Submission {
        on("authority", ROOT, Body::Grant(grant))
    }

    fn root_revokes(id: GrantId) -> Submission {
        on("authority", ROOT, Body::Revoke(Revoke { grant: id }))
    }

    const ROOT_ID: GrantId = GrantId::new(1, 0);
    const APPENDER_ID: GrantId = GrantId::new(1, 1);

    // ── Bootstrap ───────────────────────────────────────────────────

    #[test]
    fn a_bootstrap_gives_each_grant_its_role_and_id() {
        let daily = grant(DAILY, &["island"], &[Kind::Verse]);
        let roles = bootstrap(&[root_capability(), appender_grant(), daily], &ROOT);
        assert_eq!(roles, Ok(vec![Role::Root, Role::Appender, Role::Ordinary]));

        let state = started();
        assert_eq!(state.grants[&ROOT_ID].role, Role::Root);
        assert_eq!(state.grants[&APPENDER_ID].role, Role::Appender);
        assert!(state.grants.values().all(Held::in_force));
    }

    #[test]
    fn a_bootstrap_needs_one_appender_and_one_root_capability() {
        let check = |grants: &[Grant]| bootstrap(grants, &ROOT);
        let second_appender = grant(OTHER, &["gate"], &[Kind::Verdict]);
        let root_as_appender = grant(ROOT, &["gate"], &[Kind::Verdict]);

        assert_eq!(check(&[root_capability()]), Err(BootstrapError::Appender));
        assert_eq!(
            check(&[root_capability(), appender_grant(), second_appender]),
            Err(BootstrapError::Appender)
        );
        assert_eq!(
            check(&[root_capability(), root_as_appender]),
            Err(BootstrapError::Appender)
        );

        assert_eq!(check(&[appender_grant()]), Err(BootstrapError::Root));
        assert_eq!(
            check(&[root_capability(), root_capability(), appender_grant()]),
            Err(BootstrapError::Root)
        );
        // Root without the right to revoke, or without the authority
        // stream, is not a root capability.
        let no_revoke = grant(ROOT, &["authority"], &[Kind::Grant]);
        assert_eq!(
            check(&[no_revoke, appender_grant()]),
            Err(BootstrapError::Root)
        );
        let no_stream = grant(ROOT, &["policy"], &[Kind::Grant, Kind::Revoke]);
        assert_eq!(
            check(&[no_stream, appender_grant()]),
            Err(BootstrapError::Root)
        );

        // Any other grant in the bootstrap is an ordinary one.
        let grants_granting = grant(DAILY, &["authority"], &[Kind::Grant]);
        assert_eq!(
            check(&[root_capability(), appender_grant(), grants_granting]),
            Err(BootstrapError::Ungrantable(Kind::Grant))
        );
    }

    // ── Covering a write ────────────────────────────────────────────

    #[test]
    fn a_write_needs_a_grant_for_its_writer_stream_and_kind() {
        let mut state = started();
        assert_eq!(
            authorize(&state, &verse("island", DAILY)),
            Err(Denied::NoGrant)
        );

        let daily = grant(DAILY, &["island", "flare"], &[Kind::Verse]);
        assert_eq!(accept(&mut state, 2, &root_grants(daily)), Ok(()));

        assert_eq!(authorize(&state, &verse("island", DAILY)), Ok(()));
        assert_eq!(authorize(&state, &verse("flare", DAILY)), Ok(()));
        // Another stream, another kind, another writer.
        assert_eq!(
            authorize(&state, &verse("taso", DAILY)),
            Err(Denied::NoGrant)
        );
        let preference = on(
            "island",
            DAILY,
            Body::Preference(island_core::Preference {
                key: "k".into(),
                value: "v".into(),
            }),
        );
        assert_eq!(authorize(&state, &preference), Err(Denied::NoGrant));
        assert_eq!(
            authorize(&state, &verse("island", OTHER)),
            Err(Denied::NoGrant)
        );
    }

    #[test]
    fn only_the_root_capability_writes_grants() {
        let mut state = started();
        let daily = grant(DAILY, &["island"], &[Kind::Verse]);
        assert_eq!(accept(&mut state, 2, &root_grants(daily.clone())), Ok(()));

        // The daily key tries to grant: it holds no right to.
        let by_daily = on(
            "authority",
            DAILY,
            Body::Grant(grant(OTHER, &["island"], &[Kind::Verse])),
        );
        assert_eq!(authorize(&state, &by_daily), Err(Denied::NoGrant));

        // And root cannot hand that right out.
        for kind in [
            Kind::Grant,
            Kind::Revoke,
            Kind::Verdict,
            Kind::Genesis,
            Kind::Bootstrap,
        ] {
            let handing_out = grant(DAILY, &["authority"], &[Kind::Verse, kind]);
            assert_eq!(
                authorize(&state, &root_grants(handing_out)),
                Err(Denied::Ungrantable(kind))
            );
        }
        // Policy, verse and preference are grantable.
        let all = grant(DAILY, &["policy", "island"], &GRANTABLE);
        assert_eq!(authorize(&state, &root_grants(all)), Ok(()));
    }

    // ── Narrowing ───────────────────────────────────────────────────

    #[test]
    fn a_narrowing_replaces_the_grant_and_only_shrinks_it() {
        let mut state = started();
        let wide = grant(
            DAILY,
            &["island", "flare"],
            &[Kind::Verse, Kind::Preference],
        );
        assert_eq!(accept(&mut state, 2, &root_grants(wide)), Ok(()));
        let wide_id = GrantId::new(2, 0);

        let mut narrow = grant(DAILY, &["island"], &[Kind::Verse]);
        narrow.narrows = Some(wide_id);
        assert_eq!(accept(&mut state, 3, &root_grants(narrow)), Ok(()));
        let narrow_id = GrantId::new(3, 0);

        // The old grant ended at entry 3, replaced by the new one.
        assert_eq!(
            state.grants[&wide_id].ended,
            Some(Ended {
                at: 3,
                by: Some(narrow_id)
            })
        );
        assert!(state.grants[&narrow_id].in_force());
        assert_eq!(authorize(&state, &verse("island", DAILY)), Ok(()));
        assert_eq!(
            authorize(&state, &verse("flare", DAILY)),
            Err(Denied::NoGrant)
        );

        // A widening is refused, in scope and in rights.
        for (scope, rights) in [
            (&["island", "flare"][..], &[Kind::Verse][..]),
            (&["island"][..], &[Kind::Verse, Kind::Preference][..]),
        ] {
            let mut wider = grant(DAILY, scope, rights);
            wider.narrows = Some(narrow_id);
            assert_eq!(authorize(&state, &root_grants(wider)), Err(Denied::Wider));
        }
        // So is passing the grant to another key.
        let mut handed_on = grant(OTHER, &["island"], &[Kind::Verse]);
        handed_on.narrows = Some(narrow_id);
        assert_eq!(
            authorize(&state, &root_grants(handed_on)),
            Err(Denied::OtherHolder)
        );

        // The replaced grant cannot be narrowed again; an unknown one
        // neither.
        let mut again = grant(DAILY, &["island"], &[Kind::Verse]);
        again.narrows = Some(wide_id);
        assert_eq!(
            authorize(&state, &root_grants(again.clone())),
            Err(Denied::Ended(wide_id))
        );
        again.narrows = Some(GrantId::new(9, 0));
        assert_eq!(
            authorize(&state, &root_grants(again)),
            Err(Denied::UnknownGrant(GrantId::new(9, 0)))
        );

        // Narrowing to the same thing, or to nothing, is still a narrowing.
        let mut nothing = grant(DAILY, &[], &[]);
        nothing.narrows = Some(narrow_id);
        assert_eq!(accept(&mut state, 4, &root_grants(nothing)), Ok(()));
        assert_eq!(
            authorize(&state, &verse("island", DAILY)),
            Err(Denied::NoGrant)
        );
    }

    // ── Revoking ────────────────────────────────────────────────────

    #[test]
    fn a_revoke_takes_effect_on_the_next_submission() {
        let mut state = started();
        let daily = grant(DAILY, &["island"], &[Kind::Verse]);
        assert_eq!(accept(&mut state, 2, &root_grants(daily)), Ok(()));
        let id = GrantId::new(2, 0);
        assert_eq!(authorize(&state, &verse("island", DAILY)), Ok(()));

        assert_eq!(accept(&mut state, 3, &root_revokes(id)), Ok(()));
        assert_eq!(state.grants[&id].ended, Some(Ended { at: 3, by: None }));
        assert_eq!(
            authorize(&state, &verse("island", DAILY)),
            Err(Denied::NoGrant)
        );

        // Revoking it again, or something that never was, is refused.
        assert_eq!(authorize(&state, &root_revokes(id)), Err(Denied::Ended(id)));
        assert_eq!(
            authorize(&state, &root_revokes(GrantId::new(8, 0))),
            Err(Denied::UnknownGrant(GrantId::new(8, 0)))
        );
        // A new grant brings the key back: widening is a new grant.
        let back = grant(DAILY, &["island"], &[Kind::Verse]);
        assert_eq!(accept(&mut state, 4, &root_grants(back)), Ok(()));
        assert_eq!(authorize(&state, &verse("island", DAILY)), Ok(()));
    }

    // ── The two protected grants ────────────────────────────────────

    #[test]
    fn the_root_capability_cannot_be_revoked_or_narrowed_below_granting() {
        let mut state = started();
        assert_eq!(
            authorize(&state, &root_revokes(ROOT_ID)),
            Err(Denied::Guarded(ROOT_ID))
        );

        let narrowed_to = |scope: &[&str], rights: &[Kind], id| {
            let mut narrow = grant(ROOT, scope, rights);
            narrow.narrows = Some(id);
            root_grants(narrow)
        };
        // Giving up the right to grant, to revoke, or the authority stream.
        for (scope, rights) in [
            (
                &["authority", "policy"][..],
                &[Kind::Policy, Kind::Revoke][..],
            ),
            (
                &["authority", "policy"][..],
                &[Kind::Policy, Kind::Grant][..],
            ),
            (
                &["policy"][..],
                &[Kind::Policy, Kind::Grant, Kind::Revoke][..],
            ),
            (&[][..], &[][..]),
        ] {
            assert_eq!(
                authorize(&state, &narrowed_to(scope, rights, ROOT_ID)),
                Err(Denied::Guarded(ROOT_ID))
            );
        }

        // Giving up the policy right is allowed, and the guard follows the
        // capability to its new id.
        let narrow = narrowed_to(&["authority"], &[Kind::Grant, Kind::Revoke], ROOT_ID);
        assert_eq!(accept(&mut state, 2, &narrow), Ok(()));
        let new_id = GrantId::new(2, 0);
        assert_eq!(state.grants[&new_id].role, Role::Root);
        assert_eq!(
            authorize(&state, &root_revokes(new_id)),
            Err(Denied::Guarded(new_id))
        );
        assert_eq!(
            authorize(&state, &narrowed_to(&["authority"], &[Kind::Grant], new_id)),
            Err(Denied::Guarded(new_id))
        );

        // Root can no longer write policy entries ...
        let policy = on("policy", ROOT, Body::Policy(island_core::Policy::default()));
        assert_eq!(authorize(&state, &policy), Err(Denied::NoGrant));
        // ... until it grants itself that right again: an ordinary grant.
        let regrant = grant(ROOT, &["policy"], &[Kind::Policy]);
        assert_eq!(accept(&mut state, 3, &root_grants(regrant)), Ok(()));
        assert_eq!(authorize(&state, &policy), Ok(()));
        assert_eq!(state.grants[&GrantId::new(3, 0)].role, Role::Ordinary);
    }

    #[test]
    fn the_appenders_grant_cannot_be_narrowed_or_revoked() {
        let state = started();
        assert_eq!(
            authorize(&state, &root_revokes(APPENDER_ID)),
            Err(Denied::Guarded(APPENDER_ID))
        );
        let mut narrow = grant(APPENDER, &[], &[]);
        narrow.narrows = Some(APPENDER_ID);
        assert_eq!(
            authorize(&state, &root_grants(narrow)),
            Err(Denied::Guarded(APPENDER_ID))
        );
        let mut same = appender_grant();
        same.narrows = Some(APPENDER_ID);
        assert_eq!(
            authorize(&state, &root_grants(same)),
            Err(Denied::Guarded(APPENDER_ID))
        );
    }

    #[test]
    fn scope_and_rights_are_sets() {
        // Only here to pin the meaning of "subset" on empty sets.
        let empty: BTreeSet<Kind> = BTreeSet::new();
        assert!(empty.is_subset(&BTreeSet::from([Kind::Verse])));
        assert!(empty.is_subset(&empty));
    }
}
