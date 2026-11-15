//! `edit`: exact text replacement in one file, several disjoint edits at
//! once, each matched against the original. An edit that does not match
//! exactly is tried once more with both sides normalized (trailing spaces,
//! typographic quotes, dashes and spaces); then only the lines it touches
//! are rewritten from the normalized text, and every other line keeps its
//! bytes. Line endings (LF, CRLF, CR) and a byte order mark are kept.
//!
//! Ported from Pi, `packages/coding-agent/src/core/tools/edit.ts` and
//! `edit-diff.ts` (<https://github.com/badlogic/pi-mono>), Copyright (c)
//! 2025 Mario Zechner, MIT License (PI-LICENSE). Pi's offsets are UTF-16
//! code units; these are UTF-8 bytes, used the same way. Not ported: the
//! display diff and patch, which only Pi's terminal shows.

use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use super::{Outcome, string_arg, work_path};

/// The tool's description for the model.
pub const DESCRIPTION: &str = "Edit a single file using exact text replacement. Every \
edits[].oldText must match a unique, non-overlapping region of the original file. If two \
changes affect the same block or nearby lines, merge them into one edit instead of emitting \
overlapping edits. Do not include large unchanged regions just to connect distant changes.";

/// The tool's parameters, as JSON Schema.
pub fn parameters() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
            "edits": {
                "type": "array",
                "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                "items": {
                    "type": "object",
                    "properties": {
                        "oldText": { "type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." },
                        "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                    },
                    "required": ["oldText", "newText"]
                }
            }
        },
        "required": ["path", "edits"]
    })
}

/// One replacement asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// What to find.
    pub old_text: String,
    /// What to put there.
    pub new_text: String,
}

/// `len` bytes at `at` become `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replacement {
    /// Where.
    pub at: usize,
    /// How much.
    pub len: usize,
    /// With what.
    pub text: String,
}

/// Run the tool.
pub fn run(args: &Value) -> Outcome {
    let path = match string_arg(args, "path") {
        Ok(path) => path,
        Err(error) => return Outcome::error(error),
    };
    let edits = match edits_of(args) {
        Ok(edits) => edits,
        Err(error) => return Outcome::error(error),
    };
    let file = work_path(&path);
    let raw = match std::fs::read(&file) {
        Ok(raw) => raw,
        Err(error) => {
            return Outcome::error(format!("Could not edit file: {path}. {error}."));
        }
    };
    let Ok(raw) = String::from_utf8(raw) else {
        return Outcome::error(format!(
            "Could not edit file: {path}. It is not UTF-8 text."
        ));
    };
    let (bom, content) = match raw.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", raw.as_str()),
    };
    let normalized = normalize_to_lf(content);
    let (_, new_content, replacements) = match apply_edits(&normalized, &edits, &path) {
        Ok(applied) => applied,
        Err(error) => return Outcome::error(error),
    };
    let finished = if content == normalized {
        new_content
    } else {
        apply_to_original(content, &replacements)
    };
    match std::fs::write(&file, format!("{bom}{finished}")) {
        Ok(()) => Outcome::text(format!(
            "Successfully replaced {} block(s) in {path}.",
            edits.len()
        )),
        Err(error) => Outcome::error(format!("Could not edit file: {path}. {error}.")),
    }
}

/// The edits from the arguments, taking the shapes models send: an array,
/// an array in a JSON string, one edit object, or `oldText`/`newText` at
/// the top level.
pub fn edits_of(args: &Value) -> Result<Vec<Edit>, String> {
    let one = |value: &Value| -> Option<Edit> {
        Some(Edit {
            old_text: value.get("oldText")?.as_str()?.to_owned(),
            new_text: value.get("newText")?.as_str()?.to_owned(),
        })
    };
    let mut edits = match args.get("edits") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| one(item).ok_or("each edit has a string oldText and newText"))
            .collect::<Result<Vec<_>, _>>()?,
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(items)) => items
                .iter()
                .map(|item| one(item).ok_or("each edit has a string oldText and newText"))
                .collect::<Result<Vec<_>, _>>()?,
            Ok(item) => one(&item).into_iter().collect(),
            Err(_) => Vec::new(),
        },
        Some(item @ Value::Object(_)) => one(item).into_iter().collect(),
        _ => Vec::new(),
    };
    if let Some(legacy) = one(args) {
        edits.push(legacy);
    }
    if edits.is_empty() {
        return Err(
            "Edit tool input is invalid. edits must contain at least one replacement.".to_owned(),
        );
    }
    Ok(edits)
}

