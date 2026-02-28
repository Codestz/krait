# Language Setup Guide

How to set up each language for optimal krait + LSP experience.

## Tier 1 — Zero Config

These languages work out of the box once the LSP server is installed.

### Rust
- **Server**: rust-analyzer
- **Install**: `rustup component add rust-analyzer` or download from GitHub releases
- **Config**: None required. Cargo.toml is auto-detected as workspace marker.

### Go
- **Server**: gopls
- **Install**: `go install golang.org/x/tools/gopls@latest`
- **Config**: None required. go.mod is auto-detected.

## Tier 2 — Needs Project Config

These languages benefit from a config file for accurate analysis.

### TypeScript / JavaScript
- **Server**: vtsls (preferred) or typescript-language-server
- **Install**: `npm install -g @vtsls/language-server` (krait auto-installs)
- **Config**: Ensure `tsconfig.json` exists at project root.
- **Gotchas**:
  - Monorepos: each package with a tsconfig.json is detected as a workspace
  - Missing tsconfig causes degraded symbol resolution

### C / C++
- **Server**: clangd
- **Install**: Ships with LLVM or `brew install llvm`
- **Config**: Generate `compile_commands.json`:
  - CMake: `cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON ..`
  - Bear: `bear -- make`
  - Meson: auto-generated in build dir
- **Gotchas**: Without compile_commands.json, clangd can't resolve includes

## Verifying Setup

```bash
# Check what krait detects
krait status

# Verify LSP server is found
krait server list

# Test symbol resolution
krait list symbols <any-source-file>
```

If `krait list symbols` times out, the LSP server likely hasn't finished initial analysis. Wait a few seconds and retry, or check that the project config file exists.
