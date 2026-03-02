use std::path::Path;

use anyhow::{bail, Context};
use serde_json::{json, Value};

use super::client::LspClient;
use super::files::FileTracker;
use crate::commands::find::symbol_kind_name;

/// A resolved symbol location with exact line ranges.
#[derive(Debug)]
pub struct SymbolLocation {
    pub name: String,
    pub kind: String,
    /// 0-indexed start line.
    pub start_line: u32,
    /// 0-indexed end line (inclusive).
    pub end_line: u32,
    pub children: Vec<SymbolLocation>,
}

/// Resolve a symbol's exact range using `textDocument/documentSymbol`.
///
/// Supports nested names like `Config.new` by walking children.
///
/// `hint_line` (0-indexed) is used to disambiguate overloads: when multiple
/// symbols share the same name (TypeScript overload stubs + implementation),
/// the one whose `start_line` is closest to `hint_line` is selected.
///
/// # Errors
/// Returns an error if the file can't be opened or the symbol isn't found.
///
/// # Panics
/// Panics if `hint_line` is `Some` but `all_matches` is unexpectedly empty after
/// the emptiness check — this is a logic invariant that should never fire.
pub async fn resolve_symbol_range(
    name: &str,
    file_path: &Path,
    hint_line: Option<u32>,
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
) -> anyhow::Result<SymbolLocation> {
    file_tracker
        .ensure_open(file_path, client.transport_mut())
        .await
        .with_context(|| format!("failed to open: {}", file_path.display()))?;

    let uri = super::client::path_to_uri(file_path)?;
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

    let tree = parse_symbol_locations(&response);

    // Support nested names: "Config.new" → find "Config", then child "new"
    let parts: Vec<&str> = name.split('.').collect();

    // Go-specific: gopls returns receiver methods as flat entries like
    // "(*Handler).CreateSession" rather than as children of the struct.
    // Try this before the normal tree-walk for dotted names in .go files.
    if parts.len() == 2 && file_path.extension().and_then(|e| e.to_str()) == Some("go") {
        if let Some(sym) = tree
            .iter()
            .find(|s| crate::lang::go::receiver_method_matches(&s.name, parts[0], parts[1]))
        {
            return Ok(SymbolLocation {
                name: sym.name.clone(),
                kind: sym.kind.clone(),
                start_line: sym.start_line,
                end_line: sym.end_line,
                children: Vec::new(),
            });
        }
    }

    let mut current_list = &tree;
    let mut result: Option<&SymbolLocation> = None;

    for (i, part) in parts.iter().enumerate() {
        // For the last part of the name, collect ALL matches so we can
        // pick the one closest to hint_line (handles TypeScript overloads).
        let is_last = i == parts.len() - 1;

        if let (true, Some(hint)) = (is_last, hint_line) {
            let mut all_matches: Vec<&SymbolLocation> = Vec::new();
            collect_recursive(current_list, part, &mut all_matches);
            if all_matches.is_empty() {
                collect_recursive(&tree, part, &mut all_matches);
            }
            if all_matches.is_empty() {
                bail!("symbol '{name}' not found in document symbols");
            }
            let best = all_matches
                .iter()
                .min_by_key(|s| (i64::from(s.start_line) - i64::from(hint)).unsigned_abs())
                .copied()
                .expect("all_matches is non-empty, checked above");
            result = Some(best);
        } else {
            // First try the current level; if not found, search the full subtree.
            // This handles methods inside classes (e.g. `createPromotions` inside
            // `PromotionModuleService`) without requiring dotted notation.
            let found = current_list
                .iter()
                .find(|s| name_matches(&s.name, part))
                .or_else(|| find_recursive(&tree, part));
            match found {
                Some(sym) => {
                    result = Some(sym);
                    current_list = &sym.children;
                }
                None => bail!("symbol '{name}' not found in document symbols"),
            }
        }
    }

    let sym = result.context("empty symbol name")?;

    Ok(SymbolLocation {
        name: sym.name.clone(),
        kind: sym.kind.clone(),
        start_line: sym.start_line,
        end_line: sym.end_line,
        children: Vec::new(), // Don't clone the whole subtree
    })
}

/// Check if a symbol's name matches the query.
///
/// Exact match first; falls back to prefix match for generic types
/// (e.g. `IRepository<T, ID>` matches query `IRepository`).
fn name_matches(symbol_name: &str, query: &str) -> bool {
    if symbol_name == query {
        return true;
    }
    // Handle generics: "IRepository<T, ID>" matches "IRepository"
    if symbol_name.starts_with(query) {
        let next = symbol_name.as_bytes().get(query.len()).copied();
        return matches!(next, Some(b'<' | b'(' | b' '));
    }
    false
}

