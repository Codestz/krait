use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;

/// Severity of a diagnostic, ordered from most to least severe.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

impl DiagSeverity {
    fn from_lsp(raw: Option<u64>) -> Self {
        match raw {
            Some(1) => Self::Error,
            Some(2) => Self::Warning,
            Some(3) => Self::Information,
            _ => Self::Hint,
        }
    }

    /// Short label for compact output.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warn",
            Self::Information => "info",
            Self::Hint => "hint",
        }
    }
}

/// A single diagnostic from a language server.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticEntry {
    pub severity: DiagSeverity,
    /// 0-indexed line number (from LSP).
    pub line: u32,
    /// 0-indexed column (from LSP).
    pub col: u32,
    pub code: Option<String>,
    pub message: String,
}

/// Thread-safe per-file diagnostic store.
///
/// Receives `textDocument/publishDiagnostics` notifications and stores them
/// by absolute file path. Each update replaces previous diagnostics for that file.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticStore {
    inner: Arc<DashMap<PathBuf, Vec<DiagnosticEntry>>>,
}

impl DiagnosticStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace all diagnostics for `path`. Passing an empty vec removes the entry.
    pub fn update(&self, path: PathBuf, diags: Vec<DiagnosticEntry>) {
        if diags.is_empty() {
            self.inner.remove(&path);
        } else {
            self.inner.insert(path, diags);
        }
    }

    /// All diagnostics for `path`, or empty vec if none.
    #[must_use]
    pub fn get(&self, path: &Path) -> Vec<DiagnosticEntry> {
        self.inner.get(path).map(|v| v.clone()).unwrap_or_default()
    }

    /// All diagnostics across all files.
    #[must_use]
    pub fn get_all(&self) -> Vec<(PathBuf, Vec<DiagnosticEntry>)> {
        self.inner
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Remove diagnostics for a file.
    pub fn clear(&self, path: &Path) {
        self.inner.remove(path);
    }

    /// Total number of diagnostic entries across all files.
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.inner.iter().map(|e| e.value().len()).sum()
    }
}

/// Ingest a `textDocument/publishDiagnostics` notification params into `store`.
pub fn ingest_publish_diagnostics(params: Option<Value>, store: &DiagnosticStore) {
    let Some(params) = params else { return };
    let Some(uri) = params.get("uri").and_then(|v| v.as_str()) else {
        return;
    };
    let path = uri_to_path(uri);

    let Some(diags_raw) = params.get("diagnostics").and_then(|v| v.as_array()) else {
        store.update(path, vec![]);
        return;
    };

    let entries: Vec<DiagnosticEntry> = diags_raw.iter().filter_map(parse_entry).collect();
    store.update(path, entries);
}

fn parse_entry(v: &Value) -> Option<DiagnosticEntry> {
    let message = v.get("message").and_then(|m| m.as_str())?.to_string();
    let severity = DiagSeverity::from_lsp(v.get("severity").and_then(Value::as_u64));
    let start = v.get("range").and_then(|r| r.get("start"));
    let line = u32::try_from(
        start
            .and_then(|s| s.get("line"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
    .unwrap_or(0);
    let col = u32::try_from(
        start
            .and_then(|s| s.get("character"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
    .unwrap_or(0);
    let code = v.get("code").and_then(|c| {
        if let Some(s) = c.as_str() {
            Some(s.to_string())
        } else {
            c.as_u64().map(|n| n.to_string())
        }
    });
    Some(DiagnosticEntry { severity, line, col, code, message })
}

/// Strip `file://` prefix from a URI and return a `PathBuf`.
fn uri_to_path(uri: &str) -> PathBuf {
    PathBuf::from(uri.strip_prefix("file://").unwrap_or(uri))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn err(msg: &str) -> DiagnosticEntry {
        DiagnosticEntry {
            severity: DiagSeverity::Error,
            line: 1,
            col: 0,
            code: None,
            message: msg.to_string(),
        }
    }

    #[test]
    fn store_and_retrieve_diagnostics() {
        let store = DiagnosticStore::new();
        let path = PathBuf::from("/project/src/lib.rs");
        store.update(path.clone(), vec![err("oops")]);
        let diags = store.get(&path);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "oops");
    }

    #[test]
    fn update_replaces_previous() {
        let store = DiagnosticStore::new();
        let path = PathBuf::from("/project/src/lib.rs");
        store.update(path.clone(), vec![err("first")]);
        store.update(
            path.clone(),
            vec![DiagnosticEntry {
                severity: DiagSeverity::Warning,
                line: 2,
                col: 0,
                code: None,
                message: "second".to_string(),
            }],
        );
        let diags = store.get(&path);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "second");
    }

    #[test]
    fn get_nonexistent_returns_empty() {
        let store = DiagnosticStore::new();
        assert!(store.get(&PathBuf::from("/nonexistent.rs")).is_empty());
    }

    #[test]
    fn get_all_returns_everything() {
        let store = DiagnosticStore::new();
        store.update(PathBuf::from("/a.rs"), vec![err("a")]);
        store.update(PathBuf::from("/b.rs"), vec![err("b")]);
        assert_eq!(store.get_all().len(), 2);
    }

    #[test]
    fn update_empty_clears_entry() {
        let store = DiagnosticStore::new();
        let path = PathBuf::from("/project/lib.rs");
        store.update(path.clone(), vec![err("e")]);
        store.update(path.clone(), vec![]);
        assert!(store.get(&path).is_empty());
    }

    #[test]
    fn ingest_publish_diagnostics_parses_notification() {
        let store = DiagnosticStore::new();
        let params = json!({
            "uri": "file:///project/src/lib.rs",
            "diagnostics": [{
                "range": {"start": {"line": 41, "character": 9}, "end": {"line": 41, "character": 15}},
                "severity": 1,
                "code": "E0308",
                "message": "mismatched types"
            }]
        });
        ingest_publish_diagnostics(Some(params), &store);
        let diags = store.get(&PathBuf::from("/project/src/lib.rs"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, DiagSeverity::Error);
        assert_eq!(diags[0].line, 41);
        assert_eq!(diags[0].code.as_deref(), Some("E0308"));
        assert_eq!(diags[0].message, "mismatched types");
    }

    #[test]
    fn ingest_clears_on_empty_array() {
        let store = DiagnosticStore::new();
        let path = PathBuf::from("/project/src/lib.rs");
        store.update(path.clone(), vec![err("old")]);
        let params = json!({
            "uri": "file:///project/src/lib.rs",
            "diagnostics": []
        });
        ingest_publish_diagnostics(Some(params), &store);
        assert!(store.get(&path).is_empty());
    }

    #[test]
    fn severity_ordering_errors_first() {
        assert!(DiagSeverity::Error < DiagSeverity::Warning);
        assert!(DiagSeverity::Warning < DiagSeverity::Information);
        assert!(DiagSeverity::Information < DiagSeverity::Hint);
    }
}
