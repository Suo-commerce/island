//! `write`: a whole file, its directories made as needed.
//!
//! Ported from Pi, `packages/coding-agent/src/core/tools/write.ts`
//! (<https://github.com/badlogic/pi-mono>), Copyright (c) 2025 Mario
//! Zechner, MIT License (PI-LICENSE).

use serde_json::Value;

use super::{Outcome, string_arg, work_path};

/// The tool's description for the model.
pub const DESCRIPTION: &str = "Write content to a file. Creates the file if it doesn't exist, \
overwrites if it does. Automatically creates parent directories.";

/// The tool's parameters, as JSON Schema.
pub fn parameters() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
            "content": { "type": "string", "description": "Content to write to the file" }
        },
        "required": ["path", "content"]
    })
}

/// Run the tool.
pub fn run(args: &Value) -> Outcome {
    let (path, content) = match (string_arg(args, "path"), string_arg(args, "content")) {
        (Ok(path), Ok(content)) => (path, content),
        (Err(error), _) | (_, Err(error)) => return Outcome::error(error),
    };
    let file = work_path(&path);
    if let Some(dir) = file.parent()
        && let Err(error) = std::fs::create_dir_all(dir)
    {
        return Outcome::error(format!("{}: {error}", dir.display()));
    }
    match std::fs::write(&file, &content) {
        Ok(()) => Outcome::text(format!(
            "Successfully wrote {} bytes to {path}",
            content.len()
        )),
        Err(error) => Outcome::error(format!("{path}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_written_with_its_directories() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a/b/c.txt");
        let outcome = run(&serde_json::json!({ "path": file.to_str().unwrap(), "content": "hej" }));
        assert!(!outcome.is_error);
        assert!(outcome.text.starts_with("Successfully wrote 3 bytes to "));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "hej");
        assert!(run(&serde_json::json!({ "path": "x" })).is_error);
    }
}
