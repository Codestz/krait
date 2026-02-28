---
title: Contributing
description: How to contribute to krait.
---

## Development Setup

### Prerequisites

- Rust 1.75+ (`rustup update stable`)
- A language server for testing (optional):
  - TypeScript: `npm install -g @vtsls/language-server`
  - Rust: `rustup component add rust-analyzer`
  - Go: `go install golang.org/x/tools/gopls@latest`

### Build

```bash
git clone https://github.com/Codestz/krait
cd krait
cargo build
```

## Running Tests

### Unit tests (no external dependencies)

```bash
cargo test
```

Runs 299+ unit tests and basic CLI smoke tests.

### LSP integration tests (optional)

```bash
# Requires rust-analyzer
cargo test -- --ignored
```

These use fixture projects in `tests/fixtures/`.

## Code Style

### Clippy

```bash
cargo clippy    # must pass with zero warnings
```

### Error handling

- `anyhow` for errors in binary/daemon code
- `thiserror` for library-level error types
- No `.unwrap()` in library code

### Formatting

```bash
cargo fmt
```

## Submitting Changes

### Small fixes

Open a pull request directly.

### New features

1. Open an issue first to discuss the approach
2. Reference the issue in your PR
3. Include tests for new behavior

### PR checklist

- [ ] `cargo fmt` — no formatting diff
- [ ] `cargo clippy` — zero warnings
- [ ] `cargo test` — all tests pass
- [ ] New behavior has unit tests
- [ ] PR description explains the motivation

## Architecture Overview

Krait has three layers:

**CLI** (`src/main.rs`, `src/cli.rs`, `src/client.rs`)
Parses arguments, connects to daemon over Unix socket, prints results. Stateless.

**Daemon** (`src/daemon/`)
One process per project root. Manages LSP lifecycles, serves requests, maintains the symbol index.

**LSP layer** (`src/lsp/`)
`LspMultiplexer` runs one server per language, dynamically attaches workspace folders.

## Reporting Bugs

Use [GitHub Issues](https://github.com/Codestz/krait/issues). Include:

- krait version (`krait --version`)
- OS and architecture
- Minimal reproduction steps
- Output with `RUST_LOG=debug krait <command>` for LSP issues
