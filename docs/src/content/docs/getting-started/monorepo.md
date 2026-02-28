---
title: Monorepo Setup
description: Setting up krait for monorepos and multi-workspace projects.
---

## Auto-Detection

Krait automatically discovers all workspaces in a monorepo by walking the directory tree and finding manifest files (`package.json`, `go.mod`, `Cargo.toml`, `CMakeLists.txt`).

For most monorepos, just run:

```bash
krait init
```

## krait init

`krait init` scans your project, detects all workspaces, and generates `.krait/krait.toml`:

```
$ cd ~/projects/my-monorepo
$ krait init

Detected project root: /Users/me/projects/my-monorepo
Detected workspaces:
  typescript  packages/api
  typescript  packages/common
  typescript  packages/web
  go          backend

Written: krait.toml (4 workspaces)
Edit this file to customize which workspaces to index.
```

## Generated Config

```toml
[[workspace]]
path = "packages/api"
language = "typescript"

[[workspace]]
path = "packages/common"
language = "typescript"

[[workspace]]
path = "packages/web"
language = "typescript"

[[workspace]]
path = "backend"
language = "go"
```

## How It Works

Krait runs **one LSP process per language** and dynamically attaches workspace folders via `workspace/didChangeWorkspaceFolders`. This means:

- No restart needed when switching between packages
- Shared type information across packages of the same language
- Minimal memory footprint vs. one-server-per-workspace

## Tested Monorepos

Krait is tested against real-world monorepos:

| Project | Stack | Workspaces |
|---------|-------|:----------:|
| medusa | TypeScript | 76 |
| meet | TypeScript | 6 |
| WeKnora | Go + TypeScript | 3 |
