//! Who is calling: a connection's peer uid, looked up in Taso's Warden
//! config (docs/slice-2.md, "The island socket"). A Vessel in uid slot `n`
//! runs as `host_uid_base + n·65536 .. +65536` *(ran: S2 run h)*; its fork
//! children take the slots its `forks` entry lists, and are named
//! `<subject>-fork-<slot>` in its namespace (Taso `ForksConf::child_name`).
//! A Vessel's id is Taso's own derivation, `taso_core::record::vessel_id`,
//! so the id `island-socket` writes in an `access` entry is the one
//! `island-sender` reads from the Warden's sender socket name.
//!
//! A uid that is no Vessel's is no caller: the request is refused.

use std::path::Path;

use island_core::VesselId;
use serde_json::Value;
use taso_core::record::{Namespace, vessel_id};

/// Taso's uid range per slot.
pub const RANGE: u32 = 65_536;

/// A prime or one of its fork children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A Vessel of the config.
    Prime,
    /// A child the Warden made from a prime's `forks` slots.
    Fork,
}

impl Role {
    /// As `whoami` says it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Prime => "prime",
            Self::Fork => "fork",
        }
    }
}

/// One caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// Its Vessel id (Taso's chronicle id).
    pub vessel: VesselId,
    /// Its subject: the config's, or `<prime>-fork-<slot>`.
    pub subject: String,
    /// Prime or fork.
    pub role: Role,
    /// Its uid slot.
    pub slot: u32,
    /// The prime's subject: its own, or its parent's.
    pub prime: String,
    /// The prime's fork slots, in order.
    pub fork_slots: Vec<u32>,
}

/// Every caller a Warden config allows, by first host uid.
#[derive(Clone, Debug, Default)]
pub struct Callers {
    base: u32,
    by_slot: Vec<Caller>,
}

impl Callers {
    /// Read a Warden config.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Self::parse(&text).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Parse a Warden config's JSON. Refuses two callers on one slot: the
    /// peer uid would not say which is calling.
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
        let base = config["runtime"]["host_uid_base"]
            .as_u64()
            .and_then(|b| u32::try_from(b).ok())
            .ok_or("runtime.host_uid_base is missing or not a u32")?;
        let vessels = config["vessels"].as_array().ok_or("no vessels list")?;
        let mut by_slot: Vec<Caller> = Vec::new();
        for (i, vessel) in vessels.iter().enumerate() {
            let what = |field: &str| format!("vessels[{i}].{field}");
            let namespace = vessel["namespace"]
                .as_str()
                .and_then(island_env::keyfile::unhex::<16>)
                .map(Namespace::from_bytes)
                .ok_or_else(|| format!("{} is not 32 hex digits", what("namespace")))?;
            let subject = vessel["subject"]
                .as_str()
                .ok_or_else(|| format!("{} is missing", what("subject")))?
                .to_owned();
            let slot = vessel["uid_slot"]
                .as_u64()
                .and_then(|s| u32::try_from(s).ok())
                .ok_or_else(|| format!("{} is missing", what("uid_slot")))?;
            let fork_slots: Vec<u32> = match vessel["forks"]["slots"].as_array() {
                None => Vec::new(),
                Some(slots) => slots
                    .iter()
                    .map(|s| s.as_u64().and_then(|s| u32::try_from(s).ok()))
                    .collect::<Option<_>>()
                    .ok_or_else(|| format!("{} holds a non-slot", what("forks.slots")))?,
            };
            by_slot.push(Caller {
                vessel: *vessel_id(namespace, subject.as_bytes()).as_bytes(),
                subject: subject.clone(),
                role: Role::Prime,
                slot,
                prime: subject.clone(),
                fork_slots: fork_slots.clone(),
            });
            for &child in &fork_slots {
                let name = format!("{subject}-fork-{child}");
                by_slot.push(Caller {
                    vessel: *vessel_id(namespace, name.as_bytes()).as_bytes(),
                    subject: name,
                    role: Role::Fork,
                    slot: child,
                    prime: subject.clone(),
                    fork_slots: fork_slots.clone(),
                });
            }
        }
        by_slot.sort_by_key(|caller| caller.slot);
        if let Some(pair) = by_slot.windows(2).find(|pair| pair[0].slot == pair[1].slot) {
            return Err(format!(
                "uid slot {} is both {} and {}: a peer uid would not say which is calling",
                pair[0].slot, pair[0].subject, pair[1].subject
            ));
        }
        for caller in &by_slot {
            caller
                .slot
                .checked_mul(RANGE)
                .and_then(|offset| offset.checked_add(base))
                .and_then(|first| first.checked_add(RANGE - 1))
                .ok_or_else(|| {
                    format!("{}: uid slot {} does not fit", caller.subject, caller.slot)
                })?;
        }
        Ok(Self { base, by_slot })
    }

    /// The caller a peer uid belongs to.
    pub fn of_uid(&self, uid: u32) -> Option<&Caller> {
        let slot = uid.checked_sub(self.base)? / RANGE;
        self.by_slot
            .binary_search_by_key(&slot, |caller| caller.slot)
            .ok()
            .map(|i| &self.by_slot[i])
    }

    /// Every caller, by slot.
    pub fn all(&self) -> &[Caller] {
        &self.by_slot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// flare-home's entry on the server (scripts/node/install.sh).
    const NODE: &str = r#"{
        "runtime": { "host_uid_base": 200000 },
        "vessels": [{
            "namespace": "151a4d00000000000000000000000052",
            "subject": "flare-home",
            "uid_slot": 10,
            "forks": { "slots": [11, 12] }
        }]
    }"#;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_id_is_tasos_own() {
        // The Warden on the server named flare-home's sender socket
        // 914c503c980b8f7f1750c664b5743d2c.sock (2026-11-14).
        let callers = Callers::parse(NODE).unwrap();
        let prime = callers.of_uid(200_000 + 10 * RANGE).unwrap();
        assert_eq!(hex(&prime.vessel), "914c503c980b8f7f1750c664b5743d2c");
        assert_eq!((prime.role, prime.slot), (Role::Prime, 10));
    }

    #[test]
    fn a_uid_names_its_slot_and_nothing_else_is_a_caller() {
        let callers = Callers::parse(NODE).unwrap();
        let child = callers.of_uid(200_000 + 12 * RANGE + 4242).unwrap();
        assert_eq!(child.subject, "flare-home-fork-12");
        assert_eq!(
            (child.role, child.prime.as_str()),
            (Role::Fork, "flare-home")
        );
        assert_ne!(
            child.vessel,
            callers.of_uid(200_000 + 10 * RANGE).unwrap().vessel
        );
        assert!(callers.of_uid(0).is_none(), "root");
        assert!(callers.of_uid(1000).is_none(), "a user");
        assert!(
            callers.of_uid(200_000 + 13 * RANGE).is_none(),
            "no Vessel's slot"
        );
        assert!(callers.of_uid(200_000 + 9 * RANGE + 65_535).is_none());
        assert_eq!(callers.all().len(), 3);
    }

    #[test]
    fn two_callers_on_one_slot_are_refused() {
        let twice = NODE.replace("[11, 12]", "[11, 10]");
        assert!(Callers::parse(&twice).unwrap_err().contains("uid slot 10"));
        assert!(Callers::parse("{}").is_err());
    }
}
