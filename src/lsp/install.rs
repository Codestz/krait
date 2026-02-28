use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use tracing::{debug, info, warn};

use super::registry::{
    get_entry, resolve_download_url, resolve_server, servers_dir, ArchiveType, InstallMethod,
    ServerEntry,
};
use crate::detect::Language;

/// Ensure the LSP server binary is available. Tries all servers for the language
/// in preference order (e.g., vtsls before typescript-language-server), then
/// auto-installs the preferred one if none found.
///
/// Returns `(binary_path, server_entry)` so callers know which server was resolved.
///
/// # Errors
/// Returns an error if the server cannot be found or downloaded.
pub async fn ensure_server(language: Language) -> anyhow::Result<(PathBuf, ServerEntry)> {
    // 1. Try all entries in preference order (e.g., vtsls → typescript-language-server)
    if let Some((entry, path)) = resolve_server(language) {
        debug!("found {}: {}", entry.binary_name, path.display());
        return Ok((path, entry));
    }

    // 2. None found — download the preferred (first) entry
    let entry =
        get_entry(language).with_context(|| format!("no LSP server configured for {language}"))?;

    info!("{} not found, downloading...", entry.binary_name);
    let path = download_server(&entry).await?;
    Ok((path, entry))
}

/// Download and install an LSP server binary.
///
/// # Errors
/// Returns an error if the download or installation fails.
pub async fn download_server(entry: &ServerEntry) -> anyhow::Result<PathBuf> {
    let dir = servers_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create servers directory: {}", dir.display()))?;

    match &entry.install_method {
        InstallMethod::GithubRelease { archive, .. } => {
            download_github_release(entry, &dir, *archive).await
        }
        InstallMethod::Npm {
            package,
            extra_packages,
        } => download_npm(entry, &dir, package, extra_packages).await,
        InstallMethod::GoInstall { module } => download_go(entry, &dir, module).await,
    }
}

/// Download a standalone binary from a GitHub release.
async fn download_github_release(
    entry: &ServerEntry,
    dir: &Path,
    archive: ArchiveType,
) -> anyhow::Result<PathBuf> {
    let url = resolve_download_url(entry).context("cannot resolve download URL for this server")?;

    let target = dir.join(entry.binary_name);
    let tmp = dir.join(format!(".{}.tmp", entry.binary_name));

    // Download with curl
    let download_status = tokio::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&tmp)
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .await
        .context("failed to run curl — is curl installed?")?;

    if !download_status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "failed to download {} from {url}\n  {}",
            entry.binary_name,
            entry.install_advice
        );
    }

    // Decompress
    match archive {
        ArchiveType::Gzip => {
            let gunzip_status = tokio::process::Command::new("gunzip")
                .args(["-f"])
                .arg(&tmp)
                .status()
                .await
                .context("failed to run gunzip")?;

            if !gunzip_status.success() {
                let _ = std::fs::remove_file(&tmp);
                bail!("failed to decompress {}", entry.binary_name);
            }

            // gunzip removes .tmp extension → file is now without .tmp
            // Actually gunzip strips the .gz, but our file doesn't end in .gz
            // gunzip -f on a non-.gz file renames to remove the extension
            let decompressed = dir.join(format!(".{}", entry.binary_name));
            if decompressed.exists() {
                std::fs::rename(&decompressed, &target)?;
            } else if tmp.exists() {
                // gunzip may have decompressed in-place
                std::fs::rename(&tmp, &target)?;
            }
        }
    }

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&target, perms).context("failed to make binary executable")?;
    }

    // Verify it can run
    let verify = tokio::process::Command::new(&target)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    match verify {
        Ok(status) if status.success() => {
            info!("installed {} to {}", entry.binary_name, target.display());
        }
        _ => {
            warn!(
                "{} downloaded but --version check failed (may still work)",
                entry.binary_name
            );
        }
    }

    Ok(target)
}

