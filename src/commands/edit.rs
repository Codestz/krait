use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::commands::find::{SymbolMatch, find_symbol};
use crate::index::watcher::DirtyFiles;
use crate::lsp::client::LspClient;
use crate::lsp::files::FileTracker;
use crate::lsp::symbols::{SymbolLocation, resolve_symbol_range};

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Locate a symbol and return its absolute file path + resolved range.
///
/// Mirrors the candidate-iteration logic from `handle_read_symbol`.
async fn locate_symbol(
    name: &str,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
) -> anyhow::Result<(PathBuf, SymbolLocation)> {
    let search_name = name.split('.').next().unwrap_or(name);

    let candidates: Vec<SymbolMatch> =
        find_symbol(search_name, client, project_root).await?;

    if candidates.is_empty() {
        bail!("symbol '{name}' not found");
    }

    let mut last_err: Option<anyhow::Error> = None;
    for sym in &candidates {
        let abs = project_root.join(&sym.path);
        let hint_line = sym.line.checked_sub(1);
        match resolve_symbol_range(search_name, &abs, hint_line, client, file_tracker).await {
            Ok(loc) => {
                let location = if name.contains('.') {
                    resolve_symbol_range(name, &abs, hint_line, client, file_tracker).await?
                } else {
                    loc
                };
                return Ok((abs, location));
            }
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("symbol '{name}' not found")))
}

/// Atomically write `contents` to `path`.
///
/// Writes to a sibling `.tmp` file first, then renames so the write is atomic.
fn atomic_write(path: &Path, contents: &str) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)
        .with_context(|| format!("failed to write temp file: {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to rename temp file to: {}", path.display())
    })?;
    Ok(())
}

/// Mark a file dirty in the watcher so the index is refreshed on next query.
fn mark_dirty(abs_path: &Path, project_root: &Path, dirty_files: &DirtyFiles) {
    if let Ok(rel) = abs_path.strip_prefix(project_root) {
        dirty_files.mark_dirty(rel.to_string_lossy().into_owned());
    }
}

/// Ensure a trailing newline in content if missing.
fn ensure_trailing_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

// ── edit replace ─────────────────────────────────────────────────────────────

/// Replace a symbol's body with `code`.
///
/// # Errors
/// Returns an error if the symbol can't be found or the file can't be written.
pub async fn handle_edit_replace(
    name: &str,
    code: &str,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
    dirty_files: &DirtyFiles,
) -> anyhow::Result<Value> {
    let (abs_path, location) = locate_symbol(name, client, file_tracker, project_root).await?;

    let content = std::fs::read_to_string(&abs_path)
        .with_context(|| format!("failed to read: {}", abs_path.display()))?;

    let mut lines: Vec<&str> = content.lines().collect();

    let start = location.start_line as usize;
    let end = (location.end_line as usize + 1).min(lines.len());

    if start >= lines.len() {
        bail!("symbol range out of bounds in {}", abs_path.display());
    }

    let original_count = end - start;
    let new_lines: Vec<&str> = code.lines().collect();
    let new_count = new_lines.len();

    // Replace lines [start..end] with new_lines
    lines.splice(start..end, new_lines.iter().copied());

    let new_content = ensure_trailing_newline(&lines.join("\n"));
    atomic_write(&abs_path, &new_content)?;
    mark_dirty(&abs_path, project_root, dirty_files);

    let rel_path = abs_path
        .strip_prefix(project_root)
        .unwrap_or(&abs_path)
        .to_string_lossy()
        .to_string();

    Ok(json!({
        "path": rel_path,
        "symbol": name,
        "from": start + 1,
        "to": end,
        "lines_before": original_count,
        "lines_after": new_count,
    }))
}

// ── edit insert-after ─────────────────────────────────────────────────────────

