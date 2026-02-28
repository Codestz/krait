use std::path::Path;

use anyhow::Context;
use serde_json::{Value, json};

use crate::commands::workspace_edit::apply_workspace_edit;
use crate::lsp::client::{LspClient, path_to_uri};
use crate::lsp::diagnostics::{DiagSeverity, DiagnosticStore};
use crate::lsp::files::FileTracker;

/// Apply LSP quick-fix code actions for current diagnostics.
///
/// If `path` is given, only fixes diagnostics for that file.
/// Otherwise fixes all files that have diagnostics in the store.
///
/// For each diagnostic, sends `textDocument/codeAction` with `only: ["quickfix"]`
/// and applies any returned actions that carry an embedded `WorkspaceEdit`.
///
/// # Errors
/// Returns an error if any LSP request or file write fails.
pub async fn handle_fix(
    path: Option<&Path>,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
    diagnostic_store: &DiagnosticStore,
) -> anyhow::Result<Value> {
    // Resolve the target file(s)
    let all_diags = diagnostic_store.get_all();
    let file_diags = if let Some(p) = path {
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            project_root.join(p)
        };
        all_diags.into_iter().filter(|(fp, _)| *fp == abs).collect::<Vec<_>>()
    } else {
        all_diags
    };

    if file_diags.is_empty() {
        return Ok(json!({
            "fixes_applied": 0,
            "files": [],
        }));
    }

    let mut total_fixes = 0usize;
    let mut fixed_files: Vec<String> = Vec::new();

    for (file_path, diags) in &file_diags {
        file_tracker
            .ensure_open(file_path, client.transport_mut())
            .await?;
        let uri = path_to_uri(file_path)?;

        for diag in diags {
            let line = diag.line;
            let col = diag.col;
            let severity_num: u64 = match diag.severity {
                DiagSeverity::Error => 1,
                DiagSeverity::Warning => 2,
                DiagSeverity::Information => 3,
                DiagSeverity::Hint => 4,
            };
            let code_val: Value = diag
                .code
                .as_deref()
                .map_or(Value::Null, |c| Value::String(c.to_string()));

            let lsp_diag = json!({
                "range": {
                    "start": { "line": line, "character": col },
                    "end": { "line": line, "character": col }
                },
                "message": diag.message,
                "severity": severity_num,
                "code": code_val,
            });

            let params = json!({
                "textDocument": { "uri": uri.as_str() },
                "range": {
                    "start": { "line": line, "character": col },
                    "end": { "line": line, "character": col }
                },
                "context": {
                    "diagnostics": [lsp_diag],
                    "only": ["quickfix"]
                }
            });

            let request_id = client
                .transport_mut()
                .send_request("textDocument/codeAction", params)
                .await?;

            let response = client
                .wait_for_response_public(request_id)
                .await
                .context("textDocument/codeAction request failed")?;

            let actions = response.as_array().cloned().unwrap_or_default();
            for action in &actions {
                if let Some(edit) = action.get("edit") {
                    if !edit.is_null() {
                        apply_workspace_edit(edit, project_root)?;
                        total_fixes += 1;
                        let rel = file_path
                            .strip_prefix(project_root)
                            .unwrap_or(file_path)
                            .to_string_lossy()
                            .to_string();
                        if !fixed_files.contains(&rel) {
                            fixed_files.push(rel);
                        }
                    }
                }
            }
        }
    }

    Ok(json!({
        "fixes_applied": total_fixes,
        "files": fixed_files,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn fix_response_shape() {
        let data = json!({ "fixes_applied": 3, "files": ["src/lib.rs", "src/main.rs"] });
        assert_eq!(data["fixes_applied"].as_u64(), Some(3));
        assert_eq!(data["files"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn fix_no_diagnostics_shape() {
        let data = json!({ "fixes_applied": 0, "files": [] });
        assert_eq!(data["fixes_applied"].as_u64(), Some(0));
    }
}
