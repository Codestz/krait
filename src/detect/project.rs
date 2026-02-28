use std::path::{Path, PathBuf};

use super::language::Language;

/// Markers checked at each directory level, in priority order.
const MARKERS: &[&str] = &[
    ".krait",
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "CMakeLists.txt",
];

/// Walk up from `from` to find the project root by looking for known markers.
/// Returns the directory containing the first marker found, or `from` as fallback.
#[must_use]
pub fn detect_project_root(from: &Path) -> PathBuf {
    let mut current = from.to_path_buf();

    loop {
        for marker in MARKERS {
            if current.join(marker).exists() {
                return current;
            }
        }

        if !current.pop() {
            return from.to_path_buf();
        }
    }
}

/// Compute deterministic socket path for a project root.
/// Format: `<tmpdir>/krait-<16-hex-chars>.sock`
#[must_use]
pub fn socket_path(project_root: &Path) -> PathBuf {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let hash = blake3::hash(canonical.to_string_lossy().as_bytes());
    let hex = hash.to_hex();
    let short = &hex[..16];

    std::env::temp_dir().join(format!("krait-{short}.sock"))
}

/// Find all LSP workspace roots within a project for monorepo support.
///
/// Recursively walks the entire project tree (respecting `.gitignore`) to find
/// all manifest files. Each manifest's parent directory becomes a workspace
/// candidate. De-nests: if a parent directory already has the same language
/// marker, child directories are skipped (e.g., Rust workspace crates don't
/// each get their own LSP root — the workspace root covers them).
///
/// Returns `(Language, PathBuf)` pairs sorted by path depth (shallowest first).
#[must_use]
pub fn find_package_roots(project_root: &Path) -> Vec<(Language, PathBuf)> {
    let mut candidates: Vec<(Language, PathBuf)> = Vec::new();

    // Walk the entire project tree, respecting .gitignore
    let mut builder = ignore::WalkBuilder::new(project_root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true);

    for entry in builder.build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Some(filename) = entry.path().file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(parent) = entry.path().parent() else {
            continue;
        };

        // Derive workspace markers from Language::workspace_markers() — single source of truth.
        for &lang in Language::ALL {
            for &marker in lang.workspace_markers() {
                if filename == marker {
                    let pair = (lang, parent.to_path_buf());
                    if !candidates.contains(&pair) {
                        candidates.push(pair);
                    }
                }
            }
        }
    }

    // Sort by path depth (shallowest first) for correct de-nesting
    candidates.sort_by_key(|(_, p)| p.components().count());

    // De-nest: remove roots that are subdirectories of an existing root
    // for the same language.
    let mut result: Vec<(Language, PathBuf)> = Vec::new();
    for (lang, dir) in &candidates {
        let covered = result
            .iter()
            .any(|(l, r)| *l == *lang && dir.starts_with(r) && dir != r);
        if !covered {
            result.push((*lang, dir.clone()));
        }
    }

    // Fix "." root problem: if root has package.json but no tsconfig.json,
    // and sub-packages DO have tsconfig.json, skip the root JS server.
    // The root package.json is a meta-package, not a JS/TS project.
    let has_sub_tsconfigs = result
        .iter()
        .any(|(l, r)| *l == Language::TypeScript && r != project_root);
    if has_sub_tsconfigs {
        result.retain(|(lang, root)| {
            if root == project_root && *lang == Language::JavaScript {
                // Only keep root JS if it has its own tsconfig/jsconfig
                root.join("tsconfig.json").exists() || root.join("jsconfig.json").exists()
            } else {
                true
            }
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_git_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();

        let root = detect_project_root(dir.path());
        assert_eq!(root, dir.path());
    }

    #[test]
    fn detects_cargo_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();

        let root = detect_project_root(dir.path());
        assert_eq!(root, dir.path());
    }

    #[test]
    fn detects_krait_root() {
        let dir = tempfile::tempdir().unwrap();
        // Both .krait and .git exist — .krait should win (higher priority)
        std::fs::create_dir(dir.path().join(".krait")).unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();

        let root = detect_project_root(dir.path());
        assert_eq!(root, dir.path());
    }

    #[test]
    fn nested_dir_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();

        let nested = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();

        let root = detect_project_root(&nested);
        assert_eq!(root, dir.path());
    }

    #[test]
    fn no_marker_returns_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let root = detect_project_root(dir.path());
        // Should return the input path when no marker found
        assert!(root.exists());
    }

    #[test]
    fn find_package_roots_simple_rust() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();

        let roots = find_package_roots(dir.path());
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].0, Language::Rust);
        assert_eq!(roots[0].1, dir.path());
    }

    #[test]
    fn find_package_roots_monorepo_with_denesting() {
        let dir = tempfile::tempdir().unwrap();
        // Root: Cargo.toml (Rust workspace)
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        // Nested crate under crates/ — should be de-nested
        let crate_dir = dir.path().join("crates/mylib");
        std::fs::create_dir_all(&crate_dir).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "").unwrap();
        // TypeScript package — separate scope
        let api = dir.path().join("packages/api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("tsconfig.json"), "").unwrap();

        let roots = find_package_roots(dir.path());
        let rust_roots: Vec<_> = roots.iter().filter(|(l, _)| *l == Language::Rust).collect();
        let ts_roots: Vec<_> = roots
            .iter()
            .filter(|(l, _)| *l == Language::TypeScript)
            .collect();

        assert_eq!(
            rust_roots.len(),
            1,
            "one Rust root (workspace covers crates)"
        );
        assert_eq!(ts_roots.len(), 1, "one TypeScript root");
    }

    #[test]
    fn find_package_roots_ts_monorepo_multiple_packages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        for pkg in &["api", "web", "common"] {
            let p = dir.path().join("packages").join(pkg);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("tsconfig.json"), "{}").unwrap();
        }

        let roots = find_package_roots(dir.path());
        let ts_roots: Vec<_> = roots
            .iter()
            .filter(|(l, _)| *l == Language::TypeScript)
            .collect();
        // 3 separate TypeScript workspaces (different tsconfigs)
        assert_eq!(ts_roots.len(), 3);
    }

    #[test]
    fn find_package_roots_skips_root_js_when_sub_packages_have_tsconfig() {
        let dir = tempfile::tempdir().unwrap();
        // Root has package.json but NO tsconfig
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        for pkg in &["api", "web"] {
            let p = dir.path().join("packages").join(pkg);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("tsconfig.json"), "{}").unwrap();
        }

        let roots = find_package_roots(dir.path());
        // Root JS entry should be filtered out
        let js_at_root: Vec<_> = roots
            .iter()
            .filter(|(l, r)| *l == Language::JavaScript && *r == dir.path())
            .collect();
        assert!(
            js_at_root.is_empty(),
            "root package.json should be skipped when sub-packages have tsconfig"
        );
        // But TypeScript sub-packages should remain
        let ts_roots: Vec<_> = roots
            .iter()
            .filter(|(l, _)| *l == Language::TypeScript)
            .collect();
        assert_eq!(ts_roots.len(), 2);
    }

    #[test]
    fn find_package_roots_keeps_root_when_it_has_tsconfig() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();

        let p = dir.path().join("packages/api");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("tsconfig.json"), "{}").unwrap();

        let roots = find_package_roots(dir.path());
        // Root should be kept as TypeScript (has tsconfig), JS may be present too
        let ts_at_root: Vec<_> = roots
            .iter()
            .filter(|(l, r)| *l == Language::TypeScript && *r == dir.path())
            .collect();
        assert_eq!(ts_at_root.len(), 1, "root with tsconfig should be kept");
    }

    #[test]
    fn find_package_roots_deeply_nested_manifests() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate a project like medusa with deeply nested packages
        // packages/modules/providers/package.json
        let deep = dir.path().join("packages/modules/providers");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("package.json"), "{}").unwrap();
        std::fs::write(deep.join("tsconfig.json"), "{}").unwrap();

        // src/frontend/package.json (like meet project)
        let frontend = dir.path().join("src/frontend");
        std::fs::create_dir_all(&frontend).unwrap();
        std::fs::write(frontend.join("package.json"), "{}").unwrap();
        std::fs::write(frontend.join("tsconfig.json"), "{}").unwrap();

        let roots = find_package_roots(dir.path());
        let ts_roots: Vec<_> = roots
            .iter()
            .filter(|(l, _)| *l == Language::TypeScript)
            .collect();

        assert_eq!(ts_roots.len(), 2, "should find both deeply nested TS roots");
    }

    #[test]
    fn find_package_roots_arbitrary_directory_structure() {
        let dir = tempfile::tempdir().unwrap();
        // Root go.mod
        std::fs::write(dir.path().join("go.mod"), "").unwrap();
        // Frontend at non-standard location
        let frontend = dir.path().join("frontend");
        std::fs::create_dir_all(&frontend).unwrap();
        std::fs::write(frontend.join("package.json"), "{}").unwrap();
        std::fs::write(frontend.join("tsconfig.json"), "{}").unwrap();

        let roots = find_package_roots(dir.path());
        let go_roots: Vec<_> = roots.iter().filter(|(l, _)| *l == Language::Go).collect();
        let ts_roots: Vec<_> = roots
            .iter()
            .filter(|(l, _)| *l == Language::TypeScript)
            .collect();

        assert_eq!(go_roots.len(), 1, "should find Go root");
        assert_eq!(ts_roots.len(), 1, "should find TS in frontend/");
    }

    #[test]
    fn socket_path_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = socket_path(dir.path());
        let p2 = socket_path(dir.path());
        assert_eq!(p1, p2);
    }

    #[test]
    fn socket_path_differs_per_project() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let p1 = socket_path(dir1.path());
        let p2 = socket_path(dir2.path());
        assert_ne!(p1, p2);
    }

    #[test]
    fn socket_path_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path());
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("krait-"));
        assert!(Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("sock")));
    }
}
