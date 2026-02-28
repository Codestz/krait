use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rayon::prelude::*;
use regex::{Regex, RegexBuilder};
use serde::Serialize;

use crate::detect::Language;

/// Bytes to scan at the start of a file for binary detection.
/// Smaller than read.rs's `BINARY_SCAN_SIZE` intentionally — search is a hot path.
const BINARY_SCAN_BYTES: usize = 512;

/// A single match found during search.
#[derive(Debug, Serialize)]
pub struct SearchMatch {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub preview: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Aggregated search results.
#[derive(Debug, Serialize)]
pub struct SearchOutput {
    pub matches: Vec<SearchMatch>,
    pub total_matches: usize,
    pub files_searched: usize,
    pub files_with_matches: usize,
    pub truncated: bool,
}

/// Options controlling search behaviour.
#[allow(clippy::struct_excessive_bools)]
pub struct SearchOptions {
    pub pattern: String,
    pub path: Option<PathBuf>,
    pub ignore_case: bool,
    pub word: bool,
    pub literal: bool,
    pub context: u32,
    pub files_only: bool,
    pub lang_filter: Option<String>,
    pub max_matches: usize,
}

/// Run the search and return aggregated results.
///
/// # Errors
/// Returns an error if the regex pattern is invalid or file walking fails.
pub fn run(opts: &SearchOptions, project_root: &Path) -> anyhow::Result<SearchOutput> {
    let re = build_regex(opts)?;
    let search_root = opts
        .path
        .as_deref()
        .unwrap_or(project_root);
    let files = collect_files(search_root, opts)?;
    let files_searched = files.len();

    // Parallel search: each file returns a Vec<SearchMatch>
    let file_results: Vec<Vec<SearchMatch>> = files
        .par_iter()
        .map(|path| search_file(path, search_root, project_root, &re, opts))
        .collect();

    // Flatten, sort, truncate
    let mut matches: Vec<SearchMatch> = Vec::new();
    let mut files_with_matches: usize = 0;
    let mut truncated = false;

    for file_matches in file_results {
        if file_matches.is_empty() {
            continue;
        }
        files_with_matches += 1;
        for m in file_matches {
            if matches.len() >= opts.max_matches {
                truncated = true;
                break;
            }
            matches.push(m);
        }
        if truncated {
            break;
        }
    }

    let total_matches = matches.len();
    Ok(SearchOutput {
        matches,
        total_matches,
        files_searched,
        files_with_matches,
        truncated,
    })
}

fn build_regex(opts: &SearchOptions) -> anyhow::Result<Regex> {
    let pat = if opts.literal {
        regex::escape(&opts.pattern)
    } else {
        normalize_grep_escapes(&opts.pattern)
    };

    let pat = if opts.word {
        format!(r"\b{pat}\b")
    } else {
        pat
    };

    RegexBuilder::new(&pat)
        .case_insensitive(opts.ignore_case)
        .build()
        .with_context(|| format!("invalid regex pattern: {}", opts.pattern))
}

/// Convert grep/BRE-style escape sequences to Rust regex (ERE) syntax.
///
/// Agents trained on Unix tools often emit `\|` for alternation, `\+` for
/// one-or-more, etc. In Rust regex these are invalid or literal — normalize
/// them silently so patterns just work.
fn normalize_grep_escapes(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('|') => { out.push('|');  chars.next(); }
                Some('+') => { out.push('+');  chars.next(); }
                Some('?') => { out.push('?');  chars.next(); }
                Some('(') => { out.push('(');  chars.next(); }
                Some(')') => { out.push(')');  chars.next(); }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn collect_files(root: &Path, opts: &SearchOptions) -> anyhow::Result<Vec<PathBuf>> {
    let extensions: Option<&[&str]> = opts
        .lang_filter
        .as_deref()
        .map(extensions_for_lang);

    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true);

    let mut files: Vec<PathBuf> = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Some(exts) = extensions {
            match path.extension().and_then(|e| e.to_str()) {
                Some(ext) if exts.contains(&ext) => {}
                _ => continue,
            }
        }
        files.push(path.to_path_buf());
    }

    files.sort();
    Ok(files)
}

fn search_file(
    path: &Path,
    search_root: &Path,
    project_root: &Path,
    re: &Regex,
    opts: &SearchOptions,
) -> Vec<SearchMatch> {
    let Ok(bytes) = std::fs::read(path) else {
        return vec![];
    };

    // Skip binary files: null byte in first BINARY_SCAN_BYTES
    if bytes[..bytes.len().min(BINARY_SCAN_BYTES)].contains(&0u8) {
        return vec![];
    }

    let Ok(content) = std::str::from_utf8(&bytes) else {
        return vec![];
    };

    // Relative path for display
    let rel = path
        .strip_prefix(search_root)
        .or_else(|_| path.strip_prefix(project_root))
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();

    let lines: Vec<&str> = content.lines().collect();
    let mut result: Vec<SearchMatch> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let Some(m) = re.find(line) else { continue };

        let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        let col = u32::try_from(m.start() + 1).unwrap_or(1);

        let (context_before, context_after) = if opts.context > 0 {
            let ctx = opts.context as usize;
            let before: Vec<String> = lines[idx.saturating_sub(ctx)..idx]
                .iter()
                .map(ToString::to_string)
                .collect();
            let after: Vec<String> = lines[(idx + 1)..(idx + 1 + ctx).min(lines.len())]
                .iter()
                .map(ToString::to_string)
                .collect();
            (before, after)
        } else {
            (vec![], vec![])
        };

        result.push(SearchMatch {
            path: rel.clone(),
            line: line_no,
            column: col,
            preview: line.to_string(),
            context_before,
            context_after,
        });
    }

    result
}

