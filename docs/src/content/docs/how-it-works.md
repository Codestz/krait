---
title: How It Works
description: Architecture overview — daemon, LSP multiplexer, and index.
---

## Architecture

```
+---------------------------------------------+
|  AI Agent                                   |
|  krait find symbol Foo                      |
+--------------------+------------------------+
                     | Unix socket
+--------------------v------------------------+
|  krait daemon (per project)                 |
|  +--------------+  +----------------------+ |
|  | SQLite index |  | LSP Multiplexer      | |
|  | + file watch |  | one server/language  | |
|  +--------------+  +----------+-----------+ |
+-------------------------------|--------------+
                                | stdin/stdout
            +-------------------+-----------+
            |                   |           |
     +------v------+   +-------v------+  +-+----------+
     |    vtsls    |   |    gopls     |  |rust-analyzer|
     | (TypeScript)|   |    (Go)      |  |   (Rust)   |
     +-------------+   +--------------+  +------------+
```

## Components

### 1. CLI (thin proxy)

`krait` is a thin binary that:
- Parses command-line arguments
- Connects to the daemon over a Unix domain socket at `.krait/daemon.sock`
- Serializes the request and prints the response
- Stateless — no persistent state in the CLI

### 2. Daemon (one per project)

The daemon process:
- Listens on a Unix socket
- Manages LSP server lifecycles (start, stop, restart on crash)
- Maintains the SQLite symbol index
- Runs the file watcher for cache invalidation
- Shuts down after inactivity

One daemon per project root. Identified by the project root path.

### 3. LSP Multiplexer

The `LspMultiplexer` runs **one language server per language** and dynamically attaches workspace folders via `workspace/didChangeWorkspaceFolders`. This means:

- No restart needed when accessing a new package in a monorepo
- Shared type information across packages
- One `vtsls` process handles all TypeScript workspaces

Server identity is `(server_name, primary_workspace_root)`.

### 4. SQLite Index

The symbol index caches results from LSP queries:
- Built by `krait init` (parallel workspace indexing)
- Stored at `.krait/index.db`
- Queries check the cache first (O(1)), fall through to LSP on miss
- Invalidated by the file watcher (500ms debounce)

### 5. File Watcher

A `notify`-based file watcher tracks changes in the project:
- Marks files as dirty in a `DirtyFiles` set
- Dirty files bypass the cache and hit the LSP directly
- On watcher overflow, falls back to BLAKE3 hash comparison

### 6. Auto-Install

Language servers are managed automatically in `~/.krait/servers/`:
- `vtsls` — installed via npm
- `gopls` — installed via `go install`
- `rust-analyzer` — installed via rustup
- PATH takes priority over managed installs

## Query Path

For a `find symbol` query:

1. CLI sends request over Unix socket
2. Daemon checks SQLite cache (cache-first path)
3. If cache hit (and file not dirty): return immediately
4. If cache miss: forward to LSP Multiplexer
5. LSP Multiplexer routes to the correct language server
6. Response cached in SQLite, returned to CLI
