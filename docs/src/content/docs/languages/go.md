---
title: Go
description: Using krait with Go projects.
---

**Language server:** [gopls](https://pkg.go.dev/golang.org/x/tools/gopls)

## Setup

```bash
go install golang.org/x/tools/gopls@latest
```

On macOS without Go installed, krait can install gopls via Homebrew:

```bash
krait server install go    # uses brew if go is not in PATH
```

Note: installing the gopls binary alone is not sufficient — the Go toolchain must also be in PATH at runtime (see [Troubleshooting](#troubleshooting) below).

## Requirements

- `go.mod` at project root
- Go toolchain in PATH (`go` command available)
- gopls binary installed

## Zero Config

Go projects work out of the box. `go.mod` is automatically detected as the workspace marker.

```bash
cd my-go-project
krait list symbols internal/server/handler.go
```

## Supported Operations

- `find symbol` — resolves functions, types, interfaces, methods
- `hover` — Go godoc and type info
- `check` — Go compiler errors
- `edit replace` — replaces function/type bodies
- `rename` — gopls cross-file rename
- `find refs` — all usages

## Receiver Methods

For methods with receivers (e.g., `func (s *Server) Handle(...)`), use the dotted form:

```bash
krait read symbol Server.Handle
krait find symbol Server.Handle
```

## Performance

gopls is fast even on large projects. `find symbol` and `hover` typically respond in 30-60ms on warm daemon.

## Troubleshooting

### `warn  go: gopls is installed but requires go in PATH`

gopls is a standalone binary, but it calls the `go` command internally to load module graphs and type-check packages. Installing the gopls binary alone (e.g. via `brew install gopls`) is not enough.

**Fix:** Install the Go toolchain from [https://go.dev/dl/](https://go.dev/dl/), then re-index:

```bash
go version              # verify Go is available
krait daemon stop
krait init --force
```

### `indexed 0 files, 0 symbols` with no warning

gopls is likely not installed.

```bash
krait server list       # check gopls status
krait server install go # install gopls (requires go in PATH)
```

### `krait status` shows `go (gopls) — pending` indefinitely

gopls is still loading the workspace, or failed to start silently. Restart the daemon:

```bash
krait daemon stop
krait status            # daemon auto-restarts on next command
```

### First `krait init` is slow

gopls downloads and caches module dependencies on first use. This is a one-time cost. Large modules (e.g. with many external dependencies) can take 30–60 seconds the first time.