/// Insert `code` immediately after a symbol's end line.
///
/// Adds a blank line separator if the line after the symbol is not already blank.
///
/// # Errors
/// Returns an error if the symbol can't be found or the file can't be written.
pub async fn handle_edit_insert_after(
    name: &str,
    code: &str,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
    dirty_files: &DirtyFiles,
) -> anyhow::Result<Value> {
    let (abs_path, location) = locate_symbol(name, client, file_tracker, project_root).await?;

    let content = std::fs::read_to_string(&abs_path)
        .with_context(|| format!("failed to read: {}", abs_path.display()))?;

    let mut lines: Vec<&str> = content.lines().collect();
    let insert_at = (location.end_line as usize + 1).min(lines.len());

    // Add blank separator if next line is not already blank
    let needs_blank = lines.get(insert_at).is_some_and(|l| !l.trim().is_empty());

    let new_lines: Vec<&str> = code.lines().collect();
    let insert_count = new_lines.len();

    if needs_blank {
        lines.splice(insert_at..insert_at, std::iter::once("").chain(new_lines.iter().copied()));
    } else {
        lines.splice(insert_at..insert_at, new_lines.iter().copied());
    }

    let new_content = ensure_trailing_newline(&lines.join("\n"));
    atomic_write(&abs_path, &new_content)?;
    mark_dirty(&abs_path, project_root, dirty_files);

    let rel_path = abs_path
        .strip_prefix(project_root)
        .unwrap_or(&abs_path)
        .to_string_lossy()
        .to_string();

    Ok(json!({
        "path": rel_path,
        "symbol": name,
        "operation": "after",
        "inserted_at": insert_at + 1,
        "lines_added": insert_count,
    }))
}

// ── edit insert-before ────────────────────────────────────────────────────────

/// Insert `code` before a symbol, skipping any leading attributes/decorators/doc comments.
///
/// Scans upward from the symbol's start line to find `#[...]`, `@decorator`,
/// or `///`/`//!` doc comment lines, and inserts before those.
///
/// # Errors
/// Returns an error if the symbol can't be found or the file can't be written.
pub async fn handle_edit_insert_before(
    name: &str,
    code: &str,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
    dirty_files: &DirtyFiles,
) -> anyhow::Result<Value> {
    let (abs_path, location) = locate_symbol(name, client, file_tracker, project_root).await?;

    let content = std::fs::read_to_string(&abs_path)
        .with_context(|| format!("failed to read: {}", abs_path.display()))?;

    let mut lines: Vec<&str> = content.lines().collect();

    // Walk upward from symbol start to skip over attributes/decorators/doc comments
    let symbol_start = location.start_line as usize;
    let insert_at = find_insert_before_line(&lines, symbol_start);

    let new_lines: Vec<&str> = code.lines().collect();
    let insert_count = new_lines.len();

    // Insert code + blank separator before the target line
    let with_sep: Vec<&str> = new_lines.iter().copied().chain(std::iter::once("")).collect();
    lines.splice(insert_at..insert_at, with_sep.iter().copied());

    let new_content = ensure_trailing_newline(&lines.join("\n"));
    atomic_write(&abs_path, &new_content)?;
    mark_dirty(&abs_path, project_root, dirty_files);

    let rel_path = abs_path
        .strip_prefix(project_root)
        .unwrap_or(&abs_path)
        .to_string_lossy()
        .to_string();

    Ok(json!({
        "path": rel_path,
        "symbol": name,
        "operation": "before",
        "inserted_at": insert_at + 1,
        "lines_added": insert_count,
    }))
}

/// Find the line index to insert before, walking upward past attributes/doc comments.
fn find_insert_before_line(lines: &[&str], symbol_start: usize) -> usize {
    if symbol_start == 0 {
        return 0;
    }

    let mut cursor = symbol_start;

    // Walk upward while lines look like attributes, decorators, or doc comments
    loop {
        if cursor == 0 {
            break;
        }
        let prev = cursor - 1;
        let trimmed = lines[prev].trim();

        let is_attr_or_doc = trimmed.starts_with("#[")
            || trimmed.starts_with('@')
            || trimmed.starts_with("///")
            || trimmed.starts_with("//!")
            || trimmed.starts_with("/**")
            || trimmed.starts_with("* ")
            || trimmed == "*/"
            || trimmed.starts_with("/*");

        if is_attr_or_doc {
            cursor = prev;
        } else {
            break;
        }
    }

    cursor
}

