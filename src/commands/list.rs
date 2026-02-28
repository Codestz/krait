use std::path::Path;

use anyhow::Context;
use serde_json::{json, Value};

use crate::lsp::client::LspClient;
use crate::lsp::files::FileTracker;

/// A symbol in the document outline.
#[derive(Debug, serde::Serialize)]
pub struct SymbolEntry {
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub end_line: u32,
    pub children: Vec<SymbolEntry>,
}

/// List symbols in a file using `textDocument/documentSymbol`.
///
/// # Errors
/// Returns an error if the file can't be opened or the LSP request fails.
pub async fn list_symbols(
    file_path: &Path,
    depth: u8,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    project_root: &Path,
) -> anyhow::Result<Vec<SymbolEntry>> {
    let abs_path = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        project_root.join(file_path)
    };

    file_tracker
        .ensure_open(&abs_path, client.transport_mut())
        .await
        .with_context(|| format!("failed to open: {}", file_path.display()))?;

    let uri = crate::lsp::client::path_to_uri(&abs_path)?;
    let params = json!({
        "textDocument": { "uri": uri.as_str() }
    });

    let request_id = client
        .transport_mut()
        .send_request("textDocument/documentSymbol", params)
        .await?;

    let response = client
        .wait_for_response_public(request_id)
        .await
        .context("textDocument/documentSymbol request failed")?;

    let symbols = parse_document_symbols(&response, depth, 1);
    Ok(symbols)
}

fn parse_document_symbols(value: &Value, max_depth: u8, current_depth: u8) -> Vec<SymbolEntry> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };

    let mut results = Vec::new();
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let kind = crate::commands::find::symbol_kind_name(
            item.get("kind").and_then(Value::as_u64).unwrap_or(0),
        )
        .to_string();

        #[allow(clippy::cast_possible_truncation)]
        let line = item
            .pointer("/range/start/line")
            .or_else(|| item.pointer("/location/range/start/line"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32
            + 1;

        #[allow(clippy::cast_possible_truncation)]
        let end_line = item
            .pointer("/range/end/line")
            .or_else(|| item.pointer("/location/range/end/line"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32
            + 1;

        let children = if current_depth < max_depth {
            item.get("children")
                .map(|c| parse_document_symbols(c, max_depth, current_depth + 1))
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        results.push(SymbolEntry {
            name,
            kind,
            line,
            end_line,
            children,
        });
    }

    results
}

/// Format symbols as compact output with indentation.
#[must_use]
pub fn format_compact(symbols: &[SymbolEntry], indent: usize) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for sym in symbols {
        let prefix = "  ".repeat(indent);
        let _ = writeln!(out, "{prefix}{} {}", sym.kind, sym.name);
        if !sym.children.is_empty() {
            out.push_str(&format_compact(&sym.children, indent + 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_response() {
        let response = json!(null);
        let symbols = parse_document_symbols(&response, 2, 1);
        assert!(symbols.is_empty());
    }

    #[test]
    fn parse_nested_symbols() {
        let response = json!([
            {
                "name": "Config",
                "kind": 23,
                "range": { "start": { "line": 4, "character": 0 }, "end": { "line": 7, "character": 1 } },
                "selectionRange": { "start": { "line": 4, "character": 11 }, "end": { "line": 4, "character": 17 } },
                "children": [
                    {
                        "name": "name",
                        "kind": 8,
                        "range": { "start": { "line": 5, "character": 4 }, "end": { "line": 5, "character": 20 } },
                        "selectionRange": { "start": { "line": 5, "character": 8 }, "end": { "line": 5, "character": 12 } }
                    }
                ]
            }
        ]);

        let symbols = parse_document_symbols(&response, 2, 1);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Config");
        assert_eq!(symbols[0].kind, "struct");
        assert_eq!(symbols[0].children.len(), 1);
        assert_eq!(symbols[0].children[0].name, "name");
        assert_eq!(symbols[0].children[0].kind, "field");
    }

    #[test]
    fn depth_1_excludes_children() {
        let response = json!([
            {
                "name": "Config",
                "kind": 23,
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 1 } },
                "selectionRange": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 6 } },
                "children": [
                    {
                        "name": "field",
                        "kind": 8,
                        "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 10 } },
                        "selectionRange": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } }
                    }
                ]
            }
        ]);

        let symbols = parse_document_symbols(&response, 1, 1);
        assert_eq!(symbols.len(), 1);
        assert!(symbols[0].children.is_empty());
    }

    #[test]
    fn format_compact_output() {
        let symbols = vec![
            SymbolEntry {
                name: "greet".into(),
                kind: "function".into(),
                line: 1,
                end_line: 3,
                children: vec![],
            },
            SymbolEntry {
                name: "Config".into(),
                kind: "struct".into(),
                line: 5,
                end_line: 8,
                children: vec![SymbolEntry {
                    name: "name".into(),
                    kind: "field".into(),
                    line: 6,
                    end_line: 6,
                    children: vec![],
                }],
            },
        ];

        let out = format_compact(&symbols, 0);
        assert!(out.contains("function greet"));
        assert!(out.contains("struct Config"));
        assert!(out.contains("  field name"));
    }
}
