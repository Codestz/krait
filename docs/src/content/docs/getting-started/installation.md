---
title: Installation
description: How to install krait on your system.
---

## Homebrew (macOS / Linux)

```bash
brew tap Codestz/krait
brew install krait
```

## From Source (Rust 1.85+)

```bash
cargo install krait-cli
```

## Pre-built Binaries

Download from [Releases](https://github.com/Codestz/krait/releases).

## Language Servers

Krait auto-installs language servers on first use. To pre-install:

```bash
# TypeScript / JavaScript (vtsls — recommended)
npm install -g @vtsls/language-server

# Go
go install golang.org/x/tools/gopls@latest

# Rust (comes with rustup)
rustup component add rust-analyzer
```

C/C++ uses `clangd`, which ships with LLVM — install it via your system package manager.

## Verifying the Install

```bash
krait --version
krait status
```

If krait starts the daemon and prints status, you're ready.
