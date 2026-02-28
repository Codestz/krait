use std::path::PathBuf;

use crate::detect::Language;

/// How to acquire an LSP server binary.
#[derive(Debug, Clone)]
pub enum InstallMethod {
    /// Download a standalone binary from a GitHub release.
    GithubRelease {
        repo: &'static str,
        /// Asset filename template. Placeholders: `{arch}`, `{platform}`.
        asset_pattern: &'static str,
        archive: ArchiveType,
    },
    /// Install via npm to `~/.krait/servers/npm/`.
    /// Requires `node` in PATH.
    Npm {
        package: &'static str,
        extra_packages: &'static [&'static str],
    },
    /// Install via `go install` to `~/.krait/servers/go/bin/`.
    /// Requires `go` in PATH.
    GoInstall { module: &'static str },
}

/// Archive format for downloaded files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveType {
    /// Single file compressed with gzip (`.gz`).
    Gzip,
}

/// Full metadata for an LSP server.
#[derive(Debug, Clone)]
pub struct ServerEntry {
    pub language: Language,
    pub binary_name: &'static str,
    pub args: &'static [&'static str],
    pub install_method: InstallMethod,
    pub install_advice: &'static str,
}

/// Get all server entries for a language, in preference order.
///
/// The first entry is the preferred server. Callers should try each in order
/// and use the first one found, or auto-install the preferred (first) one.
#[must_use]
pub fn get_entries(language: Language) -> Vec<ServerEntry> {
    match language {
        Language::Rust => vec![ServerEntry {
            language,
            binary_name: "rust-analyzer",
            args: &[],
            install_method: InstallMethod::GithubRelease {
                repo: "rust-lang/rust-analyzer",
                asset_pattern: "rust-analyzer-{arch}-{platform}.gz",
                archive: ArchiveType::Gzip,
            },
            install_advice: "Install: `rustup component add rust-analyzer`",
        }],
        Language::TypeScript | Language::JavaScript => vec![
            ServerEntry {
                language,
                binary_name: "vtsls",
                args: &["--stdio"],
                install_method: InstallMethod::Npm {
                    package: "@vtsls/language-server",
                    extra_packages: &["typescript"],
                },
                install_advice:
                    "Install: `npm install -g @vtsls/language-server typescript`",
            },
            ServerEntry {
                language,
                binary_name: "typescript-language-server",
                args: &["--stdio"],
                install_method: InstallMethod::Npm {
                    package: "typescript-language-server",
                    extra_packages: &["typescript"],
                },
                install_advice:
                    "Install: `npm install -g typescript-language-server typescript`",
            },
        ],
        Language::Go => vec![ServerEntry {
            language,
            binary_name: "gopls",
            args: &["serve"],
            install_method: InstallMethod::GoInstall {
                module: "golang.org/x/tools/gopls@latest",
            },
            install_advice: "Install: `go install golang.org/x/tools/gopls@latest`",
        }],
        Language::Cpp => vec![ServerEntry {
            language,
            binary_name: "clangd",
            args: &[],
            install_method: InstallMethod::GithubRelease {
                repo: "clangd/clangd",
                asset_pattern: "clangd-{platform}-{arch}.zip",
                archive: ArchiveType::Gzip,
            },
            install_advice: "Install: `brew install llvm` (includes clangd) or download from https://github.com/clangd/clangd/releases",
        }],
    }
}

/// Get the preferred (first) server entry for a language.
#[must_use]
pub fn get_entry(language: Language) -> Option<ServerEntry> {
    get_entries(language).into_iter().next()
}

/// Detect the current platform for download URL resolution.
/// Returns `(platform, arch)` matching rust-analyzer's naming convention.
#[must_use]
pub fn detect_platform() -> (&'static str, &'static str) {
    let platform = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "unknown"
    };

    (platform, arch)
}

/// Resolve the download URL for a GitHub release asset.
/// Returns `None` if the install method is not a GitHub release.
#[must_use]
pub fn resolve_download_url(entry: &ServerEntry) -> Option<String> {
    match &entry.install_method {
        InstallMethod::GithubRelease {
            repo,
            asset_pattern,
            ..
        } => {
            let (platform, arch) = detect_platform();
            let asset = asset_pattern
                .replace("{arch}", arch)
                .replace("{platform}", platform);
            Some(format!(
                "https://github.com/{repo}/releases/latest/download/{asset}"
            ))
        }
        _ => None,
    }
}

/// Global directory for managed LSP server binaries.
#[must_use]
pub fn servers_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".krait")
        .join("servers")
}

/// Check if a binary exists in PATH.
#[must_use]
pub fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    which::which(binary_name).ok()
}