/// CRLF and CR to LF.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The text a near match is looked for in: NFKC, no trailing spaces on a
/// line, typographic quotes, dashes and spaces as ASCII.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    nfkc.split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Where `old` is in `content`: exactly, or in the normalized texts.
/// `(at, len, in_normalized)`.
fn find(content: &str, old: &str) -> Option<(usize, usize, bool)> {
    if let Some(at) = content.find(old) {
        return Some((at, old.len(), false));
    }
    let content = normalize_for_fuzzy_match(content);
    let old = normalize_for_fuzzy_match(old);
    content.find(&old).map(|at| (at, old.len(), true))
}

fn occurrences(content: &str, old: &str) -> usize {
    let content = normalize_for_fuzzy_match(content);
    let old = normalize_for_fuzzy_match(old);
    if old.is_empty() {
        return 0;
    }
    content.matches(&old).count()
}

/// Apply `edits` to LF text: the base text, the new text, and the
/// replacements of the base, in order.
pub fn apply_edits(
    normalized: &str,
    edits: &[Edit],
    path: &str,
) -> Result<(String, String, Vec<Replacement>), String> {
    let total = edits.len();
    let edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();
    for (index, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(if total == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{index}].oldText must not be empty in {path}.")
            });
        }
    }
    let fuzzy = edits
        .iter()
        .any(|edit| find(normalized, &edit.old_text).is_some_and(|(_, _, fuzzy)| fuzzy));
    let base = if fuzzy {
        normalize_for_fuzzy_match(normalized)
    } else {
        normalized.to_owned()
    };
    let mut matched: Vec<(usize, Replacement)> = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let Some((at, len, _)) = find(&base, &edit.old_text) else {
            return Err(if total == 1 {
                format!(
                    "Could not find the exact text in {path}. The old text must match exactly \
                     including all whitespace and newlines."
                )
            } else {
                format!(
                    "Could not find edits[{index}] in {path}. The oldText must match exactly \
                     including all whitespace and newlines."
                )
            });
        };
        let count = occurrences(&base, &edit.old_text);
        if count > 1 {
            return Err(if total == 1 {
                format!(
                    "Found {count} occurrences of the text in {path}. The text must be unique. \
                     Please provide more context to make it unique."
                )
            } else {
                format!(
                    "Found {count} occurrences of edits[{index}] in {path}. Each oldText must be \
                     unique. Please provide more context to make it unique."
                )
            });
        }
        matched.push((
            index,
            Replacement {
                at,
                len,
                text: edit.new_text.clone(),
            },
        ));
    }
    matched.sort_by_key(|(_, replacement)| replacement.at);
    for pair in matched.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);
        if before.1.at + before.1.len > after.1.at {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target \
                 disjoint regions.",
                before.0, after.0
            ));
        }
    }
    let replacements: Vec<Replacement> = matched.into_iter().map(|(_, r)| r).collect();
    let replacements = if fuzzy {
        widen_to_lines(normalized, &base, &replacements)?
    } else {
        replacements
    };
    let new_content = apply(normalized, &replacements, 0);
    if new_content == normalized {
        return Err(if total == 1 {
            format!(
                "No changes made to {path}. The replacement produced identical content. This \
                 might indicate an issue with special characters or the text not existing as \
                 expected."
            )
        } else {
            format!("No changes made to {path}. The replacements produced identical content.")
        });
    }
    Ok((normalized.to_owned(), new_content, replacements))
}

