# Krait - Code Intelligence CLI for AI Agents

## What This Is
Rust CLI + daemon providing LSP-backed code intelligence for AI coding agents.
Single binary, zero config, Unix composable. The ripgrep of code navigation.

## Current State
**v0.1 — complete.** Phases 1-12 done, all 305 tests passing, zero clippy warnings.

## Rules
- Run `cargo check` after every file edit
- Run `cargo test` after completing any task
- Use `anyhow` for error handling in binaries, `thiserror` for library code
- No `.unwrap()` in library code — always propagate errors
- Write tests alongside implementation (#[cfg(test)] mod tests)
- Keep functions under 50 lines — extract helpers

## CLI Grammar (Verb-Noun-Target)
```
krait init                              Set up .krait/ and pre-warm index
krait status                            Daemon health, LSP state, cache stats
krait check [path]                      LSP diagnostics (errors/warnings)
krait find symbol <name>                Locate symbol definition
krait find refs <name>                  Find all references to symbol
krait list symbols <path>               Semantic outline of file
krait read file <path>                  Read file with line numbers
krait read symbol <name>                Extract symbol body/code
krait edit replace <symbol>             Replace symbol body (stdin)
krait edit insert-after <symbol>        Insert code after symbol (stdin)
krait edit insert-before <symbol>       Insert code before symbol (stdin)
krait server list                       Show installed LSP servers
krait server install <lang>             Install/update a language server
krait server clean                      Remove managed servers
krait server status                     Show running LSP processes
krait search <pattern> [path]           Text search with regex, context, type filter
krait daemon start|stop|status          Daemon management (usually auto)
```

## Project Structure
```
src/
  main.rs         → Binary entry point
  lib.rs          → Library root
  cli.rs          → Clap command definitions
  client.rs       → CLI-to-daemon socket client
  protocol.rs     → Wire protocol types (Request/Response)
  daemon/         → UDS server, lifecycle, dispatch
  lsp/            → LSP client, transport, config, pool, install
  index/          → SQLite symbol index + cache
  commands/       → Business logic per command
  output/         → Formatting (compact, json, human)
  detect/         → Language + project root detection
  lang/           → Language-specific helpers (go, typescript)
```

## Testing
```bash
cargo test              # all tests
cargo test <module>     # specific module
```
Tests use fixture projects in tests/fixtures/.
