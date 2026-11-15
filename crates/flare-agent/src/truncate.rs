//! Truncation of tool output, by lines and by bytes, whichever limit is
//! hit first. Never a partial line, except the end of a single line longer
//! than the byte limit when truncating from the tail.
//!
//! Ported from Pi, `packages/coding-agent/src/core/tools/truncate.ts`
//! (<https://github.com/badlogic/pi-mono>), Copyright (c) 2025 Mario
//! Zechner, MIT License (PI-LICENSE). Pi counts UTF-8 bytes as here.

/// The most lines a tool shows.
pub const MAX_LINES: usize = 2000;
/// The most bytes a tool shows.
pub const MAX_BYTES: usize = 50 * 1024;

/// Which limit was hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    /// The line limit.
    Lines,
    /// The byte limit.
    Bytes,
}

/// What was kept of a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Truncation {
    /// What was kept.
    pub content: String,
    /// Which limit cut it, if one did.
    pub by: Option<By>,
    /// Lines in the whole text.
    pub total_lines: usize,
    /// Bytes in the whole text.
    pub total_bytes: usize,
    /// Whole lines kept.
    pub output_lines: usize,
    /// Bytes kept.
    pub output_bytes: usize,
    /// The one line kept is only its end (tail truncation).
    pub last_line_partial: bool,
    /// The first line alone is over the byte limit (head truncation):
    /// nothing is kept.
    pub first_line_exceeds_limit: bool,
}

impl Truncation {
    /// Whether anything was cut.
    pub fn truncated(&self) -> bool {
        self.by.is_some()
    }
}

/// The lines of a text: a final newline ends the last line, it does not
/// start an empty one.
fn lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn whole(content: &str, total_lines: usize) -> Truncation {
    Truncation {
        content: content.to_owned(),
        by: None,
        total_lines,
        total_bytes: content.len(),
        output_lines: total_lines,
        output_bytes: content.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
    }
}

/// Keep the first lines: for files, read from the start.
pub fn head(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let all = lines(content);
    let total_lines = all.len();
    if total_lines <= max_lines && content.len() <= max_bytes {
        return whole(content, total_lines);
    }
    if all[0].len() > max_bytes {
        return Truncation {
            content: String::new(),
            by: Some(By::Bytes),
            total_lines,
            total_bytes: content.len(),
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
        };
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0;
    let mut by = By::Lines;
    for (index, line) in all.iter().enumerate().take(max_lines) {
        let cost = line.len() + usize::from(index > 0);
        if bytes + cost > max_bytes {
            by = By::Bytes;
            break;
        }
        kept.push(line);
        bytes += cost;
    }
    if kept.len() >= max_lines && bytes <= max_bytes {
        by = By::Lines;
    }
    let out = kept.join("\n");
    Truncation {
        output_bytes: out.len(),
        output_lines: kept.len(),
        content: out,
        by: Some(by),
        total_lines,
        total_bytes: content.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
    }
}

/// Keep the last lines: for a command's output, where errors and results
/// are.
pub fn tail(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let all = lines(content);
    let total_lines = all.len();
    if total_lines <= max_lines && content.len() <= max_bytes {
        return whole(content, total_lines);
    }
    let mut kept: Vec<String> = Vec::new();
    let mut bytes = 0;
    let mut by = By::Lines;
    let mut last_line_partial = false;
    for line in all.iter().rev() {
        if kept.len() >= max_lines {
            break;
        }
        let cost = line.len() + usize::from(!kept.is_empty());
        if bytes + cost > max_bytes {
            by = By::Bytes;
            if kept.is_empty() {
                let end = end_bytes(line, max_bytes);
                bytes = end.len();
                kept.push(end.to_owned());
                last_line_partial = true;
            }
            break;
        }
        kept.push((*line).to_owned());
        bytes += cost;
    }
    if kept.len() >= max_lines && bytes <= max_bytes {
        by = By::Lines;
    }
    kept.reverse();
    let out = kept.join("\n");
    Truncation {
        output_bytes: out.len(),
        output_lines: kept.len(),
        content: out,
        by: Some(by),
        total_lines,
        total_bytes: content.len(),
        last_line_partial,
        first_line_exceeds_limit: false,
    }
}

/// The end of `text`, at most `max` bytes, starting on a character.
fn end_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// A size as people read it: `512B`, `1.5KB`, `2.0MB`.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_cut_under_both_limits() {
        let t = head("a\nb\n", 10, 100);
        assert!(!t.truncated());
        assert_eq!((t.total_lines, t.content.as_str()), (2, "a\nb\n"));
        assert_eq!(head("", 10, 10).total_lines, 0);
    }

    #[test]
    fn head_keeps_whole_lines_and_says_which_limit() {
        let text = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let t = head(&text, 3, 1000);
        assert_eq!(
            (t.content.as_str(), t.by),
            ("line 1\nline 2\nline 3", Some(By::Lines))
        );
        assert_eq!((t.total_lines, t.output_lines), (10, 3));
        // "line 1" (6) + "\nline 2" (7) = 13 fits 15; the third does not.
        let t = head(&text, 100, 15);
        assert_eq!(
            (t.content.as_str(), t.by),
            ("line 1\nline 2", Some(By::Bytes))
        );
        let t = head(&format!("{}\nshort", "x".repeat(20)), 100, 10);
        assert!(t.first_line_exceeds_limit);
        assert_eq!((t.content.as_str(), t.output_lines), ("", 0));
    }

    #[test]
    fn tail_keeps_the_end_and_a_long_last_line_in_part() {
        let text = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let t = tail(&text, 2, 1000);
        assert_eq!(
            (t.content.as_str(), t.by),
            ("line 9\nline 10", Some(By::Lines))
        );
        let t = tail(&format!("start\n{}", "é".repeat(10)), 100, 5);
        assert!(t.last_line_partial);
        // Five bytes from the end, on a character: two é.
        assert_eq!(t.content, "éé");
        assert_eq!(t.by, Some(By::Bytes));
    }

    #[test]
    fn sizes_read_as_pi_writes_them() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(50 * 1024), "50.0KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0MB");
    }
}
