use std::collections::HashSet;

use crate::commands::find::{ReferenceMatch, SymbolMatch};

/// Merge symbol results from multiple LSP servers, sorted by path:line.
#[must_use]
pub fn merge_symbol_results(results: Vec<Vec<SymbolMatch>>) -> Vec<SymbolMatch> {
    let mut merged: Vec<SymbolMatch> = results.into_iter().flatten().collect();
    merged.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));

    // Deduplicate by (path, line)
    let mut seen = HashSet::new();
    merged.retain(|m| seen.insert((m.path.clone(), m.line)));
    merged
}

/// Merge reference results from multiple LSP servers, deduplicate by path:line.
#[must_use]
pub fn merge_reference_results(results: Vec<Vec<ReferenceMatch>>) -> Vec<ReferenceMatch> {
    let mut merged: Vec<ReferenceMatch> = results.into_iter().flatten().collect();

    // Deduplicate by (path, line)
    let mut seen = HashSet::new();
    merged.retain(|m| seen.insert((m.path.clone(), m.line)));

    // Sort: definition first, then by file:line
    merged.sort_by(|a, b| {
        b.is_definition
            .cmp(&a.is_definition)
            .then(a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(path: &str, line: u32, kind: &str) -> SymbolMatch {
        SymbolMatch {
            path: path.to_string(),
            line,
            kind: kind.to_string(),
            preview: String::new(),
            body: None,
        }
    }

    fn refm(path: &str, line: u32, is_def: bool) -> ReferenceMatch {
        ReferenceMatch {
            path: path.to_string(),
            line,
            preview: String::new(),
            is_definition: is_def,
            containing_symbol: None,
        }
    }

    #[test]
    fn merge_symbols_deduplicates() {
        let r1 = vec![sym("src/a.rs", 10, "function")];
        let r2 = vec![sym("src/a.rs", 10, "function"), sym("src/b.ts", 5, "class")];

        let merged = merge_symbol_results(vec![r1, r2]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].path, "src/a.rs");
        assert_eq!(merged[1].path, "src/b.ts");
    }

    #[test]
    fn merge_symbols_sorted_by_path_line() {
        let r1 = vec![sym("src/z.rs", 1, "function")];
        let r2 = vec![sym("src/a.rs", 1, "function")];

        let merged = merge_symbol_results(vec![r1, r2]);
        assert_eq!(merged[0].path, "src/a.rs");
        assert_eq!(merged[1].path, "src/z.rs");
    }

    #[test]
    fn merge_refs_deduplicates() {
        let r1 = vec![refm("src/a.rs", 10, true)];
        let r2 = vec![refm("src/a.rs", 10, true), refm("src/b.rs", 20, false)];

        let merged = merge_reference_results(vec![r1, r2]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_refs_definition_first() {
        let r1 = vec![refm("src/b.rs", 20, false)];
        let r2 = vec![refm("src/a.rs", 10, true)];

        let merged = merge_reference_results(vec![r1, r2]);
        assert!(merged[0].is_definition);
        assert_eq!(merged[0].path, "src/a.rs");
    }

    #[test]
    fn merge_empty_inputs() {
        assert!(merge_symbol_results(vec![]).is_empty());
        assert!(merge_reference_results(vec![]).is_empty());
    }
}
