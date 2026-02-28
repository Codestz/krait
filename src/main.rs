use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use std::io::Read as _;

use std::time::Duration;

use krait::cli::{Cli, Command, DaemonCommand, EditCommand, OutputFormat, ServerCommand};
use krait::commands;
use krait::client::{self, DaemonClient};
use krait::daemon;
use krait::detect;
use krait::output;
use krait::protocol::Request;

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let cwd = std::env::current_dir()?;
    let project_root = detect::detect_project_root(&cwd);
    let format = cli.format;

    match &cli.command {
        Command::Server(server_cmd) => {
            handle_server_command(server_cmd, &project_root, format).await?;
            return Ok(());
        }
        Command::Daemon(daemon_cmd) => {
            handle_daemon_command(daemon_cmd, &project_root, format).await
        }
        Command::Init { force, dry_run } => {
            handle_init(&project_root, *force, *dry_run, format).await
        }
        Command::Check { path, errors_only } => {
            let socket_path = detect::socket_path(&project_root);
            let request = Request::Check {
                path: path.clone(),
                errors_only: *errors_only,
            };
            let mut client = DaemonClient::connect_or_start(&socket_path).await?;
            let response = client.send(&request).await?;
            let formatted = output::format_response(&response, format);

            if !response.success {
                eprintln!("{formatted}");
                std::process::exit(1);
            }

            if !formatted.is_empty() {
                println!("{formatted}");
            }

            // Exit 1 when the project has compiler errors (not just warnings)
            let has_errors = response
                .data
                .as_ref()
                .and_then(|d| d.get("errors"))
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|n| n > 0);
            if has_errors {
                std::process::exit(1);
            }

            Ok(())
        }
        Command::Edit(edit_cmd) => {
            // Read stdin before connecting — edit commands require piped input.
            let code = read_edit_stdin()?;
            let socket_path = detect::socket_path(&project_root);
            let request = match edit_cmd {
                EditCommand::Replace { symbol } => Request::EditReplace {
                    symbol: symbol.clone(),
                    code,
                },
                EditCommand::InsertAfter { symbol } => Request::EditInsertAfter {
                    symbol: symbol.clone(),
                    code,
                },
                EditCommand::InsertBefore { symbol } => Request::EditInsertBefore {
                    symbol: symbol.clone(),
                    code,
                },
            };

            let mut client = DaemonClient::connect_or_start(&socket_path).await?;
            let response = client.send(&request).await?;
            let formatted = output::format_response(&response, format);

            if response.success {
                if !formatted.is_empty() {
                    println!("{formatted}");
                }
            } else {
                eprintln!("{formatted}");
                std::process::exit(1);
            }

            Ok(())
        }
        Command::Watch { path, interval } => {
            handle_watch(path.clone(), *interval, &project_root, format).await
        }
        Command::Search {
            pattern,
            path,
            ignore_case,
            word,
            literal,
            context,
            files,
            r#type,
            max,
        } => {
            let opts = commands::search::SearchOptions {
                pattern: pattern.clone(),
                path: path.clone(),
                ignore_case: *ignore_case,
                word: *word,
                literal: *literal,
                context: *context,
                files_only: *files,
                lang_filter: r#type.clone(),
                max_matches: max.unwrap_or(200),
            };
            let search_output = commands::search::run(&opts, &project_root)?;
            let formatted =
                output::format_search(&search_output, format, *context > 0, *files);
            if search_output.matches.is_empty() && !*files {
                eprintln!("no matches");
                std::process::exit(1);
            }
            println!("{formatted}");
            Ok(())
        }
        command => {
            let socket_path = detect::socket_path(&project_root);
            let request = client::command_to_request(command);

            let mut client = DaemonClient::connect_or_start(&socket_path).await?;
            let response = client.send(&request).await?;
            let formatted = output::format_response(&response, format);

            if response.success {
                if !formatted.is_empty() {
                    println!("{formatted}");
                }
            } else {
                eprintln!("{formatted}");
                std::process::exit(1);
            }

            Ok(())
        }
    }
}

async fn handle_watch(
    path: Option<std::path::PathBuf>,
    interval_ms: u64,
    project_root: &std::path::Path,
    format: OutputFormat,
) -> Result<()> {
    let socket_path = detect::socket_path(project_root);
    loop {
        let ts = utc_time_str();
        match DaemonClient::connect_or_start(&socket_path).await {
            Ok(mut daemon_client) => {
                let request = krait::protocol::Request::Check {
                    path: path.clone(),
                    errors_only: false,
                };
                match daemon_client.send(&request).await {
                    Ok(response) => {
                        let formatted = output::format_response(&response, format);
                        let msg =
                            if formatted.is_empty() { "No diagnostics" } else { formatted.trim() };
                        println!("[{ts}] {msg}");
                    }
                    Err(e) => eprintln!("[{ts}] error: {e}"),
                }
            }
            Err(e) => eprintln!("[{ts}] error: {e}"),
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            () = tokio::time::sleep(Duration::from_millis(interval_ms)) => {}
        }
    }
    Ok(())
}

