---
title: Go
description: Using krait with Go projects.
---

**Language server:** [gopls](https://pkg.go.dev/golang.org/x/tools/gopls)

## Setup

```bash
go install golang.org/x/tools/gopls@latest
```

## Requirements

- `go.mod` at project root
- Go toolchain in PATH

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
