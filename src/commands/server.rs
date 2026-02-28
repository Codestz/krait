use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::cli::OutputFormat;
use crate::config::parse_language;
use crate::detect::Language;
use crate::lsp::install;
use crate::lsp::registry::{find_server, get_entries, servers_dir};

/// Row in the server list table.
#[derive(serde::Serialize)]
pub struct ServerListEntry {
    pub language: String,
    pub server_name: String,
    pub status: String,
    pub path: String,
    pub install_advice: String,
}

/// Build the list of all servers and their install status.
#[must_use] 
pub fn build_server_list() -> Vec<ServerListEntry> {
    let mut seen_binaries = std::collections::HashSet::new();
    let mut rows = Vec::new();

    for &lang in Language::ALL {
        let entries = get_entries(lang);
        let Some(preferred) = entries.first() else { continue };

        // Skip JavaScript if it shares the same binary as TypeScript (already shown).
        if seen_binaries.contains(preferred.binary_name) {
            // Still emit a row but mark it as shared.
            rows.push(ServerListEntry {
                language: lang.name().to_string(),
                server_name: preferred.binary_name.to_string(),
                status: "shared".to_string(),
                path: "(shared with typescript)".to_string(),
                install_advice: preferred.install_advice.to_string(),
            });
            continue;
        }

        let (status, path) = match find_server(preferred) {
            Some(p) => ("installed".to_string(), p.to_string_lossy().to_string()),
            None => (
                "not installed".to_string(),
                format!("run: krait server install {}", lang.name()),
            ),
        };

        seen_binaries.insert(preferred.binary_name);
        rows.push(ServerListEntry {
            language: lang.name().to_string(),
            server_name: preferred.binary_name.to_string(),
            status,
            path,
            install_advice: preferred.install_advice.to_string(),
        });
    }

    rows
}

/// Handle `krait server list`.
///
/// # Errors
/// Returns an error if JSON serialization fails.
pub fn handle_list(format: OutputFormat) -> anyhow::Result<()> {
    let rows = build_server_list();
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        _ => {
            println!("{}", format_server_list(&rows));
        }
    }
    Ok(())
}

/// Handle `krait server install [lang] [--reinstall]`.
///
/// # Errors
/// Returns an error if the language is unknown or download fails.
pub async fn handle_install(
    lang: Option<&str>,
    reinstall: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let languages: Vec<Language> = if let Some(name) = lang {
        let l = parse_language(name)
            .ok_or_else(|| anyhow::anyhow!("unknown language: {name}"))?;
        vec![l]
    } else {
        Language::ALL.to_vec()
    };

    let mut any_installed = false;

    for language in languages {
        let entries = get_entries(language);
        let preferred = match entries.first() {
            Some(e) => e.clone(),
            None => continue,
        };

        // Skip if already installed (unless --reinstall).
        if !reinstall {
            if let Some(path) = find_server(&preferred) {
                if lang.is_some() {
                    // Explicit single-lang install: report already installed.
                    let msg = format!(
                        "{} already installed at {}",
                        preferred.binary_name,
                        path.display()
                    );
                    match format {
                        OutputFormat::Json => {
                            println!(
                                "{}",
                                serde_json::to_string(&json!({
                                    "language": language.name(),
                                    "server_name": preferred.binary_name,
                                    "status": "already_installed",
                                    "path": path.to_string_lossy()
                                }))?
                            );
                        }
                        _ => println!("{msg}"),
                    }
                }
                continue;
            }
        }

        // If reinstall and managed, delete managed binary first.
        if reinstall {
            let managed_dir = servers_dir();
            let managed = managed_dir.join(preferred.binary_name);
            if managed.exists() {
                std::fs::remove_file(&managed)
                    .unwrap_or_else(|e| tracing::warn!("could not remove {}: {e}", managed.display()));
            }
        }

        match install::download_server(&preferred).await {
            Ok(path) => {
                any_installed = true;
                match format {
                    OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::to_string(&json!({
                                "installed": preferred.binary_name,
                                "language": language.name(),
                                "path": path.to_string_lossy()
                            }))?
                        );
                    }
                    _ => println!(
                        "installed {} → {}",
                        preferred.binary_name,
                        path.display()
                    ),
                }
            }
            Err(e) => {
                eprintln!("error: failed to install {}: {e}", preferred.binary_name);
            }
        }
    }

    // When installing all, summarise if nothing was missing.
    if lang.is_none() && !any_installed {
        match format {
            OutputFormat::Json => {}
            _ => println!("all servers already installed"),
        }
    }

    Ok(())
}