/// Depth-first search for a symbol by name through the full document symbol tree.
fn find_recursive<'a>(nodes: &'a [SymbolLocation], name: &str) -> Option<&'a SymbolLocation> {
    for node in nodes {
        if name_matches(&node.name, name) {
            return Some(node);
        }
        if let Some(found) = find_recursive(&node.children, name) {
            return Some(found);
        }
    }
    None
}

/// Collect ALL symbols with the given name (depth-first) into `out`.
fn collect_recursive<'a>(
    nodes: &'a [SymbolLocation],
    name: &str,
    out: &mut Vec<&'a SymbolLocation>,
) {
    for node in nodes {
        if name_matches(&node.name, name) {
            out.push(node);
        }
        collect_recursive(&node.children, name, out);
    }
}

/// Parse an LSP `documentSymbol` response into a hierarchical tree.
pub fn parse_symbol_locations(value: &Value) -> Vec<SymbolLocation> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };

    items.iter().map(parse_single_symbol).collect()
}

#[allow(clippy::cast_possible_truncation)]
fn parse_single_symbol(item: &Value) -> SymbolLocation {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let kind = symbol_kind_name(item.get("kind").and_then(Value::as_u64).unwrap_or(0)).to_string();

    let start_line = item
        .pointer("/range/start/line")
        .or_else(|| item.pointer("/location/range/start/line"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    let end_line = item
        .pointer("/range/end/line")
        .or_else(|| item.pointer("/location/range/end/line"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    let children = item
        .get("children")
        .map(parse_symbol_locations)
        .unwrap_or_default();

    SymbolLocation {
        name,
        kind,
        start_line,
        end_line,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_response() {
        let result = parse_symbol_locations(&json!(null));
        assert!(result.is_empty());
    }

    #[test]
    fn parse_flat_symbols() {
        let response = json!([
            {
                "name": "greet",
                "kind": 12,
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 3, "character": 1 }
                },
                "selectionRange": {
                    "start": { "line": 0, "character": 3 },
                    "end": { "line": 0, "character": 8 }
                }
            },
            {
                "name": "Config",
                "kind": 23,
                "range": {
                    "start": { "line": 5, "character": 0 },
                    "end": { "line": 10, "character": 1 }
                },
                "selectionRange": {
                    "start": { "line": 5, "character": 11 },
                    "end": { "line": 5, "character": 17 }
                }
            }
        ]);

        let symbols = parse_symbol_locations(&response);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name, "greet");
        assert_eq!(symbols[0].kind, "function");
        assert_eq!(symbols[0].start_line, 0);
        assert_eq!(symbols[0].end_line, 3);
        assert_eq!(symbols[1].name, "Config");
        assert_eq!(symbols[1].kind, "struct");
    }

    #[test]
    fn parse_nested_symbols() {
        let response = json!([
            {
                "name": "Config",
                "kind": 5,
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 20, "character": 1 }
                },
                "selectionRange": {
                    "start": { "line": 0, "character": 6 },
                    "end": { "line": 0, "character": 12 }
                },
                "children": [
                    {
                        "name": "new",
                        "kind": 6,
                        "range": {
                            "start": { "line": 5, "character": 2 },
                            "end": { "line": 10, "character": 3 }
                        },
                        "selectionRange": {
                            "start": { "line": 5, "character": 4 },
                            "end": { "line": 5, "character": 7 }
                        }
                    },
                    {
                        "name": "validate",
                        "kind": 6,
                        "range": {
                            "start": { "line": 12, "character": 2 },
                            "end": { "line": 18, "character": 3 }
                        },
                        "selectionRange": {
                            "start": { "line": 12, "character": 4 },
                            "end": { "line": 12, "character": 12 }
                        }
                    }
                ]
            }
        ]);

        let symbols = parse_symbol_locations(&response);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Config");
        assert_eq!(symbols[0].children.len(), 2);
        assert_eq!(symbols[0].children[0].name, "new");
        assert_eq!(symbols[0].children[0].start_line, 5);
        assert_eq!(symbols[0].children[0].end_line, 10);
        assert_eq!(symbols[0].children[1].name, "validate");
    }

    #[test]
    fn parse_symbol_with_location_fallback() {
        let response = json!([
            {
                "name": "test",
                "kind": 12,
                "location": {
                    "uri": "file:///tmp/test.rs",
                    "range": {
                        "start": { "line": 3, "character": 0 },
                        "end": { "line": 7, "character": 1 }
                    }
                }
            }
        ]);

        let symbols = parse_symbol_locations(&response);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].start_line, 3);
        assert_eq!(symbols[0].end_line, 7);
    }
}