/// Install an npm package to `~/.krait/servers/npm/`.
async fn download_npm(
    entry: &ServerEntry,
    dir: &Path,
    package: &str,
    extra_packages: &[&str],
) -> anyhow::Result<PathBuf> {
    // Check if node is available
    if !command_exists("node") {
        bail!(
            "Node.js is required for {} but not found in PATH.\n  {}",
            entry.binary_name,
            entry.install_advice
        );
    }

    let npm_dir = dir.join("npm");
    std::fs::create_dir_all(&npm_dir)?;

    let mut args = vec!["install", "--prefix"];
    let npm_dir_str = npm_dir
        .to_str()
        .context("npm directory path is not valid UTF-8")?;
    args.push(npm_dir_str);
    args.push(package);
    for pkg in extra_packages {
        args.push(pkg);
    }

    let status = tokio::process::Command::new("npm")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .await
        .context("failed to run npm — is npm installed?")?;

    if !status.success() {
        bail!(
            "npm install failed for {}.\n  {}",
            package,
            entry.install_advice
        );
    }

    let bin_path = npm_dir
        .join("node_modules")
        .join(".bin")
        .join(entry.binary_name);

    if !bin_path.exists() {
        bail!(
            "{} not found after npm install at {}",
            entry.binary_name,
            bin_path.display()
        );
    }

    info!(
        "installed {} via npm to {}",
        entry.binary_name,
        bin_path.display()
    );
    Ok(bin_path)
}

/// Install a Go binary via `go install`.
async fn download_go(entry: &ServerEntry, dir: &Path, module: &str) -> anyhow::Result<PathBuf> {
    if !command_exists("go") {
        bail!(
            "Go is required for {} but not found in PATH.\n  {}",
            entry.binary_name,
            entry.install_advice
        );
    }

    let go_dir = dir.join("go");
    std::fs::create_dir_all(&go_dir)?;

    let status = tokio::process::Command::new("go")
        .args(["install", module])
        .env("GOPATH", &go_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .await
        .context("failed to run go install")?;

    if !status.success() {
        bail!(
            "go install failed for {}.\n  {}",
            module,
            entry.install_advice
        );
    }

    let bin_path = go_dir.join("bin").join(entry.binary_name);
    if !bin_path.exists() {
        bail!(
            "{} not found after go install at {}",
            entry.binary_name,
            bin_path.display()
        );
    }

    info!(
        "installed {} via go install to {}",
        entry.binary_name,
        bin_path.display()
    );
    Ok(bin_path)
}

/// Check if a command exists in PATH.
fn command_exists(name: &str) -> bool {
    which::which(name).is_ok()
}

/// Remove all managed server binaries from `~/.krait/servers/`.
///
/// Makes all files writable first (Go module cache uses read-only permissions).
///
/// # Errors
/// Returns an error if the directory cannot be removed.
pub fn clean_servers() -> anyhow::Result<u64> {
    let dir = servers_dir();
    if !dir.exists() {
        return Ok(0);
    }

    let size = dir_size(&dir);
    // Make everything writable before removal (Go module cache is read-only by default).
    make_writable_recursive(&dir);
    std::fs::remove_dir_all(&dir).context("failed to remove servers directory")?;
    Ok(size)
}

/// Recursively make all files and directories writable.
fn make_writable_recursive(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            let mode = perms.mode() | 0o200;
            perms.set_mode(mode);
            let _ = std::fs::set_permissions(path, perms);
        }
    }

    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.filter_map(Result::ok) {
                make_writable_recursive(&entry.path());
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() | 0o700);
                let _ = std::fs::set_permissions(path, perms);
            }
        }
    }
}

fn dir_size(path: &Path) -> u64 {
    std::fs::read_dir(path).ok().map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|e| {
                if e.path().is_dir() {
                    dir_size(&e.path())
                } else {
                    e.metadata().map_or(0, |m| m.len())
                }
            })
            .sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_dir_creates_if_missing() {
        let dir = servers_dir();
        // We don't actually create it in this test (side effect),
        // just verify the path is valid
        assert!(dir.to_str().is_some());
        assert!(dir.ends_with("servers"));
    }

    #[test]
    fn clean_empty_is_ok() {
        // Skip if servers are actually installed — we don't want to wipe them.
        let dir = servers_dir();
        if dir.exists()
            && std::fs::read_dir(&dir)
                .map(|mut e| e.next().is_some())
                .unwrap_or(false)
        {
            return;
        }
        let result = clean_servers();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn ensure_server_finds_rust_analyzer_if_installed() {
        // This test doesn't download — it just checks if RA is in PATH
        if which::which("rust-analyzer").is_err() {
            return; // skip if not installed
        }
        let (path, entry) = ensure_server(Language::Rust).await.unwrap();
        assert!(path.exists());
        assert_eq!(entry.binary_name, "rust-analyzer");
    }

    #[test]
    fn command_exists_finds_curl() {
        assert!(command_exists("curl"));
    }

    #[test]
    fn command_exists_rejects_missing() {
        assert!(!command_exists("definitely-not-a-real-binary-xyz-123"));
    }
}
