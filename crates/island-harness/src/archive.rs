//! The seed archive (docs/spec.md, "Seed archive").
//!
//! Every case that ever broke an oracle is kept under `harness/seeds/`,
//! shrunk, with the oracle it broke. The harness runs all of them before
//! any new seed. Nothing is ever removed.
//!
//! A file holds the operations themselves, not only the seed: a seed
//! means a different case as soon as the generators change, and an
//! archive that silently stops testing what it once caught is worse than
//! none.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ops::Op;

/// One archived failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Archived {
    /// The oracle that broke.
    pub oracle: String,
    /// The seed that first produced the case.
    pub seed: u64,
    /// The operation after which the oracle broke, in the shrunk case.
    pub step: usize,
    /// What was seen.
    pub message: String,
    /// The shrunk case.
    pub ops: Vec<Op>,
}

/// Where the archive lives: `harness/seeds/` at the workspace root, or
/// the directory named by `ISLAND_HARNESS_ARCHIVE`.
pub fn dir() -> PathBuf {
    match std::env::var_os("ISLAND_HARNESS_ARCHIVE") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../harness/seeds"),
    }
}

/// Write a failure to the archive. Returns the file, which is to be
/// committed with the fix.
pub fn save(dir: &Path, archived: &Archived) -> io::Result<PathBuf> {
    let path = dir.join(format!("{}-seed-{}.json", archived.oracle, archived.seed));
    let mut json = serde_json::to_string_pretty(archived).map_err(io::Error::other)?;
    json.push('\n');
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Every archived failure, in file-name order.
pub fn load(dir: &Path) -> io::Result<Vec<(PathBuf, Archived)>> {
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)?;
            let archived = serde_json::from_str(&text).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {error}", path.display()),
                )
            })?;
            Ok((path, archived))
        })
        .collect()
}