/// Map CLI language flag to file extensions via `Language::extensions()` — single source of truth.
fn extensions_for_lang(lang: &str) -> &'static [&'static str] {
    match lang {
        "ts" | "typescript" => Language::TypeScript.extensions(),
        "js" | "javascript" => Language::JavaScript.extensions(),
        "rs" | "rust" => Language::Rust.extensions(),
        "go" => Language::Go.extensions(),
        "c" | "cpp" | "c++" | "cxx" => Language::Cpp.extensions(),
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn make_project(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }
        dir
    }

    fn opts(pattern: &str) -> SearchOptions {
        SearchOptions {
            pattern: pattern.to_string(),
            path: None,
            ignore_case: false,
            word: false,
            literal: false,
            context: 0,
            files_only: false,
            lang_filter: None,
            max_matches: 200,
        }
    }

    #[test]
    fn finds_literal_match() {
        let dir = make_project(&[("src/lib.rs", "fn hello() {}\nfn world() {}")]);
        let o = run(
            &SearchOptions { literal: true, ..opts("hello") },
            dir.path(),
        )
        .unwrap();
        assert_eq!(o.total_matches, 1);
        assert_eq!(o.matches[0].line, 1);
        assert!(o.matches[0].preview.contains("hello"));
    }

    #[test]
    fn finds_regex_match() {
        let dir = make_project(&[("a.rs", "foo123\nbar456")]);
        let o = run(&opts(r"\d+"), dir.path()).unwrap();
        assert_eq!(o.total_matches, 2);
    }

    #[test]
    fn ignore_case_works() {
        let dir = make_project(&[("a.rs", "Hello\nhello\nHELLO")]);
        let o = run(
            &SearchOptions { ignore_case: true, ..opts("hello") },
            dir.path(),
        )
        .unwrap();
        assert_eq!(o.total_matches, 3);
    }

    #[test]
    fn word_boundary_works() {
        let dir = make_project(&[("a.rs", "foobar\nfoo bar\nfoo")]);
        let o = run(
            &SearchOptions { word: true, ..opts("foo") },
            dir.path(),
        )
        .unwrap();
        // "foobar" should NOT match, "foo bar" and "foo" should match
        assert_eq!(o.total_matches, 2);
    }

    #[test]
    fn respects_max_matches() {
        let content: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        let dir = make_project(&[("a.rs", &content)]);
        let o = run(
            &SearchOptions { max_matches: 3, ..opts("line") },
            dir.path(),
        )
        .unwrap();
        assert_eq!(o.total_matches, 3);
        assert!(o.truncated);
    }

    #[test]
    fn skips_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut binary = vec![0u8; 100];
        binary.extend_from_slice(b"hello");
        fs::write(dir.path().join("file.bin"), &binary).unwrap();
        let o = run(&opts("hello"), dir.path()).unwrap();
        assert_eq!(o.total_matches, 0);
    }

    #[test]
    fn lang_filter_ts_only() {
        let dir = make_project(&[
            ("a.ts", "const foo = 1;"),
            ("b.rs", "const foo: i32 = 1;"),
        ]);
        let o = run(
            &SearchOptions {
                lang_filter: Some("ts".to_string()),
                ..opts("foo")
            },
            dir.path(),
        )
        .unwrap();
        assert_eq!(o.total_matches, 1);
        assert!(o.matches[0].path.ends_with("a.ts"));
    }

    #[test]
    fn context_lines_correct() {
        let dir = make_project(&[("a.rs", "line1\nline2\ntarget\nline4\nline5")]);
        let o = run(
            &SearchOptions { context: 1, ..opts("target") },
            dir.path(),
        )
        .unwrap();
        assert_eq!(o.total_matches, 1);
        let m = &o.matches[0];
        assert_eq!(m.context_before, vec!["line2"]);
        assert_eq!(m.context_after, vec!["line4"]);
    }

    #[test]
    fn files_only_mode() {
        let dir = make_project(&[
            ("a.rs", "needle"),
            ("b.rs", "haystack"),
        ]);
        let o = run(
            &SearchOptions { files_only: true, ..opts("needle") },
            dir.path(),
        )
        .unwrap();
        assert_eq!(o.files_with_matches, 1);
    }

    #[test]
    fn no_matches_returns_empty() {
        let dir = make_project(&[("a.rs", "hello world")]);
        let o = run(&opts("nonexistent_xyz"), dir.path()).unwrap();
        assert_eq!(o.total_matches, 0);
        assert!(!o.truncated);
    }

    #[test]
    fn grep_escape_alternation() {
        // \| is grep BRE syntax; should be treated as | (alternation)
        let dir = make_project(&[("a.ts", "import foo\nfrom bar")]);
        let o = run(&opts(r"foo\|bar"), dir.path()).unwrap();
        assert_eq!(o.total_matches, 2);
    }

    #[test]
    fn grep_escape_plus_and_parens() {
        assert_eq!(normalize_grep_escapes(r"foo\+"), "foo+");
        assert_eq!(normalize_grep_escapes(r"\(foo\)"), "(foo)");
        assert_eq!(normalize_grep_escapes(r"a\?b"), "a?b");
    }

    #[test]
    fn real_pipe_unaffected() {
        // a plain | should still work as alternation
        let dir = make_project(&[("a.ts", "import foo\nfrom bar")]);
        let o = run(&opts("foo|bar"), dir.path()).unwrap();
        assert_eq!(o.total_matches, 2);
    }
}
