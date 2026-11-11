//! The log's lock, while other threads of the same process start
//! programs.
//!
//! A program started by one thread holds copies of the process's open
//! files for a moment, and with them their locks. A log just closed by
//! one appender must still open for the next one.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use island_env::FileStorage;

#[test]
fn a_closed_log_opens_again_while_programs_are_being_started() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stop = Arc::new(AtomicBool::new(false));
    let starting = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let _ = std::process::Command::new("true").status();
            }
        })
    };
    let mut refused = 0;
    for n in 0..500 {
        let path = dir.path().join(format!("{n}.log"));
        drop(FileStorage::create(&path).expect("created"));
        if FileStorage::open(&path).is_err() {
            refused += 1;
        }
    }
    stop.store(true, Ordering::SeqCst);
    starting.join().expect("joined");
    assert_eq!(
        refused, 0,
        "a closed log was refused {refused} times of 500"
    );
}

#[test]
fn a_log_held_by_an_appender_is_still_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("island.log");
    let _held = FileStorage::create(&path).expect("created");
    let error = FileStorage::open(&path).expect_err("refused");
    assert_eq!(error.to_string(), "the log is held by another appender");
}
