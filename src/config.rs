use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::detect::Language;

/// Primary config file name (inside .krait/ directory).
const CONFIG_FILE: &str = ".krait/krait.toml";

/// Legacy config location (project root, for backwards compat).
const LEGACY_CONFIG_FILE: &str = "krait.toml";

/// Parsed project configuration from `krait.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Project root override (relative to config file location).
    pub root: Option<String>,

    /// Workspace entries — each gets its own LSP server.
    #[serde(default)]
    pub workspace: Vec<WorkspaceEntry>,

    /// Per-language server overrides.
    #[serde(default)]
    pub servers: HashMap<String, ServerOverride>,

    /// Workspaces to pre-warm and exempt from LRU eviction.
    /// Paths are relative to the project root.
    #[serde(default)]
    pub primary_workspaces: Vec<String>,

    /// Maximum concurrent LSP sessions for LRU fallback (default: 10).
    pub max_active_sessions: Option<usize>,

    /// Maximum concurrent language server processes across all languages (default: unlimited).
    /// When exceeded, the least-recently-used language server is shut down.
    pub max_language_servers: Option<usize>,
}

/// One workspace scope for LSP indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    /// Relative path to workspace root (e.g., "packages/api").
    pub path: String,

    /// Language identifier (e.g., "typescript", "rust").
    pub language: String,

    /// Server binary override (e.g., "vtsls").
    pub server: Option<String>,
}

/// Override default server binary/args for a language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerOverride {
    /// Binary name or path.
    pub binary: Option<String>,

    /// Command-line arguments.
    pub args: Option<Vec<String>>,
}

/// Where the config was loaded from.
#[derive(Debug, Clone)]
pub enum ConfigSource {
    /// Loaded from `.krait/krait.toml`.
    KraitToml,
    /// Loaded from legacy `krait.toml` at project root.
    LegacyKraitToml,
    /// No config file found — using auto-detection.
    AutoDetected,
}

impl ConfigSource {
    /// Human-readable label for status output.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::KraitToml => ".krait/krait.toml",
            Self::LegacyKraitToml => "krait.toml",
            Self::AutoDetected => "auto-detected",
        }
    }
}

/// Result of loading config: the config (if any) and its source.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Option<ProjectConfig>,
    pub source: ConfigSource,
}

/// Try to load `.krait/krait.toml` (or legacy `krait.toml`) from the project root.
///
/// Returns `None` config with `AutoDetected` source if no config file exists.
#[must_use]
pub fn load(project_root: &Path) -> LoadedConfig {
    // Try .krait/krait.toml first
    let primary = project_root.join(CONFIG_FILE);
    if primary.is_file() {
        match load_from_file(&primary) {
            Ok(config) => {
                debug!("loaded config from {}", primary.display());
                return LoadedConfig {
                    config: Some(config),
                    source: ConfigSource::KraitToml,
                };
            }
            Err(e) => {
                warn!("failed to parse {}: {e}", primary.display());
            }
        }
    }

    // Try legacy krait.toml at project root
    let legacy = project_root.join(LEGACY_CONFIG_FILE);
    if legacy.is_file() {
        match load_from_file(&legacy) {
            Ok(config) => {
                debug!("loaded config from {} (legacy location)", legacy.display());
                return LoadedConfig {
                    config: Some(config),
                    source: ConfigSource::LegacyKraitToml,
                };
            }
            Err(e) => {
                warn!("failed to parse {}: {e}", legacy.display());
            }
        }
    }

    debug!("no config found, using auto-detection");
    LoadedConfig {
        config: None,
        source: ConfigSource::AutoDetected,
    }
}

fn load_from_file(path: &Path) -> anyhow::Result<ProjectConfig> {
    let content = std::fs::read_to_string(path)?;
    let config: ProjectConfig = toml::from_str(&content)?;
    Ok(config)
}

/// Convert a loaded config into `(Language, PathBuf)` pairs for the multiplexer.
///
/// Validates each entry: skips entries with unknown languages or missing directories.
#[must_use]
pub fn config_to_package_roots(
    config: &ProjectConfig,
    project_root: &Path,
) -> Vec<(Language, PathBuf)> {
    let mut roots = Vec::new();

    for entry in &config.workspace {
        let Some(lang) = parse_language(&entry.language) else {
            warn!(
                "unknown language '{}' in krait.toml, skipping workspace '{}'",
                entry.language, entry.path
            );
            continue;
        };

        let abs_path = project_root.join(&entry.path);
        if !abs_path.is_dir() {
            warn!("workspace path '{}' does not exist, skipping", entry.path);
            continue;
        }

        roots.push((lang, abs_path));
    }

    roots
}

