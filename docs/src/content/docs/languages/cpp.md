---
title: C / C++
description: Using krait with C and C++ projects.
---

**Language server:** [clangd](https://clangd.llvm.org/)

## Setup

clangd ships with LLVM:

```bash
# macOS
brew install llvm

# Ubuntu/Debian
apt install clangd

# or download from https://github.com/clangd/clangd/releases
```

## Requirements

- `compile_commands.json` in the project root or build directory
- clangd in PATH

## Generate compile_commands.json

clangd needs compile commands to resolve includes:

```bash
# CMake
cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON -B build
ln -s build/compile_commands.json .

# Bear (for Makefiles)
bear -- make

# Meson
# Generated automatically in the build directory
```

**Without compile_commands.json**, clangd can't resolve `#include` directives and symbol resolution will be degraded.

## Supported Operations

- `find symbol` — functions, classes, structs, enums
- `hover` — types and declarations
- `check` — clangd diagnostics
- `edit replace` — function/class bodies
- `rename` — cross-file rename
- `list symbols` — file outline

## CMakeLists.txt Detection

Krait detects `CMakeLists.txt` as the project root marker for C/C++ projects.
