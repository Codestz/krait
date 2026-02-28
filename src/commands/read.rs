use std::fmt::Write;
use std::path::Path;

use anyhow::{bail, Context};
use serde_json::{json, Value};

use super::DEFAULT_MAX_LINES;

use crate::lang::typescript as lang_ts;
use crate::lsp::client::LspClient;
use crate::lsp::files::FileTracker;
use crate::lsp::symbols::resolve_symbol_range;

/// Bytes to scan for binary detection.
const BINARY_SCAN_SIZE: usize = 8192;

/// Read a file with optional line range and `max_lines`.
///
/// Pure file I/O — no LSP needed.
///
/// # Errors
/// Returns an error if the file can't be read or is binary.
pub fn handle_read_file(
    path: &Path,
    from: Option<u32>,
    to: Option<u32>,
    max_lines: Option<u32>,
    project_root: &Path,
) -> anyhow::Result<Value> {
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };

    // Check file exists
    if !abs_path.exists() {
        bail!("file not found: {}", path.display());
    }

    // Binary detection: scan first 8KB for null bytes
    let raw =
        std::fs::read(&abs_path).with_context(|| format!("failed to read: {}", path.display()))?;

    let scan_len = raw.len().min(BINARY_SCAN_SIZE);
    if raw[..scan_len].contains(&0) {
        bail!("binary file: {}", path.display());
    }

    let content = String::from_utf8(raw)
        .with_context(|| format!("file is not valid UTF-8: {}", path.display()))?;

    let all_lines: Vec<&str> = content.lines().collect();
    #[allow(clippy::cast_possible_truncation)]
    let total = all_lines.len() as u32;

    // Apply from/to (1-indexed inclusive)
    let from_idx = from.unwrap_or(1).max(1).saturating_sub(1) as usize;
    let to_idx = to.map_or(all_lines.len(), |t| (t as usize).min(all_lines.len()));

    if from_idx >= all_lines.len() {
        bail!(
            "line {} is past end of file ({} lines)",
            from_idx + 1,
            total
        );
    }

    let selected = &all_lines[from_idx..to_idx];

    // Apply max_lines
    let max = max_lines.unwrap_or(DEFAULT_MAX_LINES) as usize;
    let truncated = selected.len() > max;
    let lines = if truncated {
        &selected[..max]
    } else {
        selected
    };

    // Format with cat -n style line numbers
    let numbered = format_numbered_lines(lines, from_idx + 1);

    let display_from = from_idx + 1;
    let display_to = from_idx + lines.len();

    let rel_path = abs_path
        .strip_prefix(project_root)
        .unwrap_or(&abs_path)
        .to_string_lossy()
        .to_string();

    Ok(json!({
        "path": rel_path,
        "content": numbered,
        "from": display_from,
        "to": display_to,
        "total": total,
        "truncated": truncated,
    }))
}