/// Parse a language name string into a `Language` enum.
#[must_use]
pub fn parse_language(name: &str) -> Option<Language> {
    match name.to_lowercase().as_str() {
        "rust" => Some(Language::Rust),
        "typescript" | "ts" => Some(Language::TypeScript),
        "javascript" | "js" => Some(Language::JavaScript),
        "go" | "golang" => Some(Language::Go),
        "cpp" | "c++" | "cxx" | "c" => Some(Language::Cpp),
        _ => None,
    }
}

/// Generate a `krait.toml` config string from detected workspace roots.
#[must_use]
pub fn generate(package_roots: &[(Language, PathBuf)], project_root: &Path) -> String {
    let mut out = String::from("# krait.toml — generated by `krait init`\n");
    out.push_str("# Edit this file to customize which workspaces to index.\n");
    out.push_str("# Remove entries you don't need. Run `krait daemon stop` after changes.\n\n");

    for (lang, abs_path) in package_roots {
        let rel = abs_path
            .strip_prefix(project_root)
            .unwrap_or(abs_path)
            .to_string_lossy();
        let path_str = if rel.is_empty() { "." } else { &rel };

        let _ = writeln!(out, "[[workspace]]");
        let _ = writeln!(out, "path = \"{path_str}\"");
        let _ = writeln!(out, "language = \"{}\"", lang.name());
        out.push('\n');
    }

    // Add commented primary_workspaces section
    out.push_str("# Priority workspaces — always warm, exempt from LRU eviction\n");
    out.push_str("# primary_workspaces = [\"packages/core\", \"packages/api\"]\n\n");

    // Add commented max_active_sessions
    out.push_str("# Maximum concurrent LSP sessions for non-multi-root servers (default: 10)\n");
    out.push_str("# max_active_sessions = 10\n\n");

    out.push_str("# Maximum concurrent language server processes across all languages (default: unlimited)\n");
    out.push_str("# When exceeded, the least-recently-used language server is shut down.\n");
    out.push_str("# max_language_servers = 10\n\n");

    // Add commented server overrides section
    out.push_str("# Server overrides (uncomment to customize)\n");
    out.push_str("# [servers.typescript]\n");
    out.push_str("# binary = \"vtsls\"\n");
    out.push_str("# args = [\"--stdio\"]\n");

    out
}

/// Write config to `.krait/krait.toml`, creating `.krait/` if needed.
///
/// # Errors
/// Returns an error if the directory or file cannot be written.
pub fn write_config(project_root: &Path, content: &str) -> anyhow::Result<()> {
    let krait_dir = project_root.join(".krait");
    std::fs::create_dir_all(&krait_dir)?;
    let path = project_root.join(CONFIG_FILE);
    std::fs::write(&path, content)?;
    Ok(())
}

/// Check if a config file already exists.
#[must_use]
pub fn config_exists(project_root: &Path) -> bool {
    project_root.join(CONFIG_FILE).is_file() || project_root.join(LEGACY_CONFIG_FILE).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_language_variants() {
        assert_eq!(parse_language("rust"), Some(Language::Rust));
        assert_eq!(parse_language("typescript"), Some(Language::TypeScript));
        assert_eq!(parse_language("ts"), Some(Language::TypeScript));
        assert_eq!(parse_language("javascript"), Some(Language::JavaScript));
        assert_eq!(parse_language("js"), Some(Language::JavaScript));
        assert_eq!(parse_language("go"), Some(Language::Go));
        assert_eq!(parse_language("golang"), Some(Language::Go));
        assert_eq!(parse_language("c++"), Some(Language::Cpp));
        assert_eq!(parse_language("cpp"), Some(Language::Cpp));
        assert_eq!(parse_language("unknown"), None);
    }

    #[test]
    fn parse_language_case_insensitive() {
        assert_eq!(parse_language("Rust"), Some(Language::Rust));
        assert_eq!(parse_language("TYPESCRIPT"), Some(Language::TypeScript));
    }

    #[test]
    fn load_returns_auto_detected_when_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load(dir.path());
        assert!(loaded.config.is_none());
        assert!(matches!(loaded.source, ConfigSource::AutoDetected));
    }

    #[test]
    fn load_reads_krait_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".krait")).unwrap();
        let config_content = r#"
[[workspace]]
path = "."
language = "rust"
"#;
        std::fs::write(dir.path().join(".krait/krait.toml"), config_content).unwrap();

        let loaded = load(dir.path());
        assert!(loaded.config.is_some());
        assert!(matches!(loaded.source, ConfigSource::KraitToml));

        let config = loaded.config.unwrap();
        assert_eq!(config.workspace.len(), 1);
        assert_eq!(config.workspace[0].path, ".");
        assert_eq!(config.workspace[0].language, "rust");
    }

    #[test]
    fn load_reads_legacy_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_content = r#"
