//! `bash`: one command in `/work`, standard output and error together,
//! the last 2000 lines or 50 KB of it; the whole output in a file when
//! that was cut. A command may be given a timeout; it and every process
//! it started are killed at the end of it.
//!
//! Ported from Pi, `packages/coding-agent/src/core/tools/bash.ts` and
//! `output-accumulator.ts` (<https://github.com/badlogic/pi-mono>),
//! Copyright (c) 2025 Mario Zechner, MIT License (PI-LICENSE). Not
//! ported: the live updates, which only Pi's terminal shows.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Outcome, number_arg, string_arg, work_path};
use crate::truncate::{By, MAX_BYTES, MAX_LINES, format_size, tail};

/// The longest timeout a command may ask for, in seconds.
pub const MAX_TIMEOUT: f64 = 3600.0;
/// The timeout of a command that asks for none, in seconds: a turn must
/// end within minutes (docs/slice-2.md, "Turns").
pub const DEFAULT_TIMEOUT: f64 = 600.0;

/// The tool's description for the model.
pub fn description() -> String {
    format!(
        "Execute a bash command in the current working directory. Returns stdout and stderr. \
         Output is truncated to last {MAX_LINES} lines or {}KB (whichever is hit first). If \
         truncated, full output is saved to a temp file. Optionally provide a timeout in \
         seconds.",
        MAX_BYTES / 1024
    )
}

/// The tool's parameters, as JSON Schema.
pub fn parameters() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "command": { "type": "string", "description": "Shell command to execute" },
            "timeout": { "type": "number", "description": "Timeout in seconds (optional; 600 if not given)" }
        },
        "required": ["command"]
    })
}

/// Run the tool.
pub fn run(args: &Value) -> Outcome {
    let (command, timeout) = match (string_arg(args, "command"), number_arg(args, "timeout")) {
        (Ok(command), Ok(timeout)) => (command, timeout),
        (Err(error), _) | (_, Err(error)) => return Outcome::error(error),
    };
    let timeout = match timeout {
        None => DEFAULT_TIMEOUT,
        Some(seconds) if !seconds.is_finite() || seconds <= 0.0 => {
            return Outcome::error("Invalid timeout: must be a finite number of seconds");
        }
        Some(seconds) if seconds > MAX_TIMEOUT => {
            return Outcome::error(format!("Invalid timeout: maximum is {MAX_TIMEOUT} seconds"));
        }
        Some(seconds) => seconds,
    };
    match execute(&command, Duration::from_secs_f64(timeout)) {
        Ok(ran) => ran.outcome(timeout),
        Err(error) => Outcome::error(format!("Could not run the command: {error}")),
    }
}

/// How a command ended.
struct Ran {
    output: Vec<u8>,
    /// Its exit code; `None` if a signal ended it.
    code: Option<i32>,
    timed_out: bool,
}

fn execute(command: &str, timeout: Duration) -> std::io::Result<Ran> {
    let (mut reader, writer) = std::io::pipe()?;
    let errors = writer.try_clone()?;
    let mut shell = Command::new("bash");
    // A Vessel's environment may be empty: a path and a home for the shell.
    if std::env::var_os("PATH").is_none() {
        shell.env("PATH", "/usr/local/bin:/usr/bin:/bin");
    }
    if std::env::var_os("HOME").is_none() {
        shell.env("HOME", work_path("."));
    }
    let mut child = shell
        .arg("-c")
        .arg(command)
        .current_dir(work_path("."))
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(errors)
        // Its own process group, so a timeout ends what it started too.
        .process_group(0)
        .spawn()?;
    // The command holds the pipe's writing ends: without dropping it the
    // output would never end.
    drop(shell);
    let pgid = child.id();
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        let _ = sent.send(output);
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            kill_group(pgid);
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // The output ends when every process holding the pipe has gone. One
    // left in the background is ended after a moment.
    let output = match received.recv_timeout(Duration::from_secs(2)) {
        Ok(output) => output,
        Err(_) => {
            kill_group(pgid);
            received
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_default()
        }
    };
    Ok(Ran {
        output,
        code: status.code(),
        timed_out,
    })
}

