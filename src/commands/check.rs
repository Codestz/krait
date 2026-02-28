use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::lsp::diagnostics::{DiagSeverity, DiagnosticStore};

/// Handle the `krait check` command.
///
/// Returns diagnostics (optionally filtered to `path` and/or errors-only),
/// sorted by severity then `file:line`. Paths in the response are relative to `project_root`.
#[must_use]
pub fn handle_check(
    path: Option<&Path>,
    store: &DiagnosticStore,
    project_root: &Path,
    errors_only: bool,
) -> Value {
    let mut entries: Vec<(PathBuf, DiagSeverity, u32, u32, Option<String>, String)> = vec![];

    if let Some(filter) = path {
        let abs = if filter.is_absolute() {
            filter.to_path_buf()
        } else {
            project_root.join(filter)
        };
        for d in store.get(&abs) {
            if errors_only && d.severity != DiagSeverity::Error {
                continue;
            }
            entries.push((abs.clone(), d.severity, d.line, d.col, d.code, d.message));
        }
    } else {
        for (file_path, diags) in store.get_all() {
            for d in diags {
                if errors_only && d.severity != DiagSeverity::Error {
                    continue;
                }
                entries.push((
                    file_path.clone(),
                    d.severity,
                    d.line,
                    d.col,
                    d.code,
                    d.message,
                ));
            }
        }
    }

    // Sort: severity (Error < Warning < ...) → path → line
    entries.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.2.cmp(&b.2))
    });

    let mut errors: u64 = 0;
    let mut warnings: u64 = 0;
    let items: Vec<Value> = entries
        .iter()
        .map(|(file_path, sev, line, col, code, msg)| {
            match sev {
                DiagSeverity::Error => errors += 1,
                DiagSeverity::Warning => warnings += 1,
                _ => {}
            }
            let rel = file_path.strip_prefix(project_root).unwrap_or(file_path);
            json!({
                "severity": sev.label(),
                "path": rel.to_string_lossy(),
                // Convert 0-indexed LSP positions to 1-indexed display
                "line": line + 1,
                "col": col + 1,
                "code": code,
                "message": msg,
            })
        })
        .collect();

    let total = items.len() as u64;
    json!({
        "diagnostics": items,
        "total": total,
        "errors": errors,
        "warnings": warnings,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::lsp::diagnostics::{DiagSeverity, DiagnosticEntry, DiagnosticStore};

    fn root() -> PathBuf {
        PathBuf::from("/project")
    }

    fn make_store() -> DiagnosticStore {
        let store = DiagnosticStore::new();
        store.update(
            PathBuf::from("/project/src/lib.rs"),
            vec![
                DiagnosticEntry {
                    severity: DiagSeverity::Warning,
                    line: 2,
                    col: 4,
                    code: None,
                    message: "unused import".to_string(),
                },
                DiagnosticEntry {
                    severity: DiagSeverity::Error,
                    line: 41,
                    col: 9,
                    code: Some("E0308".to_string()),
                    message: "mismatched types".to_string(),
                },
            ],
        );
        store
    }

    #[test]
    fn check_formats_errors_first() {
        let store = make_store();
        let result = handle_check(None, &store, &root(), false);
        let diags = result["diagnostics"].as_array().unwrap();
        // First entry should be the error (E0308 on line 42)
        assert_eq!(diags[0]["severity"], "error");
        assert_eq!(diags[1]["severity"], "warn");
    }

    #[test]
    fn check_empty_is_clean() {
        let store = DiagnosticStore::new();
        let result = handle_check(None, &store, &root(), false);
        assert_eq!(result["total"], 0);
        assert!(result["diagnostics"].as_array().unwrap().is_empty());
    }

    #[test]
    fn check_filters_by_path() {
        let store = make_store();
        store.update(
            PathBuf::from("/project/src/main.rs"),
            vec![DiagnosticEntry {
                severity: DiagSeverity::Error,
                line: 5,
                col: 0,
                code: None,
                message: "other error".to_string(),
            }],
        );
        let result = handle_check(Some(Path::new("src/lib.rs")), &store, &root(), false);
        let diags = result["diagnostics"].as_array().unwrap();
        assert_eq!(diags.len(), 2, "should only return lib.rs diagnostics");
        for d in diags {
            assert_eq!(d["path"], "src/lib.rs");
        }
    }

    #[test]
    fn check_line_is_one_indexed() {
        let store = make_store();
        let result = handle_check(None, &store, &root(), false);
        let diags = result["diagnostics"].as_array().unwrap();
        let error = diags.iter().find(|d| d["severity"] == "error").unwrap();
        // LSP line 41 → display line 42
        assert_eq!(error["line"], 42);
    }

    #[test]
    fn check_counts_errors_and_warnings() {
        let store = make_store();
        let result = handle_check(None, &store, &root(), false);
        assert_eq!(result["errors"], 1);
        assert_eq!(result["warnings"], 1);
        assert_eq!(result["total"], 2);
    }

    #[test]
    fn check_errors_only_suppresses_warnings() {
        let store = make_store();
        let result = handle_check(None, &store, &root(), true);
        let diags = result["diagnostics"].as_array().unwrap();
        // Only the error should remain
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0]["severity"], "error");
        assert_eq!(result["total"], 1);
    }

    #[test]
    fn check_errors_only_clean_project_is_empty() {
        let store = DiagnosticStore::new();
        store.update(
            PathBuf::from("/project/src/lib.rs"),
            vec![DiagnosticEntry {
                severity: DiagSeverity::Warning,
                line: 0,
                col: 0,
                code: None,
                message: "unused import".to_string(),
            }],
        );
        // errors_only — the single warning should be suppressed → "No diagnostics"
        let result = handle_check(None, &store, &root(), true);
        assert_eq!(result["total"], 0);
        assert!(result["diagnostics"].as_array().unwrap().is_empty());
    }
}
