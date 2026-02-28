//! Cache-first query path: serve queries from the `SQLite` index without LSP.
//!
//! All functions are sync — no LSP dependency.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::commands::{
    find::SymbolMatch, list::SymbolEntry, read::format_numbered_lines, DEFAULT_MAX_LINES,
};
use crate::index::hasher;
use crate::index::store::{CachedSymbol, IndexStore};
use crate::index::watcher::DirtyFiles;

/// Check whether a file's current content matches the index.
///
/// When a `DirtyFiles` watcher is active:
/// - Dirty files → return false immediately (O(1) set lookup)
/// - Clean files → trust the index (just check file exists in DB)
///
/// Without a watcher, falls back to full BLAKE3 hash comparison.
fn is_file_fresh(
    store: &IndexStore,
    rel_path: &str,
    project_root: &Path,
    dirty_files: Option<&DirtyFiles>,
) -> bool {
    if let Some(df) = dirty_files {
        // Watcher active: use dirty set instead of hashing
        if df.is_dirty(rel_path) {
            return false;
        }
        // Not dirty — trust the index, just verify file is indexed
        return store.get_file_hash(rel_path).ok().flatten().is_some();
    }

    // No watcher: full BLAKE3 check (original behavior)
    let Some(stored_hash) = store.get_file_hash(rel_path).ok().flatten() else {
        return false;
    };
    let abs_path = project_root.join(rel_path);
    let Ok(current_hash) = hasher::hash_file(&abs_path) else {
        return false;
    };
    stored_hash == current_hash
}

/// Serve `find symbol` from the cache. Returns `None` if cache has no results
/// or any source file is stale.
pub fn cached_find_symbol(
    store: &IndexStore,
    name: &str,
    project_root: &Path,
    dirty_files: Option<&DirtyFiles>,
) -> Option<Vec<SymbolMatch>> {
    let symbols = store.find_symbols_by_name(name).ok()?;
    if symbols.is_empty() {
        return None;
    }

    // Check freshness for every file containing a match.
    // If any file is stale, bail out entirely — the LSP path will give correct results.
    for sym in &symbols {
        if !is_file_fresh(store, &sym.path, project_root, dirty_files) {
            return None;
        }
    }

    // Group symbol indices by file path to read each file only once
    let mut by_path: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, sym) in symbols.iter().enumerate() {
        by_path.entry(sym.path.as_str()).or_default().push(i);
    }

    let mut previews = vec![String::new(); symbols.len()];
    for (rel_path, indices) in &by_path {
        let abs = project_root.join(rel_path);
        if let Ok(content) = std::fs::read_to_string(&abs) {
            let lines: Vec<&str> = content.lines().collect();
            for &idx in indices {
                // Lines are 0-indexed in DB, display as 1-indexed
                let line_no = symbols[idx].range_start_line as usize;
                previews[idx] = lines.get(line_no).unwrap_or(&"").trim().to_string();
            }
        }
    }

    let mut results: Vec<SymbolMatch> = symbols
        .into_iter()
        .zip(previews)
        .map(|(sym, preview)| SymbolMatch {
            path: sym.path,
            // Lines are 0-indexed in DB, display as 1-indexed
            line: sym.range_start_line + 1,
            kind: sym.kind,
            preview,
            body: None,
        })
        .collect();

    results.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    Some(results)
}

/// Build a hierarchical `SymbolEntry` tree from flat `CachedSymbol` rows.
///
/// Uses `parent_name` to reconstruct nesting. Symbols without a parent are
/// top-level. Respects `max_depth` (1 = top-level only, 2 = one level of
/// children, etc.).
fn build_hierarchy(symbols: &[CachedSymbol], max_depth: u8) -> Vec<SymbolEntry> {
    // Top-level symbols: no parent
    let top_level: Vec<&CachedSymbol> =
        symbols.iter().filter(|s| s.parent_name.is_none()).collect();

    top_level
        .into_iter()
        .map(|sym| build_entry(sym, symbols, max_depth, 1))
        .collect()
}

fn build_entry(
    sym: &CachedSymbol,
    all_symbols: &[CachedSymbol],
    max_depth: u8,
    current_depth: u8,
) -> SymbolEntry {
    let children = if current_depth < max_depth {
        all_symbols
            .iter()
            .filter(|s| s.parent_name.as_deref() == Some(&sym.name))
            .map(|child| build_entry(child, all_symbols, max_depth, current_depth + 1))
            .collect()
    } else {
        Vec::new()
    };

    SymbolEntry {
        name: sym.name.clone(),
        kind: sym.kind.clone(),
        line: sym.range_start_line + 1, // 0-indexed → 1-indexed
        end_line: sym.range_end_line + 1,
        children,
    }
}