/// Apply sorted replacements, their offsets counted from `offset`.
fn apply(content: &str, replacements: &[Replacement], offset: usize) -> String {
    let mut out = content.to_owned();
    for replacement in replacements.iter().rev() {
        let at = replacement.at - offset;
        out.replace_range(at..at + replacement.len, &replacement.text);
    }
    out
}

/// Each line with its ending, as byte spans.
fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            spans.push((start, index + 1));
            start = index + 1;
        }
    }
    if start < content.len() {
        spans.push((start, content.len()));
    }
    spans
}

/// The lines a replacement touches: `[start, end)`.
fn lines_of(spans: &[(usize, usize)], replacement: &Replacement) -> Result<(usize, usize), String> {
    let (from, to) = (replacement.at, replacement.at + replacement.len);
    let start = spans
        .iter()
        .position(|&(_, end)| from < end)
        .unwrap_or(spans.len().saturating_sub(1));
    let last = if replacement.len == 0 {
        start
    } else {
        spans
            .iter()
            .position(|&(_, end)| to <= end)
            .ok_or("Replacement range is outside the base content.")?
    };
    if start >= spans.len() {
        return Err("Replacement range is outside the base content.".to_owned());
    }
    Ok((start, last + 1))
}

/// Replacements matched in the normalized text, as replacements of whole
/// lines of the original: touched lines are rewritten from the normalized
/// text, all others keep their bytes.
fn widen_to_lines(
    original: &str,
    base: &str,
    replacements: &[Replacement],
) -> Result<Vec<Replacement>, String> {
    let original_lines = line_spans(original);
    let base_lines = line_spans(base);
    if original_lines.len() != base_lines.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .to_owned(),
        );
    }
    let mut groups: Vec<(usize, usize, Vec<Replacement>)> = Vec::new();
    for replacement in replacements {
        let (start, end) = lines_of(&base_lines, replacement)?;
        match groups.last_mut() {
            Some(group) if start < group.1 => {
                group.1 = group.1.max(end);
                group.2.push(replacement.clone());
            }
            _ => groups.push((start, end, vec![replacement.clone()])),
        }
    }
    Ok(groups
        .into_iter()
        .map(|(start, end, group)| {
            let (from, to) = (base_lines[start].0, base_lines[end - 1].1);
            let (original_from, original_to) = (original_lines[start].0, original_lines[end - 1].1);
            Replacement {
                at: original_from,
                len: original_to - original_from,
                text: apply(&base[from..to], &group, from),
            }
        })
        .collect())
}

/// The line ending at `offset`: its line's, or the line before's for an
/// unterminated last line. A lone CR counts only in a file without LF.
fn ending_at(content: &str, offset: usize) -> &'static str {
    let feed = content[offset..]
        .find('\n')
        .map(|at| at + offset)
        .or_else(|| content[..offset].rfind('\n'));
    match feed {
        None if content.contains('\r') => "\r",
        None => "\n",
        Some(at) if at > 0 && content.as_bytes()[at - 1] == b'\r' => "\r\n",
        Some(_) => "\n",
    }
}

