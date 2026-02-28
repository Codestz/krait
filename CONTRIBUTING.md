# Contributing to krait

Thank you for your interest in contributing! This document covers everything you need to get started.

---

## Table of Contents

- [Development Setup](#development-setup)
- [Project Structure](#project-structure)
- [Running Tests](#running-tests)
- [Code Style](#code-style)
- [Submitting Changes](#submitting-changes)
- [Architecture Overview](#architecture-overview)

---

## Development Setup

### Prerequisites

- Rust 1.85 or later (`rustup update stable`)
- A language server for testing (optional but recommended):
  - TypeScript: `npm install -g @vtsls/language-server`
  - Rust: `rustup component add rust-analyzer`
  - Go: `go install golang.org/x/tools/gopls@latest`

### Build

```bash
git clone https://github.com/Codestz/krait
cd krait
cargo build
```

### Release build

```bash
cargo build --release
./target/release/krait --help
```

---

## Project Structure

```
src/
  main.rs           Entry point, CLI dispatch, watch loop
  lib.rs            Library root, public API
  cli.rs            Clap command definitions
  client.rs         CLI-to-daemon client, command_to_request()
  protocol.rs       Wire protocol types (Request / Response)
  daemon/           UDS server, lifecycle, request dispatch
  lsp/              LSP client, transport, multiplexer, install
  index/            SQLite symbol index, cache, file watcher
  commands/         Business logic per command
  output/           Output formatters (compact, json, human)
  detect/           Language detection, project root detection
  lang/             Language-specific helpers
tests/
  fixtures/         Minimal Rust projects for e2e tests
  e2e_phase1.rs     CLI smoke tests (no LSP required)
  e2e_phase2.rs     LSP integration tests (marked #[ignore])
  e2e_phase3.rs     Discovery command tests (marked #[ignore])
internal-docs/
  VISION.md         Project goals and philosophy
  ARCHITECTURE.md   Technical architecture details
  ROADMAP.md        Future roadmap
```

---

## Running Tests

### Unit tests (always pass, no external dependencies)

```bash
cargo test
```

This runs 299+ unit tests and basic CLI smoke tests. No language servers or external projects required.

### LSP integration tests (optional)

The e2e tests in `e2e_phase2.rs` and `e2e_phase3.rs` use real LSP servers and are marked `#[ignore]` to keep CI fast. Run them locally with:

```bash
# Requires rust-analyzer
cargo test -- --ignored
```

These tests use fixture projects in `tests/fixtures/` (included in the repository) — they do not depend on any external codebases.

---

## Code Style

### Clippy

All clippy lints must pass. The project uses strict settings (`all = deny`, `pedantic = warn`):

```bash
cargo clippy
```

Fix any warnings before submitting a PR. Clippy is enforced in CI.

### Error handling

- Use `anyhow` for errors in binary/daemon code
- Use `thiserror` for library-level error types
- No `.unwrap()` in library code — always propagate with `?`

### Functions

- Keep functions under 50 lines; extract helpers if needed
- Write `#[cfg(test)] mod tests { ... }` alongside every non-trivial module

### Formatting

```bash
cargo fmt
```

Formatting is enforced in CI.

---

## Submitting Changes

### Small fixes and typos

Open a pull request directly.

### New features or significant changes

1. Open an issue first to discuss the approach
2. Reference the issue in your PR
3. Include tests for new behavior
4. Update relevant docs if the architecture changes

### Pull request checklist

- [ ] `cargo fmt` — no formatting diff
- [ ] `cargo clippy` — zero warnings
- [ ] `cargo test` — all tests pass
- [ ] New behavior has unit tests
- [ ] PR description explains the motivation

---

## Architecture Overview

Krait has three layers:

**CLI** (`src/main.rs`, `src/cli.rs`, `src/client.rs`)
Parses arguments, connects to the daemon over a Unix socket, prints results. Stateless.

**Daemon** (`src/daemon/`)
One process per project root. Manages LSP lifecycles, serves requests, maintains the symbol index. Started automatically on first use, shuts down after inactivity.

**LSP layer** (`src/lsp/`)
The `LspMultiplexer` runs one language server process per language and dynamically attaches workspace folders via `workspace/didChangeWorkspaceFolders`. Responses are buffered out-of-order and matched by request ID.

For deeper context, read `internal-docs/VISION.md` and `internal-docs/ARCHITECTURE.md`.

---

## Reporting Bugs

Use [GitHub Issues](https://github.com/Codestz/krait/issues). Include:

- krait version (`krait --version`)
- OS and architecture
- Minimal reproduction steps
- Output with `RUST_LOG=debug krait <command>` if the issue involves LSP communication

## Security

See [SECURITY.md](SECURITY.md) for the vulnerability reporting process.
