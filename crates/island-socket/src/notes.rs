//! Fork notes (docs/slice-2.md, decision D4 and "Forking, step by step"):
//! what a fork child made, in at most 16 KiB, posted to the socket by the
//! child and read from it by its prime. A child's files and actions stay
//! in its own Vessel; until Taso has a `Proposals` query, the note is how
//! a prime learns what its forks made.
//!
//! The socket keeps each child's newest note, by its prime and its uid
//! slot, in `state_dir/notes.json`, so a restart of `island-socket` loses
//! none: a child posts its last note once and exits. A slot is never given
//! twice, so a note is never another child's. Each note names the fork
//! set it belongs to (the prime's word, opaque here), so a prime tells the
//! notes of the set it is waiting for from those of an earlier one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The longest note, in bytes.
pub const MAX_NOTE: usize = 16 * 1024;

/// The longest name of a fork set, in bytes.
pub const MAX_SET: usize = 128;

/// What a child posts: `POST /note`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Posted {
    /// The fork set it belongs to, as its task file names it.
    pub set: String,
    /// What it made.
    pub text: String,
    /// Whether it is done.
    #[serde(default)]
    pub done: bool,
    /// Whether it is done without anything to promote: it failed before
    /// it could end a turn of its own.
    #[serde(default)]
    pub failed: bool,
}

/// A child's newest note, as its prime reads it: `GET /notes`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    /// The child's name: `<prime>-fork-<slot>`.
    pub child: String,
    /// Its uid slot.
    pub slot: u32,
    /// The fork set it belongs to.
    pub set: String,
    /// What it made.
    pub text: String,
    /// Whether it is done.
    pub done: bool,
    /// Whether it failed and holds nothing to promote.
    #[serde(default)]
    pub failed: bool,
    /// When it was posted, in microseconds since the epoch.
    pub at: u64,
}

/// Every child's newest note, by prime and slot.
#[derive(Debug, Default)]
pub struct Notes {
    path: Option<PathBuf>,
    by_prime: BTreeMap<String, BTreeMap<u32, Note>>,
}

impl Notes {
    /// In memory only (tests).
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Read `state_dir/notes.json`, or start empty if there is none.
    pub fn load(state_dir: &Path) -> Result<Self, String> {
        let path = state_dir.join("notes.json");
        let by_prime = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        Ok(Self {
            path: Some(path),
            by_prime,
        })
    }

    /// Keep a child's note in place of its last one. `Err`: it could not
    /// be written to disk, and is not kept: the child posts it again.
    pub fn put(&mut self, prime: &str, note: Note) -> Result<(), String> {
        let slots = self.by_prime.entry(prime.to_owned()).or_default();
        let before = slots.insert(note.slot, note.clone());
        if let Err(why) = self.save() {
            let slots = self.by_prime.entry(prime.to_owned()).or_default();
            match before {
                Some(before) => slots.insert(note.slot, before),
                None => slots.remove(&note.slot),
            };
            return Err(why);
        }
        Ok(())
    }

    /// The notes of `prime`'s children, by slot.
    pub fn of(&self, prime: &str) -> Vec<Note> {
        self.by_prime
            .get(prime)
            .map(|slots| slots.values().cloned().collect())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let tmp = path.with_extension("json.tmp");
        serde_json::to_vec_pretty(&self.by_prime)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|error| format!("{}: {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(slot: u32, set: &str, text: &str, done: bool) -> Note {
        Note {
            child: format!("flare-home-fork-{slot}"),
            slot,
            set: set.to_owned(),
            text: text.to_owned(),
            done,
            failed: false,
            at: 7,
        }
    }

    #[test]
    fn a_childs_newest_note_is_kept_across_a_restart_and_is_its_primes_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut notes = Notes::load(dir.path()).unwrap();
        assert!(notes.of("flare-home").is_empty());
        notes
            .put("flare-home", note(12, "5-7", "started", false))
            .unwrap();
        notes
            .put("flare-home", note(11, "5-7", "a draft", true))
            .unwrap();
        notes
            .put("flare-home", note(12, "5-7", "another draft", true))
            .unwrap();
        notes.put("other", note(31, "1-1", "theirs", true)).unwrap();

        let again = Notes::load(dir.path()).unwrap();
        let mine = again.of("flare-home");
        assert_eq!(
            mine.iter()
                .map(|n| (n.slot, n.text.as_str(), n.done))
                .collect::<Vec<_>>(),
            [(11, "a draft", true), (12, "another draft", true)]
        );
        assert_eq!(again.of("other").len(), 1);
        assert!(again.of("nobody").is_empty());
    }

    #[test]
    fn a_note_that_cannot_be_written_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut notes = Notes::load(&dir.path().join("gone")).unwrap();
        assert!(notes.put("flare-home", note(11, "5-7", "x", true)).is_err());
        assert!(notes.of("flare-home").is_empty());
    }
}
