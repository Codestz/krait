---
title: Diagnostics Commands
description: Commands for checking code health with LSP diagnostics.
---

## check

Run LSP diagnostics on a file or the whole project.

```bash
krait check [path]
krait check [path] --errors-only    # suppress warnings and hints
```

**Example:**
```
$ krait check src/orders/service.ts
error src/orders/service.ts:45:12 TS2339 Property 'id' does not exist on type 'never'
warning src/orders/service.ts:67:5 TS6133 'result' is declared but never read
2 errors, 1 warning
```

**Exit codes:**
- `0` — no errors (warnings are OK)
- `1` — one or more errors

This makes `krait check` usable in CI pipelines:

```bash
krait check src/ --errors-only && echo "Clean"
```

## status

Show daemon health, LSP state, and cache stats.

```bash
krait status
```

**Example:**
```
daemon: pid=12345 uptime=5m
config: krait.toml (5 workspaces)
lsp: 2 sessions
  typescript  packages/api  ready
  go          backend       ready
index: 8928 symbols, 0 dirty files, watcher active
```
