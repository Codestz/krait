---
title: TypeScript
description: Using krait with TypeScript projects.
---

**Language server:** [vtsls](https://github.com/yioneko/vtsls) (preferred)

## Setup

Krait auto-installs vtsls on first use via npm. To pre-install:

```bash
npm install -g @vtsls/language-server
```

## Requirements

- `tsconfig.json` at the project root (or workspace root in a monorepo)
- Node.js in PATH

## Monorepos

Each sub-package with a `tsconfig.json` is detected as a separate workspace. Run `krait init` to generate the workspace config:

```bash
krait init
```

## Supported Operations

All krait operations work with TypeScript:

- `find symbol` — resolves types, interfaces, functions, classes
- `hover` — shows full type signatures and JSDoc
- `check` — TypeScript compiler errors and warnings
- `edit replace` — replaces symbol body with LSP-aware boundaries
- `rename` — cross-file rename with TypeScript compiler

## Troubleshooting

**Symbols not found:** Ensure `tsconfig.json` exists. Missing tsconfig causes degraded resolution.

**Slow first query:** vtsls needs 2-10s to load all types on first start. Subsequent queries are 20-60ms.

**Monorepo packages missing:** Run `krait init` to detect and register all packages.
