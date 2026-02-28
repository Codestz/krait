use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Go,
    Cpp,
}

impl Language {
    /// Human-readable name for display.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Go => "go",
            Self::Cpp => "c++",
        }
    }
}

impl Language {
    /// File extensions associated with this language.
    #[must_use]
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["rs"],
            Self::TypeScript => &["ts", "tsx"],
            Self::JavaScript => &["js", "jsx", "mjs", "cjs"],
            Self::Go => &["go"],
            Self::Cpp => &["c", "cpp", "cc", "cxx", "h", "hpp", "hxx"],
        }
    }

    /// Workspace marker files that indicate this language's project root.
    /// Used by `find_package_roots()` for monorepo workspace detection.
    #[must_use]
    pub fn workspace_markers(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["Cargo.toml"],
            Self::TypeScript => &["tsconfig.json"],
            Self::JavaScript => &["package.json"],
            Self::Go => &["go.mod"],
            Self::Cpp => &["CMakeLists.txt", "compile_commands.json"],
        }
    }

    /// All language variants.
    pub const ALL: &'static [Language] = &[
        Language::Rust,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
        Language::Cpp,
    ];
}

/// Determine the language for a file based on its extension.
/// Delegates to `Language::extensions()` — single source of truth.
#[must_use]
pub fn language_for_file(path: &Path) -> Option<Language> {
    let ext = path.extension()?.to_str()?;
    Language::ALL.iter().copied().find(|lang| lang.extensions().contains(&ext))
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Common JS/TS monorepo subdirectory conventions.
const MONOREPO_DIRS: &[&str] = &["packages", "apps", "libs", "src"];

/// Detect languages used in a project by scanning for config files.
/// Marker file names come from `Language::workspace_markers()` — single source of truth.
/// Returns languages in priority order.
#[must_use]
pub fn detect_languages(root: &Path) -> Vec<Language> {
    let mut languages = Vec::new();

    if Language::Rust.workspace_markers().iter().any(|m| root.join(m).exists()) {
        languages.push(Language::Rust);
    }

    // TypeScript and JavaScript share package.json; tsconfig.json or .ts files disambiguate.
    let has_tsconfig = Language::TypeScript.workspace_markers().iter().any(|m| root.join(m).exists());
    let has_package_json = Language::JavaScript.workspace_markers().iter().any(|m| root.join(m).exists());

    if has_tsconfig || has_ts_files(root) {
        languages.push(Language::TypeScript);
    } else if has_package_json {
        languages.push(Language::JavaScript);
    }

    if Language::Go.workspace_markers().iter().any(|m| root.join(m).exists()) {
        languages.push(Language::Go);
    }

    if Language::Cpp.workspace_markers().iter().any(|m| root.join(m).exists()) {
        languages.push(Language::Cpp);
    }

    languages
}

fn has_ts_files(root: &Path) -> bool {
    let mut dirs = Vec::new();
    let src = root.join("src");
    if src.is_dir() {
        dirs.push(src);
    }
    dirs.push(root.to_path_buf());

    // Monorepo: scan well-known subdirectory conventions for tsconfig or .ts files
    for &pkg_dir in MONOREPO_DIRS {
        let pd = root.join(pkg_dir);
        if let Ok(entries) = std::fs::read_dir(&pd) {
            for entry in entries.filter_map(Result::ok) {
                let pkg = entry.path();
                if pkg.is_dir() {
                    // tsconfig.json in a package is a strong signal
                    if Language::TypeScript.workspace_markers().iter().any(|m| pkg.join(m).exists()) {
                        return true;
                    }
                    let pkg_src = pkg.join("src");
                    if pkg_src.is_dir() {
                        dirs.push(pkg_src);
                    }
                }
            }
        }
    }

    let ts_exts = Language::TypeScript.extensions();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        if entries.filter_map(Result::ok).any(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| ts_exts.contains(&x))
        }) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();

        let langs = detect_languages(dir.path());
        assert_eq!(langs, vec![Language::Rust]);
    }

    #[test]
    fn detects_typescript_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();

        let langs = detect_languages(dir.path());
        assert_eq!(langs, vec![Language::TypeScript]);
    }

    #[test]
    fn detects_typescript_from_package_json_with_ts_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/index.ts"), "").unwrap();

        let langs = detect_languages(dir.path());
        assert_eq!(langs, vec![Language::TypeScript]);
    }

    #[test]
    fn detects_typescript_monorepo_with_packages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let pkg = dir.path().join("packages/api");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("tsconfig.json"), "{}").unwrap();

        let langs = detect_languages(dir.path());
        assert_eq!(langs, vec![Language::TypeScript]);
    }

    #[test]
    fn detects_typescript_nested_under_src() {
        // Projects like `meet` where TS packages live under src/frontend, src/sdk/...
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("src/frontend");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("tsconfig.json"), "{}").unwrap();

        let langs = detect_languages(dir.path());
        assert_eq!(langs, vec![Language::TypeScript]);
    }

    #[test]
    fn detects_javascript_from_package_json_without_ts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let langs = detect_languages(dir.path());
        assert_eq!(langs, vec![Language::JavaScript]);
    }

    #[test]
    fn detects_go_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "").unwrap();

        let langs = detect_languages(dir.path());
        assert_eq!(langs, vec![Language::Go]);
    }

    #[test]
    fn detects_polyglot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let langs = detect_languages(dir.path());
        assert_eq!(langs, vec![Language::Rust, Language::JavaScript]);
    }

    #[test]
    fn empty_project_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let langs = detect_languages(dir.path());
        assert!(langs.is_empty());
    }
}