[[workspace]]
path = "."
language = "go"
"#;
        std::fs::write(dir.path().join("krait.toml"), config_content).unwrap();

        let loaded = load(dir.path());
        assert!(loaded.config.is_some());
        assert!(matches!(loaded.source, ConfigSource::LegacyKraitToml));
    }

    #[test]
    fn krait_toml_takes_priority_over_legacy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".krait")).unwrap();
        std::fs::write(
            dir.path().join(".krait/krait.toml"),
            "[[workspace]]\npath = \".\"\nlanguage = \"rust\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("krait.toml"),
            "[[workspace]]\npath = \".\"\nlanguage = \"go\"\n",
        )
        .unwrap();

        let loaded = load(dir.path());
        let config = loaded.config.unwrap();
        assert_eq!(config.workspace[0].language, "rust");
        assert!(matches!(loaded.source, ConfigSource::KraitToml));
    }

    #[test]
    fn config_to_package_roots_validates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();

        let config = ProjectConfig {
            root: None,
            workspace: vec![
                WorkspaceEntry {
                    path: "src".to_string(),
                    language: "rust".to_string(),
                    server: None,
                },
                WorkspaceEntry {
                    path: "nonexistent".to_string(),
                    language: "rust".to_string(),
                    server: None,
                },
                WorkspaceEntry {
                    path: "src".to_string(),
                    language: "fakeLang".to_string(),
                    server: None,
                },
            ],
            servers: HashMap::new(),
            primary_workspaces: vec![],
            max_active_sessions: None,
            max_language_servers: None,
        };

        let roots = config_to_package_roots(&config, dir.path());
        assert_eq!(roots.len(), 1, "only valid entries should be returned");
        assert_eq!(roots[0].0, Language::Rust);
    }

    #[test]
    fn generate_produces_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![
            (Language::TypeScript, dir.path().join("packages/api")),
            (Language::TypeScript, dir.path().join("packages/web")),
        ];

        let content = generate(&roots, dir.path());
        assert!(content.contains("[[workspace]]"));
        assert!(content.contains("packages/api"));
        assert!(content.contains("packages/web"));
        assert!(content.contains("language = \"typescript\""));

        // Verify it parses back
        let parsed: ProjectConfig = toml::from_str(&content).unwrap();
        assert_eq!(parsed.workspace.len(), 2);
    }

    #[test]
    fn generate_dot_for_root_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![(Language::Rust, dir.path().to_path_buf())];

        let content = generate(&roots, dir.path());
        assert!(content.contains("path = \".\""));
    }

    #[test]
    fn config_exists_detects_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!config_exists(dir.path()));

        std::fs::create_dir(dir.path().join(".krait")).unwrap();
        std::fs::write(dir.path().join(".krait/krait.toml"), "").unwrap();
        assert!(config_exists(dir.path()));
    }

    #[test]
    fn config_with_primary_workspaces() {
        let content = r#"
primary_workspaces = ["packages/core", "packages/api"]
max_active_sessions = 5

[[workspace]]
path = "."
language = "typescript"
"#;
        let config: ProjectConfig = toml::from_str(content).unwrap();
        assert_eq!(
            config.primary_workspaces,
            vec!["packages/core", "packages/api"]
        );
        assert_eq!(config.max_active_sessions, Some(5));
    }

    #[test]
    fn config_defaults_for_optional_fields() {
        let content = r#"
[[workspace]]
path = "."
language = "rust"
"#;
        let config: ProjectConfig = toml::from_str(content).unwrap();
        assert!(config.primary_workspaces.is_empty());
        assert!(config.max_active_sessions.is_none());
    }

    #[test]
    fn generate_includes_priority_and_sessions_comments() {
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![(Language::Rust, dir.path().to_path_buf())];
        let content = generate(&roots, dir.path());
        assert!(content.contains("primary_workspaces"));
        assert!(content.contains("max_active_sessions"));
    }

    #[test]
    fn config_with_server_overrides() {
        let content = r#"
[[workspace]]
path = "."
language = "typescript"

[servers.typescript]
binary = "vtsls"
args = ["--stdio"]
"#;
        let config: ProjectConfig = toml::from_str(content).unwrap();
        assert_eq!(config.workspace.len(), 1);
        let ts_server = config.servers.get("typescript").unwrap();
        assert_eq!(ts_server.binary.as_deref(), Some("vtsls"));
        assert_eq!(
            ts_server.args.as_deref(),
            Some(&["--stdio".to_string()][..])
        );
    }
}