/// Handle `krait server clean`.
///
/// # Errors
/// Returns an error if the clean operation or JSON serialization fails.
pub fn handle_clean(format: OutputFormat) -> anyhow::Result<()> {
    let bytes = install::clean_servers()?;
    #[allow(clippy::cast_precision_loss)]
    let mb = bytes as f64 / 1_048_576.0;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "cleaned": true,
                    "path": servers_dir().to_string_lossy(),
                    "bytes_freed": bytes
                }))?
            );
        }
        _ => {
            if bytes == 0 {
                println!("nothing to clean (~/.krait/servers/ was empty or missing)");
            } else {
                println!(
                    "cleaned ~/.krait/servers/ ({mb:.1} MB freed)"
                );
            }
        }
    }
    Ok(())
}

/// Compact text formatter for server list (used by output/compact.rs too).
#[must_use] 
pub fn format_server_list(rows: &[ServerListEntry]) -> String {
    if rows.is_empty() {
        return "no servers configured".to_string();
    }

    // Column widths
    let lang_w = rows.iter().map(|r| r.language.len()).max().unwrap_or(0).max(8);
    let name_w = rows.iter().map(|r| r.server_name.len()).max().unwrap_or(0).max(11);
    let stat_w = rows.iter().map(|r| r.status.len()).max().unwrap_or(0).max(13);

    let mut out = String::new();
    for row in rows {
        let _ = writeln!(
            out,
            "{:<lang_w$}  {:<name_w$}  {:<stat_w$}  {}",
            row.language, row.server_name, row.status, row.path,
            lang_w = lang_w,
            name_w = name_w,
            stat_w = stat_w,
        );
    }

    // Advice for not-installed entries
    let missing: Vec<&ServerListEntry> = rows
        .iter()
        .filter(|r| r.status == "not installed")
        .collect();
    if !missing.is_empty() {
        out.push('\n');
        for row in missing {
            let _ = writeln!(out, "  {}", row.install_advice);
        }
    }

    out.trim_end().to_string()
}

/// Format a JSON server list value (array of objects) for compact output.
/// Used by compact.rs when the daemon response is a server list.
#[must_use] 
pub fn format_server_list_json(items: &[Value]) -> String {
    let rows: Vec<ServerListEntry> = items
        .iter()
        .filter_map(|v| {
            Some(ServerListEntry {
                language: v.get("language")?.as_str()?.to_string(),
                server_name: v.get("server_name")?.as_str()?.to_string(),
                status: v.get("status")?.as_str()?.to_string(),
                path: v.get("path")?.as_str()?.to_string(),
                install_advice: v
                    .get("install_advice")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect();
    format_server_list(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_server_list_covers_all_languages() {
        let rows = build_server_list();
        // Should have one row per Language::ALL entry
        assert_eq!(rows.len(), Language::ALL.len());
    }

    #[test]
    fn build_server_list_has_rust_analyzer() {
        let rows = build_server_list();
        let rust = rows.iter().find(|r| r.language == "rust").unwrap();
        assert_eq!(rust.server_name, "rust-analyzer");
    }

    #[test]
    fn format_server_list_empty() {
        assert_eq!(format_server_list(&[]), "no servers configured");
    }

    #[test]
    fn format_server_list_installed() {
        let rows = vec![ServerListEntry {
            language: "rust".to_string(),
            server_name: "rust-analyzer".to_string(),
            status: "installed".to_string(),
            path: "/usr/local/bin/rust-analyzer".to_string(),
            install_advice: String::new(),
        }];
        let out = format_server_list(&rows);
        assert!(out.contains("rust"));
        assert!(out.contains("rust-analyzer"));
        assert!(out.contains("installed"));
    }
}
