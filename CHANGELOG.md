# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.1.0] - 2026-02-28

Initial release.

### Added

**Core infrastructure**
- Daemon architecture: one long-lived process per project root over a Unix domain socket
- Auto-start: daemon spawned on first use, shuts down after inactivity
- Auto-install language servers to `~/.krait/servers/` (vtsls, gopls, rust-analyzer)
- SQLite symbol index with BLAKE3 file hashing and file-watcher-based cache invalidation
- Dynamic multi-root LSP: one language server process per language, workspace folders attached dynamically via `workspace/didChangeWorkspaceFolders`
- Monorepo support: auto-detect all workspace roots recursively (tested up to 76 workspaces)
- Output formats: `compact` (LLM-optimized), `json`, `human`

**Navigation commands**
- `krait find symbol <name>` — locate symbol definition via LSP `workspace/symbol`
- `krait find refs <name>` — find all references via `textDocument/references`
- `krait list symbols <path>` — semantic file outline via `textDocument/documentSymbol`
- `krait read file <path>` — file contents with line numbers, binary detection
- `krait read symbol <name>` — extract symbol body, `--signature-only` flag

**Editing commands**
- `krait edit replace <symbol>` — replace symbol body from stdin
- `krait edit insert-after <symbol>` — insert code after a symbol from stdin
- `krait edit insert-before <symbol>` — insert code before a symbol from stdin
- Atomic file writes via temp-file-then-rename (no partial file corruption)

**Agent commands**
- `krait hover <symbol>` — type info and documentation via `textDocument/hover`
- `krait format <path>` — LSP formatter via `textDocument/formatting`
- `krait rename <symbol> <new-name>` — cross-file rename via `textDocument/rename`
- `krait fix [path]` — apply LSP quick fixes via `textDocument/codeAction`

**Diagnostics**
- `krait check [path]` — LSP diagnostics, `--errors-only` flag, exit code 1 on errors (CI-friendly)
- `krait status` — daemon health, LSP state, cache stats, index dirty file count

**Configuration**
- `krait init` — auto-detect workspaces and generate `.krait/krait.toml`
- `krait init --dry-run` — preview config without writing
- `krait daemon start|stop|status` — daemon lifecycle management

**Language support (v0.1)**
- TypeScript / JavaScript via vtsls
- Go via gopls
- Rust via rust-analyzer
- C/C++ via clangd

**Performance** (benchmarked on open-source projects, warm daemon)
- Warm `find symbol`: 32–59ms across TypeScript and Go projects
- Warm `hover`: 40–70ms (excluding one-time LSP type-load on first call)
- Warm `check`: 37–50ms
- Cold start (daemon off, index cached): 146–586ms depending on workspace size

[0.1.0]: https://github.com/Codestz/krait/releases/tag/v0.1.0
