---
title: Rust
description: Using krait with Rust projects.
---

**Language server:** [rust-analyzer](https://rust-analyzer.github.io/)

## Setup

rust-analyzer ships with rustup:

```bash
rustup component add rust-analyzer
```

Or download from [GitHub releases](https://github.com/rust-lang/rust-analyzer/releases).

## Requirements

- `Cargo.toml` at project root
- Rust toolchain installed

## Zero Config

Cargo.toml is auto-detected. For workspace crates:

```bash
cd my-rust-workspace
krait init    # detects all member crates
```

## Supported Operations

- `find symbol` — functions, structs, enums, traits, impls
- `hover` — Rust types, trait bounds, doc comments
- `check` — `cargo check` errors surfaced via LSP
- `edit replace` — replaces function/impl bodies
- `rename` — cross-file rename
- `find refs` — all usages

## Trait Methods

Use dotted notation for trait/impl methods:

```bash
krait read symbol MyStruct.my_method
krait hover MyStruct.my_method
```

## Performance

rust-analyzer may take 10-30s to fully load a large workspace on first start. Subsequent warm queries are 30-80ms.