/// Kill a process group, with the shell's `kill` (no unsafe code here).
fn kill_group(pgid: u32) {
    let _ = Command::new("bash")
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .arg("-c")
        .arg(format!("kill -KILL -- -{pgid} 2>/dev/null"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl Ran {
    fn outcome(&self, timeout: f64) -> Outcome {
        let text = String::from_utf8_lossy(&self.output);
        let truncation = tail(&text, MAX_LINES, MAX_BYTES);
        let mut shown = if truncation.content.is_empty() && !self.timed_out {
            "(no output)".to_owned()
        } else {
            truncation.content.clone()
        };
        if truncation.truncated() {
            let saved = save_full_output(&self.output);
            let first = truncation.total_lines - truncation.output_lines + 1;
            let last = truncation.total_lines;
            let note = if truncation.last_line_partial {
                format!(
                    "[Showing last {} of line {last} (line is {}). Full output: {saved}]",
                    format_size(truncation.output_bytes),
                    format_size(text.lines().last().map_or(0, str::len))
                )
            } else if truncation.by == Some(By::Lines) {
                format!(
                    "[Showing lines {first}-{last} of {}. Full output: {saved}]",
                    truncation.total_lines
                )
            } else {
                format!(
                    "[Showing lines {first}-{last} of {} ({} limit). Full output: {saved}]",
                    truncation.total_lines,
                    format_size(MAX_BYTES)
                )
            };
            shown = format!("{shown}\n\n{note}");
        }
        let status = |text: &str, status: String| {
            if text.is_empty() {
                status
            } else {
                format!("{text}\n\n{status}")
            }
        };
        if self.timed_out {
            return Outcome::error(status(
                &shown,
                format!("Command timed out after {timeout} seconds"),
            ));
        }
        match self.code {
            Some(0) => Outcome::text(shown),
            Some(code) => {
                Outcome::error(status(&shown, format!("Command exited with code {code}")))
            }
            None => Outcome::error(status(
                &shown,
                "Command terminated without an exit code".to_owned(),
            )),
        }
    }
}

/// The whole output, in a file under `/work/.flare/tmp`; its path, or why
/// there is none.
fn save_full_output(output: &[u8]) -> String {
    let digest = blake3::hash(output).to_hex();
    let path = work_path(&format!(".flare/tmp/bash-{}.log", &digest.as_str()[..16]));
    let saved = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, output));
    match saved {
        Ok(()) => path.display().to_string(),
        Err(error) => format!("(not saved: {error})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_work<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        let _guard = super::super::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::set_work(dir.path());
        f(dir.path())
    }

    #[test]
    fn output_and_exit_codes_come_back() {
        with_work(|_| {
            let ok = run(&serde_json::json!({ "command": "echo out; echo err >&2" }));
            assert_eq!(ok, Outcome::text("out\nerr\n"));
            let failed = run(&serde_json::json!({ "command": "echo no; exit 3" }));
            assert_eq!(failed, Outcome::error("no\n\n\nCommand exited with code 3"));
            assert_eq!(
                run(&serde_json::json!({ "command": "true" })),
                Outcome::text("(no output)")
            );
        });
    }

    #[test]
    fn a_command_runs_in_work() {
        with_work(|dir| {
            std::fs::write(dir.join("here.txt"), "").unwrap();
            assert_eq!(
                run(&serde_json::json!({ "command": "ls" })),
                Outcome::text("here.txt\n")
            );
        });
    }

    #[test]
    fn a_timeout_ends_the_command_and_what_it_started() {
        with_work(|_| {
            let outcome = run(
                &serde_json::json!({ "command": "echo started; sleep 30 & sleep 30", "timeout": 0.5 }),
            );
            assert!(outcome.is_error);
            assert!(outcome.text.starts_with("started"), "{}", outcome.text);
            assert!(
                outcome
                    .text
                    .ends_with("Command timed out after 0.5 seconds"),
                "{}",
                outcome.text
            );
            assert!(run(&serde_json::json!({ "command": "true", "timeout": -1 })).is_error);
            assert!(run(&serde_json::json!({ "command": "true", "timeout": 7200 })).is_error);
        });
    }

    #[test]
    fn long_output_keeps_its_end_and_the_rest_is_saved() {
        with_work(|dir| {
            let outcome = run(&serde_json::json!({ "command": "seq 1 3000" }));
            assert!(!outcome.is_error);
            assert!(
                outcome.text.starts_with("1001\n"),
                "{}",
                &outcome.text[..20]
            );
            let note = outcome.text.lines().last().unwrap();
            assert!(
                note.starts_with("[Showing lines 1001-3000 of 3000. Full output: "),
                "{note}"
            );
            let saved = note.trim_end_matches(']').rsplit(' ').next().unwrap();
            assert!(saved.starts_with(dir.to_str().unwrap()));
            assert_eq!(
                std::fs::read_to_string(saved).unwrap().lines().count(),
                3000
            );
        });
    }
}
