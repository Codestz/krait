use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "krait",
    about = "Code intelligence CLI for AI agents",
    version,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Output format
    #[arg(long, global = true, default_value = "compact")]
    pub format: OutputFormat,

    /// Enable verbose logging
    #[arg(long, global = true)]
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Compact,
    Json,
    Human,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Generate krait.toml config from auto-detected workspaces
    Init {
        /// Overwrite existing krait.toml
        #[arg(long)]
        force: bool,

        /// Print what would be generated without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// Show daemon health, LSP state, cache stats
    Status,

    /// Show LSP diagnostics (errors/warnings)
    Check {
        /// File path to check (all files if omitted)
        path: Option<PathBuf>,

        /// Show only errors, suppress warnings/hints
        #[arg(long)]
        errors_only: bool,
    },

    /// Find symbols and references
    #[command(subcommand)]
    Find(FindCommand),

    /// List symbols or other resources
    #[command(subcommand)]
    List(ListCommand),

    /// Read files or symbol bodies
    #[command(subcommand)]
    Read(ReadCommand),

    /// Semantic code editing via stdin
    #[command(subcommand)]
    Edit(EditCommand),

    /// Manage the background daemon
    #[command(subcommand)]
    Daemon(DaemonCommand),

    /// Show type info and documentation for a symbol
    Hover {
        /// Symbol name to hover over
        name: String,
    },

    /// Format a file using the LSP formatter
    Format {
        /// File path to format
        path: std::path::PathBuf,
    },

    /// Rename a symbol across all files
    Rename {
        /// Symbol name to rename
        symbol: String,
        /// New name for the symbol
        new_name: String,
    },

    /// Apply LSP quick-fix code actions for current diagnostics
    Fix {
        /// File path to fix (all files with diagnostics if omitted)
        path: Option<std::path::PathBuf>,
    },

    /// Poll for diagnostics and print timestamped results
    Watch {
        /// Path to check (all files if omitted)
        path: Option<std::path::PathBuf>,
        /// Polling interval in milliseconds
        #[arg(long, default_value = "1500")]
        interval: u64,
    },

    /// Manage LSP server installations
    #[command(subcommand)]
    Server(ServerCommand),

    /// Search for a pattern in project files
    Search {
        /// Pattern to search for (regex by default)
        pattern: String,

        /// File or directory to search in (default: project root)
        path: Option<PathBuf>,

        /// Case-insensitive search
        #[arg(short = 'i', long)]
        ignore_case: bool,

        /// Match whole words only
        #[arg(short = 'w', long)]
        word: bool,

        /// Treat pattern as literal string (no regex)
        #[arg(short = 'F', long)]
        literal: bool,

        /// Show N lines of context around each match
        #[arg(short = 'C', long, default_value = "0")]
        context: u32,

        /// List only matching file paths
        #[arg(short = 'l', long)]
        files: bool,

        /// Filter by language (ts, js, rs, go, py, java, cs, rb, lua)
        #[arg(long, value_name = "LANG")]
        r#type: Option<String>,

        /// Max total matches (default: 200)
        #[arg(long)]
        max: Option<usize>,
    },
}

#[derive(Subcommand, Debug)]
pub enum FindCommand {
    /// Locate symbol definition
    Symbol {
        /// Symbol name to find
        name: String,

        /// Filter results to paths containing this substring (for disambiguation)
        #[arg(long, value_name = "SUBSTR")]
        path: Option<String>,

        /// Exclude noise paths (www/, dist/, `node_modules`/, .d.ts, .mdx)
        #[arg(long)]
        src_only: bool,

        /// Include full symbol body in results (like Serena's `include_body=True`)
        #[arg(long)]
        include_body: bool,
    },
    /// Find all references to a symbol
    Refs {
        /// Symbol name to search for
        name: String,

        /// Enrich each reference with its containing symbol (function/class name)
        #[arg(long)]
        with_symbol: bool,
    },
    /// Navigate from interface method to concrete implementations
    Impl {
        /// Symbol name (interface method) to find implementations of
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ListCommand {
    /// Semantic outline of a file
    Symbols {
        /// File path to list symbols for
        path: PathBuf,

        /// Depth of symbol tree (1=top-level, 2=methods, 3=full)
        #[arg(long, default_value = "1")]
        depth: u8,
    },
}

#[derive(Subcommand, Debug)]
pub enum ReadCommand {
    /// Read file contents with line numbers
    File {
        /// File path to read
        path: PathBuf,

        /// Start line (1-indexed)
        #[arg(long)]
        from: Option<u32>,

        /// End line (inclusive)
        #[arg(long)]
        to: Option<u32>,

        /// Max lines to show
        #[arg(long)]
        max_lines: Option<u32>,
    },
    /// Extract symbol body/code
    Symbol {
        /// Symbol name to read
        name: String,

        /// Show only the signature/declaration
        #[arg(long)]
        signature_only: bool,

        /// Max lines to show
        #[arg(long)]
        max_lines: Option<u32>,

        /// Select the definition whose path contains this substring (for disambiguation)
        #[arg(long, value_name = "SUBSTR")]
        path: Option<String>,

        /// Skip overload stubs (single-line `;` declarations) and return the implementation
        #[arg(long)]
        has_body: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum EditCommand {
    /// Replace symbol body with stdin
    Replace {
        /// Symbol to replace
        symbol: String,
    },
    /// Insert code after a symbol (stdin)
    InsertAfter {
        /// Symbol to insert after
        symbol: String,
    },
    /// Insert code before a symbol (stdin)
    InsertBefore {
        /// Symbol to insert before
        symbol: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServerCommand {
    /// List all supported LSP servers and their install status
    List,
    /// Install an LSP server (omit lang to install all missing)
    Install {
        /// Language (rust, go, python, java, cpp, csharp, ruby, lua, ts, js)
        lang: Option<String>,
        /// Force reinstall even if already present
        #[arg(long)]
        reinstall: bool,
    },
    /// Remove all managed servers from ~/.krait/servers/
    Clean,
    /// Show running LSP server status from daemon
    Status,
    /// Restart a language server in the running daemon
    Restart {
        /// Language to restart (rust, go, python, etc.)
        lang: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Start the daemon (foreground)
    Start,
    /// Stop the running daemon
    Stop,
    /// Show daemon status
    Status,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_help_does_not_panic() {
        Cli::command().debug_assert();
    }

    #[test]
    fn cli_parses_find_symbol() {
        let cli = Cli::try_parse_from(["krait", "find", "symbol", "MyStruct"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Find(FindCommand::Symbol { name, .. }) if name == "MyStruct"
        ));
    }

    #[test]
    fn cli_parses_read_file_with_flags() {
        let cli = Cli::try_parse_from([
            "krait", "read", "file", "src/lib.rs", "--from", "5", "--to", "10", "--max-lines",
            "20",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Read(ReadCommand::File {
                ref path,
                from: Some(5),
                to: Some(10),
                max_lines: Some(20),
            }) if path == &PathBuf::from("src/lib.rs")
        ));
    }

    #[test]
    fn cli_parses_edit_replace() {
        let cli = Cli::try_parse_from(["krait", "edit", "replace", "my_func"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Edit(EditCommand::Replace { symbol }) if symbol == "my_func"
        ));
    }

    #[test]
    fn cli_rejects_unknown_command() {
        let result = Cli::try_parse_from(["krait", "explode"]);
        assert!(result.is_err());
    }
}