// ── Output formatting ─────────────────────────────────────────────────────────

/// Format an edit replace response for compact output.
#[must_use]
pub fn format_replace(data: &Value) -> String {
    let path = data["path"].as_str().unwrap_or("?");
    let symbol = data["symbol"].as_str().unwrap_or("?");
    let from = data["from"].as_u64().unwrap_or(0);
    let to = data["to"].as_u64().unwrap_or(0);
    let before = data["lines_before"].as_u64().unwrap_or(0);
    let after = data["lines_after"].as_u64().unwrap_or(0);
    format!("replaced {path}:{from}-{to} {symbol} ({before} lines → {after} lines)")
}

/// Format an insert response for compact output.
#[must_use]
pub fn format_insert(data: &Value, kind: &str) -> String {
    let path = data["path"].as_str().unwrap_or("?");
    let symbol = data["symbol"].as_str().unwrap_or("?");
    let at = data["inserted_at"].as_u64().unwrap_or(0);
    let count = data["lines_added"].as_u64().unwrap_or(0);
    format!("inserted {kind} {path}:{at} {symbol} ({count} lines added at line {at})")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_tmp(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn atomic_write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rs");
        atomic_write(&path, "fn hello() {}").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "fn hello() {}");
    }

    #[test]
    fn atomic_write_no_tmp_left_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rs");
        atomic_write(&path, "fn hello() {}").unwrap();
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn find_insert_before_skips_attributes() {
        let lines = vec![
            "fn unrelated() {}",  // 0
            "",                    // 1
            "#[derive(Debug)]",    // 2
            "#[allow(dead_code)]", // 3
            "struct Foo {",        // 4
            "}",                   // 5
        ];
        // Symbol starts at line 4; should insert before line 2
        assert_eq!(find_insert_before_line(&lines, 4), 2);
    }

    #[test]
    fn find_insert_before_skips_doc_comments() {
        let lines = vec![
            "fn other() {}",  // 0
            "",               // 1
            "/// My doc",     // 2
            "fn target() {}", // 3
        ];
        assert_eq!(find_insert_before_line(&lines, 3), 2);
    }

    #[test]
    fn find_insert_before_no_attrs_returns_symbol_start() {
        let lines = vec![
            "fn a() {}", // 0
            "",          // 1
            "fn b() {}", // 2
        ];
        assert_eq!(find_insert_before_line(&lines, 2), 2);
    }

    #[test]
    fn find_insert_before_at_start_of_file() {
        let lines = vec!["fn only() {}"];
        assert_eq!(find_insert_before_line(&lines, 0), 0);
    }

    #[test]
    fn ensure_trailing_newline_adds_newline() {
        assert_eq!(ensure_trailing_newline("hello"), "hello\n");
    }

    #[test]
    fn ensure_trailing_newline_no_double_newline() {
        assert_eq!(ensure_trailing_newline("hello\n"), "hello\n");
    }

    #[test]
    fn format_replace_output() {
        let data = json!({
            "path": "src/lib.rs",
            "symbol": "greet",
            "from": 5,
            "to": 15,
            "lines_before": 11,
            "lines_after": 8,
        });
        let out = format_replace(&data);
        assert!(out.contains("replaced"));
        assert!(out.contains("src/lib.rs:5-15"));
        assert!(out.contains("greet"));
        assert!(out.contains("11 lines → 8 lines"));
    }

    #[test]
    fn format_insert_after_output() {
        let data = json!({
            "path": "src/lib.rs",
            "symbol": "greet",
            "inserted_at": 16,
            "lines_added": 5,
        });
        let out = format_insert(&data, "after");
        assert!(out.contains("inserted after"));
        assert!(out.contains("src/lib.rs:16"));
        assert!(out.contains("5 lines added"));
    }
}
