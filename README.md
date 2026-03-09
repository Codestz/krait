# krait

**Code intelligence CLI for AI agents** — LSP-backed symbol search, semantic editing, diagnostics, and hover in a single Rust binary.

[![CI](https://github.com/Codestz/krait/actions/workflows/ci.yml/badge.svg)](https://github.com/Codestz/krait/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/krait-cli.svg)](https://crates.io/crates/krait-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> **v0.1 — Initial release.** Core language support: TypeScript, JavaScript, Go, Rust, and C++.
> More languages are coming in future releases.

---

## What is krait?

Krait is a headless IDE backend for AI coding agents. Instead of asking an agent to guess line numbers or read entire files, krait gives it precise, semantic operations backed by the real Language Server Protocol:

```bash
krait find symbol PaymentService          # where is it defined?
krait hover PaymentService                # what is its type signature?
krait read symbol PaymentService          # extract the full body
echo '...' | krait edit replace processPayment  # replace by name, not line
krait check src/payments.ts              # LSP diagnostics
krait fix src/payments.ts                # auto-apply quick fixes
krait format src/payments.ts             # LSP formatter
```

No line numbers. No file parsing. The LSP is the source of truth.

---

## Why krait?

Most code intelligence tools for AI agents fall into one of two categories:

**MCP servers and file tools** expose raw file I/O: read this file, write that file, grep for this string. The agent sees text — no types, no structure. Edits target line numbers that shift the moment any other edit runs. On large codebases, tools that read entire files consume thousands of tokens per operation.

**Full IDE integrations** (language plugins, editor extensions) are tightly coupled to a specific editor's extension API. They're powerful but not composable — you can't pipe their output, use them in CI, or wire them to an agent running outside that editor.

Krait occupies the space in between: a headless LSP client you can use from any agent, any script, any terminal.

- **Semantic over textual** — `krait edit replace MyStruct` finds the symbol by name and replaces its body. The LSP, not a regex, defines its boundaries. No line number drift, no file corruption.
- **Token-efficient by default** — the compact output format is designed for LLM context windows. `krait list symbols` gives a file's structure in a few lines rather than thousands.
- **Unix composable** — every command reads from stdin or writes to stdout. `generate-code | krait edit replace Handler` just works.
- **Warm queries in tens of milliseconds** — a persistent daemon keeps LSP servers alive between calls. No cold start on every query.
- **Zero config for most projects** — drop it in, run a command. `krait init` handles the rest for monorepos.

---

## Features

- **Single binary** — one `krait` executable, no runtime dependencies
- **Daemon architecture** — persistent LSP processes, warm queries in ~20ms
- **Auto-installs language servers** — vtsls, gopls, rust-analyzer managed automatically
- **Monorepo-native** — tested on 76-workspace TypeScript monorepos
- **Semantic editing** — edit by symbol name, not line number (prevents file corruption)
- **Agent-first output** — compact, token-efficient format optimized for LLM context windows
- **Multi-language** — TypeScript, JavaScript, Go, Rust, C++ (v0.1); more coming

---

## Installation

### Homebrew (macOS / Linux)

```bash
brew tap Codestz/tap
brew install krait
```

### From source (requires Rust 1.85+)

```bash
cargo install krait-cli
```

### Pre-built binaries

Download from [Releases](https://github.com/Codestz/krait/releases).

### Language servers

Krait auto-installs language servers on first use. To pre-install:

```bash
# TypeScript / JavaScript (vtsls — recommended)
npm install -g @vtsls/language-server

# Go
go install golang.org/x/tools/gopls@latest

# Rust (comes with rustup)
rustup component add rust-analyzer
```

---

## Quick Start

```bash
cd your-project

# Optional: generate config for monorepos
krait init

# Navigate code
krait find symbol MyStruct
krait list symbols src/lib.rs
krait read symbol MyStruct

# Understand APIs
krait hover MyStruct

# Edit semantically
cat new_impl.rs | krait edit replace MyStruct
krait rename OldName NewName

# Verify
krait check
krait fix
krait format src/lib.rs
```

---

## Command Reference

### Navigation

```bash
krait find symbol <name>          # locate symbol definition
krait find refs <name>            # find all references
krait list symbols <path>         # semantic outline of a file
krait list symbols <path> --depth 2   # include methods/fields
krait read file <path>            # file contents with line numbers
krait read file <path> --from 10 --to 50
krait read symbol <name>          # extract symbol body
krait read symbol <name> --signature-only
```

### Understanding

```bash
krait hover <symbol>              # type info + documentation
krait check [path]                # LSP diagnostics (errors + warnings)
krait check [path] --errors-only  # suppress warnings
krait status                      # daemon health, LSP state, cache stats
```

### Editing

```bash
# All edit commands read new code from stdin
cat new_body.rs | krait edit replace <symbol>
echo 'fn helper() {}' | krait edit insert-after <symbol>
echo 'fn helper() {}' | krait edit insert-before <symbol>

krait rename <symbol> <new-name>  # cross-file LSP rename
krait fix [path]                  # apply LSP quick fixes
krait format <path>               # run LSP formatter
```

### Server management

```bash
krait server list                 # show configured language servers
krait server install [lang]       # install/update a language server
krait server clean                # remove managed server binaries
krait server status               # show running LSP processes
```

### Daemon

```bash
krait daemon start     # start in foreground (usually auto-started)
krait daemon stop      # stop the daemon
krait daemon status    # show daemon status
```

### Output formats

```bash
krait find symbol Foo             # compact (default, LLM-optimized)
krait find symbol Foo --format json   # structured JSON
krait find symbol Foo --format human  # human-readable
```

---

## Output Format

The default `compact` format minimizes tokens for LLM consumption:

```
# krait find symbol createOrder
fn createOrder  src/orders/service.ts:42

# krait list symbols src/orders/service.ts
fn createOrder [42]
fn cancelOrder [67]
class OrderService [12]
  fn constructor [13]
  fn validateItems [28]

# krait check src/orders/service.ts
error src/orders/service.ts:45:12 TS2339 Property 'id' does not exist on type 'never'
warning src/orders/service.ts:67:5 TS6133 'result' is declared but never read
2 errors, 1 warning

# krait hover OrderService
class OrderService extends BaseService<Order>
Manages the full order lifecycle including payment, fulfillment, and cancellation.
src/orders/service.ts:12
```

---

## How It Works

```
┌─────────────────────────────────────────────┐
│  AI Agent                                   │
│  krait find symbol Foo                      │
└────────────────────┬────────────────────────┘
                     │ Unix socket
┌────────────────────▼────────────────────────┐
│  krait daemon (per project)                 │
│  ┌──────────────┐  ┌──────────────────────┐ │
│  │ SQLite index │  │ LSP Multiplexer      │ │
│  │ + file watch │  │ one server/language  │ │
│  └──────────────┘  └──────────┬───────────┘ │
└───────────────────────────────│─────────────┘
                                │ stdin/stdout
            ┌───────────────────┼───────────────┐
            │                   │               │
     ┌──────▼──────┐   ┌───────▼──────┐  ┌─────▼──────┐
     │    vtsls    │   │    gopls     │  │rust-analyzer│
     │ (TypeScript)│   │    (Go)      │  │   (Rust)   │
     └─────────────┘   └─────────────┘  └────────────┘
```

1. **CLI** — thin proxy, sends requests over a Unix domain socket
2. **Daemon** — one process per project root, manages LSP lifecycles
3. **LSP Multiplexer** — one language server per language, dynamically attaches workspace folders (no restart needed for monorepos)
4. **SQLite index** — warm query cache, invalidated by file watcher
5. **Auto-install** — manages language server binaries in `~/.krait/servers/`

---

## Performance

Benchmarked on three open-source projects using real codebases.

### Indexing (`krait init`)

| Project | Stack | Workspaces | Files | Symbols | Time |
|---------|-------|:----------:|------:|--------:|-----:|
| [medusa](https://github.com/medusajs/medusa) | TypeScript | 76 | 8,928 | 260,579 | **23.4s** |
| [meet](https://github.com/numerique-gouv/meet) | TypeScript | 6 | 373 | 8,432 | **3.5s** |
| [WeKnora](https://github.com/Tencent/WeKnora) | Go + TypeScript | 3 | 318 | 7,442 | **9.2s** |

Re-indexing after files are cached: **8,928 files validated in 1.1s** (BLAKE3 hash check, no LSP round-trips for unchanged files).

### Warm query latency

**Warm** = daemon running, LSP fully initialized.

| Operation | medusa (TS, 76 workspaces) | meet (TS, 3 pkgs) | WeKnora (Go) |
|-----------|:--------------------------:|:-----------------:|:------------:|
| `find symbol` | ~59ms | ~40ms | ~32ms |
| `list symbols` | ~40ms | ~33ms | ~44ms |
| `hover` | ~41ms | ~46ms | ~61ms |
| `check` | ~37ms | ~40ms | ~50ms |
| `find refs` | ~1.2s | ~80ms | ~260ms |

### Cold start (first query, daemon off)

| Project | Cold start |
|---------|:----------:|
| [medusa](https://github.com/medusajs/medusa) — TypeScript, 76 workspaces | ~173ms |
| [meet](https://github.com/numerique-gouv/meet) — TypeScript, 3 packages | ~586ms |
| [WeKnora](https://github.com/Tencent/WeKnora) — Go | ~146ms |

> `find refs` is proportional to workspace size — it scans all files for references. The first `hover` after daemon start has a one-time warmup cost (1–7s) while the LSP loads all types into memory; subsequent calls are 40–70ms.

---

## Configuration

Krait works with zero config. For monorepos, run `krait init` to generate `.krait/krait.toml`:

```bash
krait init        # auto-detect workspaces
krait init --dry-run  # preview without writing
```

Generated config:

```toml
[[workspaces]]
language = "typescript"
root = "packages/core"

[[workspaces]]
language = "typescript"
root = "packages/api"

[[workspaces]]
language = "go"
root = "backend"
```

Krait auto-detects project root by walking up from the current directory looking for `Cargo.toml`, `package.json`, `go.mod`, `CMakeLists.txt`, etc.

---

## Language Support

| Language | Server | Auto-install |
|----------|--------|-------------|
| TypeScript / JavaScript | vtsls | npm |
| Go | gopls | go install |
| Rust | rust-analyzer | rustup |
| C / C++ | clangd | system package |

> v0.1 ships with these 5 languages. Additional language support is planned for future releases.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

## License

MIT — see [LICENSE](LICENSE).