/// Read a symbol's body from source, using LSP to find its range.
///
/// Takes pre-found `SymbolMatch` candidates (from `workspace/symbol`) to avoid
/// duplicate queries. Tries each candidate's file via `documentSymbol` until
/// one resolves the symbol at the top level.
///
/// When `has_body` is true, skips overload stubs (1-2 line declarations ending in `;`
/// and `.d.ts` files) and returns the first candidate with a real implementation body.
/// Falls back to the first stub if no real body is found.
///
/// # Errors
/// Returns an error if the symbol can't be found or the file can't be read.
#[allow(clippy::too_many_arguments)]
pub async fn handle_read_symbol(
    name: &str,
    candidates: &[crate::commands::find::SymbolMatch],
    signature_only: bool,
    max_lines: Option<u32>,
    has_body: bool,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
) -> anyhow::Result<Value> {
    if candidates.is_empty() {
        bail!("symbol '{name}' not found");
    }

    let lookup_name = name.split('.').next().unwrap_or(name);
    let mut last_err = None;
    // Fallback when has_body=true but only stubs found
    let mut stub_fallback: Option<Value> = None;

    // Prioritise definition-like kinds over reference-like kinds.
    // JS `module.exports = { Foo }` produces a `property` candidate at the
    // exports line in addition to the actual `class`/`function` candidate.
    // Iterating property last ensures we read the real body first.
    let sorted: Vec<_> = {
        let (preferred, rest): (Vec<_>, Vec<_>) = candidates
            .iter()
            .partition(|s| !matches!(s.kind.as_str(), "property" | "variable" | "field"));
        preferred.into_iter().chain(rest).collect()
    };

    for sym in sorted {
        // Skip .d.ts declaration files when has_body is requested
        if has_body && sym.path.ends_with(".d.ts") {
            continue;
        }

        let abs = project_root.join(&sym.path);
        // Convert 1-indexed candidate line to 0-indexed hint for overload disambiguation
        let hint_line = sym.line.checked_sub(1);
        let loc =
            match resolve_symbol_range(lookup_name, &abs, hint_line, client, file_tracker).await {
                Ok(loc) => loc,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };

        // For dotted names (e.g. "Config.new"), resolve the nested part
        let location = if name.contains('.') {
            match resolve_symbol_range(name, &abs, hint_line, client, file_tracker).await {
                Ok(l) => l,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            }
        } else {
            loc
        };

        // Extract lines from file
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(e) => {
                last_err =
                    Some(anyhow::Error::from(e).context(format!("failed to read: {}", sym.path)));
                continue;
            }
        };

        let all_lines: Vec<&str> = content.lines().collect();
        let start = location.start_line as usize;
        let end = (location.end_line as usize + 1).min(all_lines.len());

        if start >= all_lines.len() {
            last_err = Some(anyhow::anyhow!("symbol range out of bounds"));
            continue;
        }

        let selected = &all_lines[start..end];

        let display_lines: &[&str] = if signature_only {
            let sig_end = selected
                .iter()
                .position(|l| l.contains('{'))
                .map_or(1, |i| i + 1);
            &selected[..sig_end.min(selected.len())]
        } else {
            selected
        };

        let max = max_lines.unwrap_or(DEFAULT_MAX_LINES) as usize;
        let truncated = display_lines.len() > max;
        let display_lines = if truncated {
            &display_lines[..max]
        } else {
            display_lines
        };

        let numbered = format_numbered_lines(display_lines, start + 1);
        let display_from = start + 1;
        let display_to = start + display_lines.len();

        let result = json!({
            "path": sym.path,
            "symbol": location.name,
            "kind": location.kind,
            "content": numbered,
            "from": display_from,
            "to": display_to,
            "truncated": truncated,
        });

        if has_body && lang_ts::is_overload_stub(selected) {
            if stub_fallback.is_none() {
                stub_fallback = Some(result);
            }
            continue;
        }

        return Ok(result);
    }

    // has_body requested but only stubs found — return first stub rather than nothing
    if let Some(fallback) = stub_fallback {
        return Ok(fallback);
    }

    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("symbol '{name}' not found in document symbols")))
}

/// Format lines with `cat -n` style numbering.
pub(crate) fn format_numbered_lines(lines: &[&str], start_num: usize) -> String {
    let last_num = start_num + lines.len();
    let width = last_num.to_string().len().max(4);

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let num = start_num + i;
        let _ = writeln!(out, "{num:>width$}\t{line}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_detection_rejects_null_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("binary.bin");
        std::fs::write(&file, b"hello\x00world").unwrap();

        let result = handle_read_file(&file, None, None, None, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("binary file"));
    }

    #[test]
    fn read_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "line1\nline2\nline3\nline4\nline5\n").unwrap();

        let result = handle_read_file(Path::new("test.txt"), None, None, None, dir.path()).unwrap();

        assert_eq!(result["total"], 5);
        assert_eq!(result["from"], 1);
        assert_eq!(result["to"], 5);
        assert_eq!(result["truncated"], false);
        assert!(result["content"].as_str().unwrap().contains("line1"));
    }

    #[test]
    fn read_file_with_range() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

        let result =
            handle_read_file(Path::new("test.txt"), Some(2), Some(4), None, dir.path()).unwrap();

        assert_eq!(result["from"], 2);
        assert_eq!(result["to"], 4);
        let content = result["content"].as_str().unwrap();
        assert!(content.contains('b'));
        assert!(content.contains('c'));
        assert!(content.contains('d'));
        // Should not contain lines outside range
        assert!(!content.contains("\ta\n"));
        assert!(!content.contains("\te\n"));
    }

    #[test]
    fn read_file_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        let mut content = String::new();
        for i in 1..=10 {
            use std::fmt::Write;
            let _ = writeln!(content, "line{i}");
        }
        std::fs::write(&file, content).unwrap();

        let result =
            handle_read_file(Path::new("test.txt"), None, None, Some(3), dir.path()).unwrap();

        assert_eq!(result["truncated"], true);
        assert_eq!(result["to"], 3);
    }

    #[test]
    fn read_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = handle_read_file(Path::new("nonexistent.txt"), None, None, None, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn format_numbered_lines_basic() {
        let lines = vec!["hello", "world"];
        let out = format_numbered_lines(&lines, 1);
        assert!(out.contains("   1\thello\n"));
        assert!(out.contains("   2\tworld\n"));
    }

    #[test]
    fn format_numbered_lines_offset() {
        let lines = vec!["a", "b"];
        let out = format_numbered_lines(&lines, 98);
        assert!(out.contains("  98\ta\n"));
        assert!(out.contains("  99\tb\n"));
    }

    #[test]
    fn read_file_past_end() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();

        let result = handle_read_file(Path::new("test.txt"), Some(100), None, None, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("past end"));
    }
}
