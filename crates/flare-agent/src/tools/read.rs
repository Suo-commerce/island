//! `read`: a text file, from a line, a number of lines, truncated to
//! 2000 lines or 50 KB with a note saying how to go on.
//!
//! Ported from Pi, `packages/coding-agent/src/core/tools/read.ts`
//! (<https://github.com/badlogic/pi-mono>), Copyright (c) 2025 Mario
//! Zechner, MIT License (PI-LICENSE). Not ported: images, which the
//! agent's models are not given.

use serde_json::Value;

use super::{Outcome, number_arg, string_arg, work_path};
use crate::truncate::{By, MAX_BYTES, MAX_LINES, format_size, head};

/// The tool's description for the model.
pub fn description() -> String {
    format!(
        "Read the contents of a file. For text files, output is truncated to {MAX_LINES} lines \
         or {}KB (whichever is hit first). Use offset/limit for large files. When you need the \
         full file, continue with offset until complete.",
        MAX_BYTES / 1024
    )
}

/// The tool's parameters, as JSON Schema.
pub fn parameters() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
            "offset": { "type": "number", "description": "Line number to start reading from (1-indexed)" },
            "limit": { "type": "number", "description": "Maximum number of lines to read" }
        },
        "required": ["path"]
    })
}

/// Run the tool.
pub fn run(args: &Value) -> Outcome {
    let (path, offset, limit) = match (
        string_arg(args, "path"),
        number_arg(args, "offset"),
        number_arg(args, "limit"),
    ) {
        (Ok(path), Ok(offset), Ok(limit)) => (path, offset, limit),
        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
            return Outcome::error(error);
        }
    };
    let bytes = match std::fs::read(work_path(&path)) {
        Ok(bytes) => bytes,
        Err(error) => return Outcome::error(format!("{path}: {error}")),
    };
    if bytes.contains(&0) {
        return Outcome::error(format!(
            "{path} is a binary file ({}); read reads text. Use bash to look into it.",
            format_size(bytes.len())
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    Outcome::text(match select(&text, &path, offset, limit) {
        Ok(text) => text,
        Err(error) => return Outcome::error(error),
    })
}

/// The lines asked for, with Pi's notes.
pub fn select(
    text: &str,
    path: &str,
    offset: Option<f64>,
    limit: Option<f64>,
) -> Result<String, String> {
    let all: Vec<&str> = text.split('\n').collect();
    let total = all.len();
    let start = match offset {
        Some(offset) if offset >= 1.0 => offset as usize - 1,
        _ => 0,
    };
    if start >= total {
        let shown = offset.map_or_else(|| "0".to_owned(), |o| format!("{o}"));
        return Err(format!(
            "Offset {shown} is beyond end of file ({total} lines total)"
        ));
    }
    let limit = limit.map(|limit| limit.max(0.0) as usize);
    let end = limit.map_or(total, |limit| (start + limit).min(total));
    let selected = all[start..end].join("\n");
    let truncation = head(&selected, MAX_LINES, MAX_BYTES);
    let first = start + 1;
    if truncation.first_line_exceeds_limit {
        return Ok(format!(
            "[Line {first} is {}, exceeds {} limit. Use bash: sed -n '{first}p' {path} | head -c {MAX_BYTES}]",
            format_size(all[start].len()),
            format_size(MAX_BYTES)
        ));
    }
    if truncation.truncated() {
        let last = first + truncation.output_lines - 1;
        let next = last + 1;
        let note = match truncation.by {
            Some(By::Lines) => {
                format!("[Showing lines {first}-{last} of {total}. Use offset={next} to continue.]")
            }
            _ => format!(
                "[Showing lines {first}-{last} of {total} ({} limit). Use offset={next} to continue.]",
                format_size(MAX_BYTES)
            ),
        };
        return Ok(format!("{}\n\n{note}", truncation.content));
    }
    if limit.is_some() && end < total {
        let remaining = total - end;
        return Ok(format!(
            "{}\n\n[{remaining} more lines in file. Use offset={} to continue.]",
            truncation.content,
            end + 1
        ));
    }
    Ok(truncation.content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_short_file_is_read_whole() {
        assert_eq!(select("a\nb", "f", None, None), Ok("a\nb".to_owned()));
    }

    #[test]
    fn a_range_says_what_is_left() {
        let text = numbered(10);
        assert_eq!(
            select(&text, "f", Some(3.0), Some(2.0)),
            Ok("line 3\nline 4\n\n[6 more lines in file. Use offset=5 to continue.]".to_owned())
        );
        assert_eq!(
            select(&text, "f", Some(9.0), Some(5.0)),
            Ok("line 9\nline 10".to_owned())
        );
        assert_eq!(
            select(&text, "f", Some(11.0), None),
            Err("Offset 11 is beyond end of file (10 lines total)".to_owned())
        );
    }

    #[test]
    fn a_long_file_is_cut_with_where_to_go_on() {
        let text = numbered(2500);
        let shown = select(&text, "f", None, None).unwrap();
        assert!(shown.ends_with("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));
        let wide = vec!["x".repeat(1000); 100].join("\n");
        let shown = select(&wide, "f", None, None).unwrap();
        assert!(
            shown.ends_with(
                "[Showing lines 1-51 of 100 (50.0KB limit). Use offset=52 to continue.]"
            ),
            "{}",
            &shown[shown.len() - 90..]
        );
        let one = "y".repeat(60 * 1024);
        assert_eq!(
            select(&one, "big.txt", None, None),
            Ok("[Line 1 is 60.0KB, exceeds 50.0KB limit. Use bash: sed -n '1p' big.txt | head -c 51200]".to_owned())
        );
    }
}