/// Apply replacements of `normalize_to_lf(original)` to `original`: text
/// outside them keeps its bytes; new line breaks take the ending of the
/// line where their replacement starts.
pub fn apply_to_original(original: &str, replacements: &[Replacement]) -> String {
    // Only a CRLF changes length when normalizing; counting those before a
    // normalized offset gives the original one.
    let crlfs: Vec<usize> = original.match_indices("\r\n").map(|(at, _)| at).collect();
    let to_original = |offset: usize| -> usize {
        let before = crlfs
            .iter()
            .enumerate()
            .take_while(|(index, at)| **at - index < offset)
            .count();
        offset + before
    };
    let mut parts = String::new();
    let mut ends_with_cr = false;
    let mut push = |part: &str, parts: &mut String| {
        if part.is_empty() {
            return;
        }
        if ends_with_cr && part.starts_with('\n') {
            parts.push('\n');
        }
        parts.push_str(part);
        ends_with_cr = part.ends_with('\r');
    };
    let mut position = 0;
    for replacement in replacements {
        let start = to_original(replacement.at);
        push(&original[position..start], &mut parts);
        let ending = ending_at(original, start);
        let text = if ending == "\n" {
            replacement.text.clone()
        } else {
            replacement.text.replace('\n', ending)
        };
        push(&text, &mut parts);
        position = to_original(replacement.at + replacement.len);
    }
    push(&original[position..], &mut parts);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Edit {
        Edit {
            old_text: old.to_owned(),
            new_text: new.to_owned(),
        }
    }

    fn applied(content: &str, edits: &[Edit]) -> Result<String, String> {
        apply_edits(content, edits, "f.txt").map(|(_, new, _)| new)
    }

    #[test]
    fn exact_edits_apply_against_the_original() {
        let text = "fn a() {}\nfn b() {}\nfn c() {}\n";
        assert_eq!(
            applied(text, &[edit("fn c", "fn z"), edit("fn a", "fn y")]),
            Ok("fn y() {}\nfn b() {}\nfn z() {}\n".to_owned())
        );
    }

    #[test]
    fn missing_duplicate_empty_overlapping_and_unchanged_are_refused() {
        let text = "one\ntwo\ntwo\nthree\n";
        assert!(
            applied(text, &[edit("four", "x")])
                .unwrap_err()
                .starts_with("Could not find the exact text")
        );
        assert!(
            applied(text, &[edit("one", "1"), edit("four", "x")])
                .unwrap_err()
                .starts_with("Could not find edits[1]")
        );
        assert!(
            applied(text, &[edit("two", "2")])
                .unwrap_err()
                .starts_with("Found 2 occurrences")
        );
        assert_eq!(
            applied(text, &[edit("", "x")]),
            Err("oldText must not be empty in f.txt.".to_owned())
        );
        assert!(
            applied(text, &[edit("one\ntwo", "a"), edit("ne\ntw", "b")])
                .unwrap_err()
                .contains("overlap")
        );
        assert!(
            applied(text, &[edit("one", "one")])
                .unwrap_err()
                .starts_with("No changes made")
        );
    }

    #[test]
    fn a_near_match_rewrites_only_the_lines_it_touches() {
        // Typographic quotes and trailing spaces in the file; ASCII in the
        // edit. The untouched line keeps its trailing spaces.
        let text = "say \u{201C}hello\u{201D}   \nkeep me   \n";
        let new = applied(text, &[edit("say \"hello\"", "say \"bye\"")]).unwrap();
        assert_eq!(new, "say \"bye\"\nkeep me   \n");
    }

    #[test]
    fn line_endings_and_a_bom_are_kept() {
        let original = "a\r\nb\r\nc\r\n";
        let normalized = normalize_to_lf(original);
        let (_, _, replacements) =
            apply_edits(&normalized, &[edit("b\n", "b1\nb2\n")], "f").unwrap();
        assert_eq!(
            apply_to_original(original, &replacements),
            "a\r\nb1\r\nb2\r\nc\r\n"
        );

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bom.txt");
        std::fs::write(&file, "\u{feff}x = 1\r\ny = 2\r\n").unwrap();
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "edits": [{ "oldText": "y = 2", "newText": "y = 3" }]
        });
        let outcome = run(&args);
        assert!(!outcome.is_error, "{}", outcome.text);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "\u{feff}x = 1\r\ny = 3\r\n"
        );
    }

    #[test]
    fn the_shapes_models_send_are_read() {
        let one = serde_json::json!({ "path": "f", "edits": { "oldText": "a", "newText": "b" } });
        assert_eq!(edits_of(&one).unwrap(), vec![edit("a", "b")]);
        let text =
            serde_json::json!({ "path": "f", "edits": "[{\"oldText\":\"a\",\"newText\":\"b\"}]" });
        assert_eq!(edits_of(&text).unwrap(), vec![edit("a", "b")]);
        let legacy = serde_json::json!({ "path": "f", "oldText": "a", "newText": "b" });
        assert_eq!(edits_of(&legacy).unwrap(), vec![edit("a", "b")]);
        assert!(edits_of(&serde_json::json!({ "path": "f", "edits": [] })).is_err());
    }
}
