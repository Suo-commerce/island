//! Records the git commit this build is made from. A `policy` entry names
//! it as its `evaluator`: the build that judges (docs/spec.md, "The
//! ratchet in slice 1").
//!
//! `ISLAND_BUILD_COMMIT` is the commit `HEAD` names, with `-dirty` added
//! when tracked files differ from it, or `unknown` outside a git checkout.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    let commit = git(&["rev-parse", "HEAD"]).filter(|commit| !commit.is_empty());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|changes| !changes.is_empty());
    let build = match commit {
        Some(commit) if dirty => format!("{commit}-dirty"),
        Some(commit) => commit,
        None => "unknown".to_owned(),
    };
    println!("cargo:rustc-env=ISLAND_BUILD_COMMIT={build}");

    // Look again when HEAD moves, a commit is made or files are staged.
    println!("cargo:rerun-if-changed=build.rs");
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/index");
    }
}