/// Serve `list symbols` from the cache. Returns `None` if file is stale or missing.
pub fn cached_list_symbols(
    store: &IndexStore,
    rel_path: &str,
    depth: u8,
    project_root: &Path,
    dirty_files: Option<&DirtyFiles>,
) -> Option<Vec<SymbolEntry>> {
    if !is_file_fresh(store, rel_path, project_root, dirty_files) {
        return None;
    }

    let symbols = store.find_symbols_by_path(rel_path).ok()?;
    if symbols.is_empty() {
        return None;
    }

    Some(build_hierarchy(&symbols, depth))
}

/// Serve `read symbol` from the cache. Returns `None` if symbol not found or file stale.
///
/// Supports dotted names (e.g., `Config.new`) by searching for the parent first,
/// then finding the child in the same file's symbols.
pub fn cached_read_symbol(
    store: &IndexStore,
    name: &str,
    signature_only: bool,
    max_lines: Option<u32>,
    project_root: &Path,
    dirty_files: Option<&DirtyFiles>,
) -> Option<Value> {
    let (search_name, child_name) = if let Some(dot_pos) = name.find('.') {
        (&name[..dot_pos], Some(&name[dot_pos + 1..]))
    } else {
        (name, None)
    };

    let symbols = store.find_symbols_by_name(search_name).ok()?;
    if symbols.is_empty() {
        return None;
    }

    // Find the target symbol (either the parent, or the child for dotted names)
    let target = if let Some(child) = child_name {
        // For dotted names: find a child symbol in the same file as the parent
        let parent = symbols.first()?;
        let file_symbols = store.find_symbols_by_path(&parent.path).ok()?;
        file_symbols
            .into_iter()
            .find(|s| s.name == child && s.parent_name.as_deref() == Some(search_name))?
    } else {
        symbols.into_iter().next()?
    };

    // Check file freshness
    if !is_file_fresh(store, &target.path, project_root, dirty_files) {
        return None;
    }

    // Read lines from disk
    let abs_path = project_root.join(&target.path);
    let content = std::fs::read_to_string(&abs_path).ok()?;
    let all_lines: Vec<&str> = content.lines().collect();

    let start = target.range_start_line as usize;
    let end = (target.range_end_line as usize + 1).min(all_lines.len());

    if start >= all_lines.len() {
        return None;
    }

    let selected = &all_lines[start..end];

    let lines: &[&str] = if signature_only {
        let sig_end = selected
            .iter()
            .position(|l| l.contains('{'))
            .map_or(1, |i| i + 1);
        &selected[..sig_end.min(selected.len())]
    } else {
        selected
    };

    let max = max_lines.unwrap_or(DEFAULT_MAX_LINES) as usize;
    let truncated = lines.len() > max;
    let lines = if truncated { &lines[..max] } else { lines };

    let numbered = format_numbered_lines(lines, start + 1);

    let display_from = start + 1;
    let display_to = start + lines.len();

    Some(json!({
        "path": target.path,
        "symbol": target.name,
        "kind": target.kind,
        "content": numbered,
        "from": display_from,
        "to": display_to,
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store_with_symbols() -> (IndexStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = IndexStore::open_in_memory().unwrap();

        // Create a test file
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("lib.rs"),
            "// line 1\nstruct Config {\n    name: String,\n    value: u32,\n}\n\nimpl Config {\n    fn new() -> Self {\n        Config { name: String::new(), value: 0 }\n    }\n}\n",
        ).unwrap();

        // Hash the file
        let hash = hasher::hash_file(&src_dir.join("lib.rs")).unwrap();
        store.upsert_file("src/lib.rs", &hash).unwrap();

        // Insert symbols (0-indexed lines)
        let symbols = vec![
            CachedSymbol {
                name: "Config".into(),
                kind: "struct".into(),
                path: "src/lib.rs".into(),
                range_start_line: 1,
                range_start_col: 0,
                range_end_line: 4,
                range_end_col: 1,
                parent_name: None,
            },
            CachedSymbol {
                name: "name".into(),
                kind: "field".into(),
                path: "src/lib.rs".into(),
                range_start_line: 2,
                range_start_col: 4,
                range_end_line: 2,
                range_end_col: 20,
                parent_name: Some("Config".into()),
            },
            CachedSymbol {
                name: "value".into(),
                kind: "field".into(),
                path: "src/lib.rs".into(),
                range_start_line: 3,
                range_start_col: 4,
                range_end_line: 3,
                range_end_col: 15,
                parent_name: Some("Config".into()),
            },
            CachedSymbol {
                name: "new".into(),
                kind: "function".into(),
                path: "src/lib.rs".into(),
                range_start_line: 7,
                range_start_col: 4,
                range_end_line: 9,
                range_end_col: 5,
                parent_name: Some("Config".into()),
            },
        ];
        store.insert_symbols("src/lib.rs", &symbols).unwrap();

        (store, dir)
    }

    // --- is_file_fresh tests (no watcher — BLAKE3 fallback) ---

    #[test]
    fn file_freshness_matches() {
        let (store, dir) = make_store_with_symbols();
        assert!(is_file_fresh(&store, "src/lib.rs", dir.path(), None));
    }

    #[test]
    fn file_freshness_stale_after_modify() {
        let (store, dir) = make_store_with_symbols();
        std::fs::write(dir.path().join("src/lib.rs"), "modified content").unwrap();
        assert!(!is_file_fresh(&store, "src/lib.rs", dir.path(), None));
    }

    #[test]
    fn file_freshness_missing_file() {
        let store = IndexStore::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_file_fresh(&store, "nonexistent.rs", dir.path(), None));
    }

    // --- is_file_fresh tests (with watcher) ---

    #[test]
    fn fresh_with_watcher_clean_file() {
        let (store, dir) = make_store_with_symbols();
        let df = DirtyFiles::new();
        // File is indexed and not dirty → fresh
        assert!(is_file_fresh(&store, "src/lib.rs", dir.path(), Some(&df)));
    }

    #[test]
    fn stale_with_watcher_dirty_file() {
        let (store, dir) = make_store_with_symbols();
        let df = DirtyFiles::new();
        df.mark_dirty("src/lib.rs".to_string());
        // File is dirty → stale (no BLAKE3 needed)
        assert!(!is_file_fresh(&store, "src/lib.rs", dir.path(), Some(&df)));
    }

    #[test]
    fn stale_with_watcher_poisoned() {
        let (store, dir) = make_store_with_symbols();
        let df = DirtyFiles::new();
        df.poison();
        // Everything is dirty when poisoned
        assert!(!is_file_fresh(&store, "src/lib.rs", dir.path(), Some(&df)));
    }

    #[test]
    fn stale_with_watcher_not_indexed() {
        let store = IndexStore::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let df = DirtyFiles::new();
        // File not in index at all → stale
        assert!(!is_file_fresh(&store, "unknown.rs", dir.path(), Some(&df)));
    }

    // --- cached_find_symbol ---

    #[test]
    fn find_symbol_from_cache() {
        let (store, dir) = make_store_with_symbols();
        let results = cached_find_symbol(&store, "Config", dir.path(), None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "struct");
        assert_eq!(results[0].line, 2);
        assert_eq!(results[0].path, "src/lib.rs");
        assert!(results[0].preview.contains("struct Config"));
    }

    #[test]
    fn find_symbol_stale_file() {
        let (store, dir) = make_store_with_symbols();
        std::fs::write(dir.path().join("src/lib.rs"), "modified").unwrap();
        assert!(cached_find_symbol(&store, "Config", dir.path(), None).is_none());
    }

    #[test]
    fn find_symbol_dirty_via_watcher() {
        let (store, dir) = make_store_with_symbols();
        let df = DirtyFiles::new();
        df.mark_dirty("src/lib.rs".to_string());
        assert!(cached_find_symbol(&store, "Config", dir.path(), Some(&df)).is_none());
    }

    #[test]
    fn find_symbol_clean_via_watcher() {
        let (store, dir) = make_store_with_symbols();
        let df = DirtyFiles::new();
        let results = cached_find_symbol(&store, "Config", dir.path(), Some(&df)).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn find_symbol_not_in_cache() {
        let (store, dir) = make_store_with_symbols();
        assert!(cached_find_symbol(&store, "NonExistent", dir.path(), None).is_none());
    }

    // --- cached_list_symbols ---

    #[test]
    fn list_symbols_from_cache() {
        let (store, dir) = make_store_with_symbols();
        let symbols = cached_list_symbols(&store, "src/lib.rs", 2, dir.path(), None).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Config");
        assert_eq!(symbols[0].children.len(), 3);
    }

    #[test]
    fn list_symbols_depth_1() {
        let (store, dir) = make_store_with_symbols();
        let symbols = cached_list_symbols(&store, "src/lib.rs", 1, dir.path(), None).unwrap();
        assert_eq!(symbols.len(), 1);
        assert!(symbols[0].children.is_empty());
    }

    #[test]
    fn list_symbols_stale_file() {
        let (store, dir) = make_store_with_symbols();
        std::fs::write(dir.path().join("src/lib.rs"), "modified").unwrap();
        assert!(cached_list_symbols(&store, "src/lib.rs", 2, dir.path(), None).is_none());
    }

    #[test]
    fn list_symbols_dirty_via_watcher() {
        let (store, dir) = make_store_with_symbols();
        let df = DirtyFiles::new();
        df.mark_dirty("src/lib.rs".to_string());
        assert!(cached_list_symbols(&store, "src/lib.rs", 2, dir.path(), Some(&df)).is_none());
    }

    // --- cached_read_symbol ---

    #[test]
    fn read_symbol_from_cache() {
        let (store, dir) = make_store_with_symbols();
        let result = cached_read_symbol(&store, "Config", false, None, dir.path(), None).unwrap();
        assert_eq!(result["path"], "src/lib.rs");
        assert_eq!(result["symbol"], "Config");
        assert_eq!(result["kind"], "struct");
        assert_eq!(result["from"], 2);
        assert_eq!(result["truncated"], false);
        assert!(result["content"]
            .as_str()
            .unwrap()
            .contains("struct Config"));
    }

    #[test]
    fn read_symbol_signature_only() {
        let (store, dir) = make_store_with_symbols();
        let result = cached_read_symbol(&store, "Config", true, None, dir.path(), None).unwrap();
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("struct Config"));
        assert!(!content.contains("value"));
    }

    #[test]
    fn read_symbol_dotted_name() {
        let (store, dir) = make_store_with_symbols();
        let result =
            cached_read_symbol(&store, "Config.new", false, None, dir.path(), None).unwrap();
        assert_eq!(result["symbol"], "new");
        assert_eq!(result["kind"], "function");
        assert!(result["content"].as_str().unwrap().contains("fn new"));
    }

    #[test]
    fn read_symbol_stale_file() {
        let (store, dir) = make_store_with_symbols();
        std::fs::write(dir.path().join("src/lib.rs"), "modified").unwrap();
        assert!(cached_read_symbol(&store, "Config", false, None, dir.path(), None).is_none());
    }

    #[test]
    fn read_symbol_dirty_via_watcher() {
        let (store, dir) = make_store_with_symbols();
        let df = DirtyFiles::new();
        df.mark_dirty("src/lib.rs".to_string());
        assert!(cached_read_symbol(&store, "Config", false, None, dir.path(), Some(&df)).is_none());
    }

    #[test]
    fn read_symbol_not_found() {
        let (store, dir) = make_store_with_symbols();
        assert!(cached_read_symbol(&store, "NonExistent", false, None, dir.path(), None).is_none());
    }

    #[test]
    fn read_symbol_max_lines() {
        let (store, dir) = make_store_with_symbols();
        let result =
            cached_read_symbol(&store, "Config", false, Some(2), dir.path(), None).unwrap();
        assert_eq!(result["truncated"], true);
        assert_eq!(result["to"], 3);
    }

    #[test]
    fn hierarchy_preserves_line_numbers() {
        let (store, dir) = make_store_with_symbols();
        let symbols = cached_list_symbols(&store, "src/lib.rs", 2, dir.path(), None).unwrap();
        assert_eq!(symbols[0].line, 2);
        assert_eq!(symbols[0].end_line, 5);
    }

    #[test]
    fn empty_index_returns_none() {
        let store = IndexStore::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();

        assert!(cached_find_symbol(&store, "Foo", dir.path(), None).is_none());
        assert!(cached_list_symbols(&store, "src/lib.rs", 2, dir.path(), None).is_none());
        assert!(cached_read_symbol(&store, "Foo", false, None, dir.path(), None).is_none());
    }
}
