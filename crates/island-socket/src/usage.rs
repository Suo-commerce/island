//! The day's usage per Vessel and provider, against each provider's
//! budget (docs/slice-2.md, "The island socket", rules). Kept in a file in
//! the state directory and written after every call, so restarting
//! `island-socket` is no way past a budget. A day is a UTC day.
//!
//! Also kept there: which fork slots have called, so `whoami` can say how
//! many forks a prime has left. A child's first request is `whoami`, and a
//! slot is never given twice *(read; ran: S4)*, so the slots seen are the
//! slots used.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use island_core::VesselId;
use serde::{Deserialize, Serialize};

use crate::config::Budget;

/// One Vessel's day with one provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spent {
    /// Calls answered (or attempted: a provider that did not answer
    /// counts).
    pub calls: u64,
    /// Bytes both ways.
    pub bytes: u64,
    /// Calls refused because the budget was spent.
    pub refused: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    day: u64,
    used: Vec<Row>,
    #[serde(default)]
    forks_seen: BTreeMap<String, BTreeSet<u32>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Row {
    vessel: String,
    provider: String,
    #[serde(flatten)]
    spent: Spent,
}

/// The state, in memory and on disk.
#[derive(Debug)]
pub struct Usage {
    path: Option<PathBuf>,
    day: u64,
    used: BTreeMap<(VesselId, String), Spent>,
    forks_seen: BTreeMap<String, BTreeSet<u32>>,
}

/// Over budget: what was spent and the budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Over {
    /// Spent so far today.
    pub spent: Spent,
    /// The budget.
    pub budget: Budget,
}

/// Today, as days since 1970-01-01 UTC, by the system clock.
pub fn today() -> u64 {
    use island_env::Clock;
    island_env::SystemClock.now_micros() / 86_400_000_000
}

impl Usage {
    /// In memory only (tests).
    pub fn in_memory(day: u64) -> Self {
        Self {
            path: None,
            day,
            used: BTreeMap::new(),
            forks_seen: BTreeMap::new(),
        }
    }

    /// Read `state_dir/usage.json`, or start empty if there is none.
    pub fn load(state_dir: &Path, day: u64) -> Result<Self, String> {
        let path = state_dir.join("usage.json");
        let mut usage = Self::in_memory(day);
        usage.path = Some(path.clone());
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(usage),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let file: File =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        usage.forks_seen = file.forks_seen;
        if file.day == day {
            for row in file.used {
                let vessel = island_env::keyfile::unhex::<16>(&row.vessel)
                    .ok_or_else(|| format!("{}: a bad vessel id", path.display()))?;
                usage.used.insert((vessel, row.provider), row.spent);
            }
        }
        Ok(usage)
    }

    fn roll(&mut self, day: u64) {
        if day != self.day {
            self.day = day;
            self.used.clear();
        }
    }

    /// Whether one more call fits; a refusal is counted.
    pub fn admit(
        &mut self,
        day: u64,
        vessel: VesselId,
        provider: &str,
        budget: Budget,
    ) -> Result<(), Over> {
        self.roll(day);
        let spent = self.used.entry((vessel, provider.to_owned())).or_default();
        if spent.calls >= budget.calls || spent.bytes >= budget.bytes {
            spent.refused += 1;
            let over = Over {
                spent: *spent,
                budget,
            };
            self.save();
            return Err(over);
        }
        Ok(())
    }

    /// A call done: `bytes` both ways.
    pub fn spend(&mut self, day: u64, vessel: VesselId, provider: &str, bytes: u64) {
        self.roll(day);
        let spent = self.used.entry((vessel, provider.to_owned())).or_default();
        spent.calls += 1;
        spent.bytes = spent.bytes.saturating_add(bytes);
        self.save();
    }

    /// What one Vessel spent today with one provider.
    pub fn spent(&self, vessel: VesselId, provider: &str) -> Spent {
        self.used
            .get(&(vessel, provider.to_owned()))
            .copied()
            .unwrap_or_default()
    }

