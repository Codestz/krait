use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::Value;

/// Apply a JSON `WorkspaceEdit` to files on disk atomically.
///
/// Handles both `changes` (old form: `{uri: [TextEdit]}`) and
/// `documentChanges` (new form with `TextDocumentEdit`).
///
/// Returns the list of absolute paths that were modified.
///
/// # Errors
/// Returns an error if any file cannot be read or written.
pub fn apply_workspace_edit(edit: &Value, project_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let file_edits = collect_file_edits(edit);

    let mut modified = Vec::new();
    for (path, edits) in file_edits {
        if edits.is_empty() {
            continue;
        }
        let abs_path = if path.is_absolute() {
            path.clone()
        } else {
            project_root.join(&path)
        };
        apply_text_edits_to_file(&abs_path, &edits)
            .with_context(|| format!("failed to apply edits to {}", abs_path.display()))?;
        modified.push(abs_path);
    }

    Ok(modified)
}

/// Collect `(absolute_path, Vec<TextEdit>)` pairs from any `WorkspaceEdit` format.
fn collect_file_edits(edit: &Value) -> Vec<(PathBuf, Vec<Value>)> {
    let mut result: Vec<(PathBuf, Vec<Value>)> = Vec::new();

    if let Some(doc_changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for change in doc_changes {
            // TextDocumentEdit: has `textDocument` and `edits`
            if let Some(edits_arr) = change.get("edits").and_then(Value::as_array) {
                let uri = change
                    .pointer("/textDocument/uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                result.push((uri_to_path(uri), edits_arr.clone()));
            }
            // CreateFile/RenameFile/DeleteFile: skip (unsupported for now)
        }
    } else if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits_val) in changes {
            let edits = edits_val.as_array().cloned().unwrap_or_default();
            result.push((uri_to_path(uri), edits));
        }
    }

    result
}

fn uri_to_path(uri: &str) -> PathBuf {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    PathBuf::from(path)
}

/// Apply LSP `TextEdit` list to a file on disk.
///
/// Reads the file, applies edits in reverse position order (to preserve offsets),
/// then writes back atomically via a temp file + rename.
fn apply_text_edits_to_file(path: &Path, edits: &[Value]) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

    // Sort in reverse order: last position first so earlier positions stay valid
    let mut sorted: Vec<&Value> = edits.iter().collect();
    sorted.sort_by(|a, b| {
        let al = a
            .pointer("/range/start/line")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let bl = b
            .pointer("/range/start/line")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let ac = a
            .pointer("/range/start/character")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let bc = b
            .pointer("/range/start/character")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        bl.cmp(&al).then(bc.cmp(&ac))
    });

    for edit in sorted {
        #[allow(clippy::cast_possible_truncation)]
        let start_line = edit
            .pointer("/range/start/line")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        #[allow(clippy::cast_possible_truncation)]
        let start_char = edit
            .pointer("/range/start/character")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        #[allow(clippy::cast_possible_truncation)]
        let end_line = edit
            .pointer("/range/end/line")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        #[allow(clippy::cast_possible_truncation)]
        let end_char = edit
            .pointer("/range/end/character")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let new_text = edit.get("newText").and_then(Value::as_str).unwrap_or("");

        apply_single_edit(
            &mut lines, start_line, start_char, end_line, end_char, new_text,
        );
    }

    let mut new_content = lines.join("\n");
    if trailing_newline && !new_content.ends_with('\n') {
        new_content.push('\n');
    }

    // Atomic write: temp file + rename
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &new_content)
        .with_context(|| format!("failed to write temp file: {}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("failed to rename temp file to {}: {e}", path.display())
    })?;

    Ok(())
}

fn apply_single_edit(
    lines: &mut Vec<String>,
    start_line: usize,
    start_char: usize,
    end_line: usize,
    end_char: usize,
    new_text: &str,
) {
    // Extend lines if needed
    while lines.len() <= end_line {
        lines.push(String::new());
    }

    if start_line == end_line {
        let line = &lines[start_line];
        let byte_start = char_offset_to_byte(line, start_char);
        let byte_end = char_offset_to_byte(line, end_char);
        let mut combined = line[..byte_start].to_string();
        combined.push_str(new_text);
        combined.push_str(&line[byte_end..]);

        if new_text.contains('\n') {
            let new_lines: Vec<String> = combined.lines().map(str::to_string).collect();
            lines.splice(start_line..=start_line, new_lines);
        } else {
            lines[start_line] = combined;
        }
    } else {
        // Multi-line replacement
        let prefix = {
            let l = &lines[start_line];
            let b = char_offset_to_byte(l, start_char);
            l[..b].to_string()
        };
        let suffix = {
            let l = &lines[end_line];
            let b = char_offset_to_byte(l, end_char);
            l[b..].to_string()
        };
        let combined = format!("{prefix}{new_text}{suffix}");
        let new_lines: Vec<String> = combined.lines().map(str::to_string).collect();
        lines.splice(start_line..=end_line, new_lines);
    }
}

/// Convert a UTF-16 character offset to a byte offset in `s`.
fn char_offset_to_byte(s: &str, char_offset: usize) -> usize {
    s.char_indices()
        .nth(char_offset)
        .map_or(s.len(), |(i, _)| i)
}

/// Count the total number of `TextEdit` entries across all files in a `WorkspaceEdit`.
pub fn count_workspace_edits(edit: &Value) -> usize {
    let mut count = 0usize;
    if let Some(doc_changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for change in doc_changes {
            if let Some(edits) = change.get("edits").and_then(Value::as_array) {
                count += edits.len();
            }
        }
    } else if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for edits_val in changes.values() {
            if let Some(edits) = edits_val.as_array() {
                count += edits.len();
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_single_line_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        std::fs::write(&path, "fn hello() {}\nfn world() {}\n").unwrap();

        let edit = serde_json::json!({
            "changes": {
                format!("file://{}", path.display()): [
                    {
                        "range": {
                            "start": {"line": 0, "character": 3},
                            "end": {"line": 0, "character": 8}
                        },
                        "newText": "greet"
                    }
                ]
            }
        });

        let modified = apply_workspace_edit(&edit, dir.path()).unwrap();
        assert_eq!(modified.len(), 1);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("fn greet() {}"));
    }

    #[test]
    fn apply_multi_line_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        std::fs::write(&path, "fn a() {\n    let x = 1;\n}\n").unwrap();

        let edit = serde_json::json!({
            "changes": {
                format!("file://{}", path.display()): [
                    {
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 2, "character": 1}
                        },
                        "newText": "fn b() {}"
                    }
                ]
            }
        });

        apply_workspace_edit(&edit, dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("fn b() {}"));
    }

    #[test]
    fn count_workspace_edits_changes() {
        let edit = serde_json::json!({
            "changes": {
                "file:///a.rs": [{"range": {}, "newText": "x"}, {"range": {}, "newText": "y"}],
                "file:///b.rs": [{"range": {}, "newText": "z"}],
            }
        });
        assert_eq!(count_workspace_edits(&edit), 3);
    }

    #[test]
    fn count_workspace_edits_document_changes() {
        let edit = serde_json::json!({
            "documentChanges": [
                {
                    "textDocument": {"uri": "file:///a.rs"},
                    "edits": [{"range": {}, "newText": "x"}]
                }
            ]
        });
        assert_eq!(count_workspace_edits(&edit), 1);
    }
}
