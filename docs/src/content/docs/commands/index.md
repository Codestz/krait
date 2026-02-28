---
title: Command Overview
description: Complete reference for all krait commands.
---

Krait follows a **Verb-Noun-Target** grammar:

```
krait <verb> <noun> <target> [flags]
```

## Command Categories

| Category | Commands |
|----------|----------|
| **Navigation** | `find symbol`, `find refs`, `list symbols`, `read file`, `read symbol` |
| **Understanding** | `hover`, `check`, `status` |
| **Editing** | `edit replace`, `edit insert-after`, `edit insert-before`, `rename`, `fix`, `format` |
| **Server** | `server list`, `server install`, `server clean`, `server status` |
| **Daemon** | `daemon start`, `daemon stop`, `daemon status` |
| **Project** | `init`, `status` |

## Output Format

All commands support `--format`:

```bash
krait find symbol Foo              # compact (default, LLM-optimized)
krait find symbol Foo --format json    # structured JSON
krait find symbol Foo --format human   # human-readable
```

## Global Flags

| Flag | Description |
|------|-------------|
| `--format <compact\|json\|human>` | Output format |
| `--version` | Print version |
| `--help` | Print help |