    /// A fork slot of `prime` has called. Taso gives a child the first
    /// of the prime's fork slots no child was ever born in
    /// (`free_fork_slot`, in the config's order), so a child in one slot
    /// means every slot before it in `fork_slots` is used too, whether its
    /// child ever called or not.
    pub fn saw_fork(&mut self, prime: &str, slot: u32, fork_slots: &[u32]) {
        let seen = self.forks_seen.entry(prime.to_owned()).or_default();
        let upto = fork_slots
            .iter()
            .position(|&s| s == slot)
            .map_or(0, |place| place + 1);
        let mut changed = seen.insert(slot);
        for &before in &fork_slots[..upto] {
            changed |= seen.insert(before);
        }
        if changed {
            self.save();
        }
    }

    /// The fork slots of `prime` that have called.
    pub fn forks_seen(&self, prime: &str) -> usize {
        self.forks_seen.get(prime).map_or(0, BTreeSet::len)
    }

    /// Write the file, atomically. A failure is reported and the call
    /// goes on: the budget in memory still holds until a restart.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let file = File {
            day: self.day,
            used: self
                .used
                .iter()
                .map(|((vessel, provider), spent)| Row {
                    vessel: vessel.iter().map(|b| format!("{b:02x}")).collect(),
                    provider: provider.clone(),
                    spent: *spent,
                })
                .collect(),
            forks_seen: self.forks_seen.clone(),
        };
        let tmp = path.with_extension("json.tmp");
        let result = serde_json::to_vec_pretty(&file)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, path));
        if let Err(error) = result {
            eprintln!("island-socket: saving {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: VesselId = [7; 16];
    const BUDGET: Budget = Budget {
        calls: 2,
        bytes: 1000,
    };

    #[test]
    fn a_budget_holds_in_calls_and_in_bytes_and_counts_refusals() {
        let mut usage = Usage::in_memory(10);
        assert!(usage.admit(10, V, "local", BUDGET).is_ok());
        usage.spend(10, V, "local", 100);
        assert!(usage.admit(10, V, "local", BUDGET).is_ok());
        usage.spend(10, V, "local", 100);
        let over = usage.admit(10, V, "local", BUDGET).unwrap_err();
        assert_eq!((over.spent.calls, over.spent.refused), (2, 1));
        // Another provider, another Vessel: their own budgets.
        assert!(usage.admit(10, V, "gpu", BUDGET).is_ok());
        assert!(usage.admit(10, [8; 16], "local", BUDGET).is_ok());
        // Bytes alone can spend it.
        usage.spend(10, [8; 16], "local", 1000);
        assert!(usage.admit(10, [8; 16], "local", BUDGET).is_err());
        // A new day starts afresh.
        assert!(usage.admit(11, V, "local", BUDGET).is_ok());
        assert_eq!(usage.spent(V, "local"), Spent::default());
    }

    #[test]
    fn a_restart_is_no_way_past_a_budget_but_a_new_day_is() {
        let dir = tempfile::tempdir().unwrap();
        let mut usage = Usage::load(dir.path(), 10).unwrap();
        usage.spend(10, V, "local", 5);
        usage.spend(10, V, "local", 5);
        usage.saw_fork("flare-home", 11, &[11, 12, 13]);
        let again = Usage::load(dir.path(), 10).unwrap();
        assert_eq!(again.spent(V, "local").calls, 2);
        assert_eq!(again.forks_seen("flare-home"), 1);
        let tomorrow = Usage::load(dir.path(), 11).unwrap();
        assert_eq!(tomorrow.spent(V, "local").calls, 0);
        assert_eq!(tomorrow.forks_seen("flare-home"), 1, "slots stay used");
    }

    #[test]
    fn a_fork_in_a_slot_means_the_slots_before_it_are_used() {
        // Taso gives the first slot never taken: the child in 13 was not
        // the first, though the children in 11 and 12 never called.
        let mut usage = Usage::in_memory(10);
        usage.saw_fork("flare-home", 13, &[11, 12, 13, 14]);
        assert_eq!(usage.forks_seen("flare-home"), 3);
        usage.saw_fork("flare-home", 12, &[11, 12, 13, 14]);
        assert_eq!(usage.forks_seen("flare-home"), 3);
        // A slot the config no longer lists still counts for itself.
        usage.saw_fork("flare-home", 99, &[11, 12, 13, 14]);
        assert_eq!(usage.forks_seen("flare-home"), 4);
    }
}