/// Check if a managed binary exists in `~/.krait/servers/` or tool-specific locations.
#[must_use]
pub fn find_managed(binary_name: &str) -> Option<PathBuf> {
    let path = servers_dir().join(binary_name);
    if path.exists() && path.is_file() {
        return Some(path);
    }

    // npm bin directory
    let npm_path = servers_dir()
        .join("npm")
        .join("node_modules")
        .join(".bin")
        .join(binary_name);
    if npm_path.exists() {
        return Some(npm_path);
    }

    // go bin directory
    let go_path = servers_dir().join("go").join("bin").join(binary_name);
    if go_path.exists() {
        return Some(go_path);
    }

    // go install default output (~/$GOPATH/bin, falls back to ~/go/bin)
    if let Some(home) = dirs::home_dir() {
        let gopath =
            std::env::var("GOPATH").map_or_else(|_| home.join("go"), std::path::PathBuf::from);
        let go_default = gopath.join("bin").join(binary_name);
        if go_default.exists() {
            return Some(go_default);
        }
    }

    None
}

/// Find the server binary for a specific entry — checks PATH first, then managed directory.
#[must_use]
pub fn find_server(entry: &ServerEntry) -> Option<PathBuf> {
    find_in_path(entry.binary_name).or_else(|| find_managed(entry.binary_name))
}

/// Resolve the best available server for a language.
///
/// Tries each entry in preference order (e.g., vtsls before typescript-language-server).
/// Returns the first entry whose binary is found, along with its path.
/// If none found, returns `None`.
#[must_use]
pub fn resolve_server(language: Language) -> Option<(ServerEntry, PathBuf)> {
    for entry in get_entries(language) {
        if let Some(path) = find_server(&entry) {
            return Some((entry, path));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_entry_for_all_languages() {
        assert!(get_entry(Language::Rust).is_some());
        assert!(get_entry(Language::TypeScript).is_some());
        assert!(get_entry(Language::JavaScript).is_some());
        assert!(get_entry(Language::Go).is_some());
        assert!(get_entry(Language::Cpp).is_some());
    }

    #[test]
    fn platform_detection_returns_valid_tuple() {
        let (platform, arch) = detect_platform();
        assert!(
            ["apple-darwin", "unknown-linux-gnu", "unknown"].contains(&platform),
            "unexpected platform: {platform}"
        );
        assert!(
            ["aarch64", "x86_64", "unknown"].contains(&arch),
            "unexpected arch: {arch}"
        );
    }

    #[test]
    fn download_url_resolves_for_rust_analyzer() {
        let entry = get_entry(Language::Rust).unwrap();
        let url = resolve_download_url(&entry).unwrap();
        assert!(url.starts_with("https://github.com/rust-lang/rust-analyzer/releases/"));
        assert!(url.contains("rust-analyzer-"));
        assert!(url.contains(".gz"), "URL should contain .gz: {url}");
    }

    #[test]
    fn download_url_none_for_npm_packages() {
        let entry = get_entry(Language::TypeScript).unwrap();
        assert!(resolve_download_url(&entry).is_none());
    }

    #[test]
    fn typescript_and_javascript_share_entry() {
        let ts = get_entry(Language::TypeScript).unwrap();
        let js = get_entry(Language::JavaScript).unwrap();
        assert_eq!(ts.binary_name, js.binary_name);
        assert_eq!(ts.binary_name, "vtsls");
    }

    #[test]
    fn typescript_entries_have_vtsls_preferred() {
        let entries = get_entries(Language::TypeScript);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].binary_name, "vtsls");
        assert_eq!(entries[1].binary_name, "typescript-language-server");
    }

    #[test]
    fn resolve_server_returns_none_when_nothing_installed() {
        // This is a best-effort test — if neither vtsls nor ts-lang-server
        // is installed, it returns None. If one is, it returns it.
        let result = resolve_server(Language::TypeScript);
        if let Some((entry, path)) = result {
            assert!(path.exists());
            assert!(
                entry.binary_name == "vtsls" || entry.binary_name == "typescript-language-server"
            );
        }
    }

    #[test]
    fn servers_dir_is_under_home() {
        let dir = servers_dir();
        let home = dirs::home_dir().unwrap();
        assert!(
            dir.starts_with(&home),
            "servers_dir {dir:?} not under home {home:?}"
        );
        assert!(dir.ends_with("servers"));
    }

    #[test]
    fn find_managed_returns_none_for_missing() {
        assert!(find_managed("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn rust_entry_has_github_release_method() {
        let entry = get_entry(Language::Rust).unwrap();
        assert!(matches!(
            entry.install_method,
            InstallMethod::GithubRelease { .. }
        ));
    }

    #[test]
    fn vtsls_entry_has_npm_method() {
        let entry = get_entry(Language::TypeScript).unwrap();
        assert!(matches!(entry.install_method, InstallMethod::Npm { .. }));
        if let InstallMethod::Npm { package, .. } = entry.install_method {
            assert_eq!(package, "@vtsls/language-server");
        }
    }

    #[test]
    fn go_entry_has_go_install_method() {
        let entry = get_entry(Language::Go).unwrap();
        assert!(matches!(
            entry.install_method,
            InstallMethod::GoInstall { .. }
        ));
    }
}