/// Return current UTC time as HH:MM:SS.
fn utc_time_str() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

async fn handle_init(
    project_root: &std::path::Path,
    force: bool,
    dry_run: bool,
    format: OutputFormat,
) -> Result<()> {
    use krait::config;

    // Check if config already exists
    if !force && config::config_exists(project_root) {
        eprintln!(".krait/krait.toml already exists. Use --force to overwrite.");
        std::process::exit(1);
    }

    // Auto-detect workspaces
    let package_roots = detect::find_package_roots(project_root);

    eprintln!("Detected project root: {}", project_root.display());
    eprintln!("Detected workspaces:");
    for (lang, root) in &package_roots {
        let rel = root
            .strip_prefix(project_root)
            .unwrap_or(root)
            .to_string_lossy();
        let label = if rel.is_empty() { "." } else { &rel };
        eprintln!("  {lang:<12}  {label}");
    }

    let content = config::generate(&package_roots, project_root);

    if dry_run {
        eprintln!("\n--- .krait/krait.toml (dry run) ---");
        print!("{content}");
        return Ok(());
    }

    config::write_config(project_root, &content)?;
    eprintln!(
        "\nWritten: .krait/krait.toml ({} workspaces)",
        package_roots.len()
    );

    // Build the symbol index via the daemon
    eprintln!("Building symbol index...");
    let socket_path = detect::socket_path(project_root);
    let mut client = DaemonClient::connect_or_start(&socket_path).await?;
    let response = client.send(&krait::protocol::Request::Init).await?;
    let formatted = output::format_response(&response, format);

    if response.success {
        if !formatted.is_empty() {
            eprintln!("{formatted}");
        }
    } else {
        eprintln!("warning: index build failed: {formatted}");
    }

    Ok(())
}

/// Read code from stdin for edit commands.
///
/// Warns on TTY (user forgot to pipe), errors on empty input.
fn read_edit_stdin() -> Result<String> {
    use std::io::IsTerminal as _;
    if std::io::stdin().is_terminal() {
        eprintln!(
            "This command reads from stdin. Pipe code into it:\n  \
             echo 'fn new_body() {{}}' | krait edit replace <symbol>"
        );
    }
    let mut code = String::new();
    std::io::stdin().read_to_string(&mut code)?;
    if code.trim().is_empty() {
        anyhow::bail!(
            "No input. Pipe code into this command:\n  \
             echo 'fn new_body() {{}}' | krait edit replace <symbol>"
        );
    }
    Ok(code)
}

async fn handle_server_command(
    cmd: &ServerCommand,
    project_root: &std::path::Path,
    format: OutputFormat,
) -> Result<()> {
    match cmd {
        ServerCommand::List => commands::server::handle_list(format),
        ServerCommand::Install { lang, reinstall } => {
            commands::server::handle_install(lang.as_deref(), *reinstall, format).await
        }
        ServerCommand::Clean => commands::server::handle_clean(format),
        ServerCommand::Status => {
            let socket_path = detect::socket_path(project_root);
            let mut client = DaemonClient::connect_or_start(&socket_path).await?;
            let response = client.send(&krait::protocol::Request::ServerStatus).await?;
            let formatted = output::format_response(&response, format);
            if response.success {
                if !formatted.is_empty() {
                    println!("{formatted}");
                }
            } else {
                eprintln!("{formatted}");
                std::process::exit(1);
            }
            Ok(())
        }
        ServerCommand::Restart { lang } => {
            let socket_path = detect::socket_path(project_root);
            let mut client = DaemonClient::connect_or_start(&socket_path).await?;
            let response = client
                .send(&krait::protocol::Request::ServerRestart {
                    language: lang.clone(),
                })
                .await?;
            let formatted = output::format_response(&response, format);
            if response.success {
                if !formatted.is_empty() {
                    println!("{formatted}");
                }
            } else {
                eprintln!("{formatted}");
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

async fn handle_daemon_command(
    cmd: &DaemonCommand,
    project_root: &std::path::Path,
    format: OutputFormat,
) -> Result<()> {
    match cmd {
        DaemonCommand::Start => {
            daemon::run_daemon(project_root).await?;
        }
        DaemonCommand::Stop => {
            let socket_path = detect::socket_path(project_root);
            let mut client = DaemonClient::connect(&socket_path).await?;
            let resp = client.send(&krait::protocol::Request::DaemonStop).await?;
            if resp.success {
                eprintln!("daemon stopped");
            }
        }
        DaemonCommand::Status => {
            let socket_path = detect::socket_path(project_root);
            let mut client = DaemonClient::connect(&socket_path).await?;
            let resp = client.send(&krait::protocol::Request::Status).await?;
            println!("{}", output::format_response(&resp, format));
        }
    }
    Ok(())
}
