---
title: Troubleshooting
description: Common issues and fixes when using krait.
---

## Index Issues

### `krait init` shows `indexed 0 files, 0 symbols`

**With a warning line:** the warning tells you exactly what's wrong. Common causes:

| Warning | Cause | Fix |
|---------|-------|-----|
| `gopls is installed but requires go in PATH` | Go toolchain missing | Install Go from [go.dev/dl](https://go.dev/dl/) |
| `failed to install gopls: Go is required` | gopls not installed, no Go in PATH | Install Go, then `krait server install go` |
| `no LSP server configured for <lang>` | Language not supported | Check [Language Support](/languages/) |

**With no warning:** the LSP server may not be installed.

```bash
krait server list       # see what's installed
krait server install <lang>
krait init --force
```

### `krait init` re-indexes everything every time

The index is content-addressed (BLAKE3 hashes). Files only re-index when their content changes. If everything re-indexes on each run, the `.krait/index.db` may have been deleted or is being regenerated.

Check that `.krait/` is not in your `.gitignore` or being cleaned by another tool.

### Symbols are stale after editing a file

The file watcher marks edited files dirty within ~500ms. If symbols appear stale:

```bash
krait status            # check "dirty files" count
krait init --force      # full re-index if watcher has fallen behind
```

---

## Daemon Issues

### `krait status` hangs or returns no output

The daemon may have crashed. Remove the stale socket and restart:

```bash
krait daemon stop       # cleans up PID + socket files
krait status            # daemon auto-restarts
```

### `krait daemon start` fails with "already running"

A stale PID file exists from a previous crash. Stop the daemon to clean it up:

```bash
krait daemon stop
krait daemon start
```

### Daemon shuts down unexpectedly

The daemon idles out after 30 minutes of inactivity by default. It auto-restarts on the next command — no manual action needed.

To keep it alive longer, set `KRAIT_IDLE_TIMEOUT` (in seconds) before starting:

```bash
KRAIT_IDLE_TIMEOUT=3600 krait daemon start   # 1 hour
```

---

## LSP Server Issues

### `krait server install <lang>` fails

| Error | Fix |
|-------|-----|
| `Go is required but not found in PATH` | Install Go from [go.dev/dl](https://go.dev/dl/) |
| `Node.js is required but not found in PATH` | Install Node.js from [nodejs.org](https://nodejs.org/) |
| `Homebrew is required but not found in PATH` | Install Homebrew or use the language's native install method |
| `npm install failed` | Check npm registry access; try `npm install` manually |

### `krait server list` shows a server as `not installed`

Run the install command shown in the output:

```bash
krait server install rust   # installs rust-analyzer
krait server install go     # installs gopls
```

### A language server keeps crashing

```bash
krait server status         # see running LSP processes
krait daemon stop           # full restart
krait status                # re-boot all servers
```

If crashes persist, the language server binary may be corrupted. Remove managed servers and reinstall:

```bash
krait server clean          # removes ~/.krait/servers/
krait server install <lang>
```

---

## macOS-Specific Issues

### `krait` binary crashes immediately (exit 137)

After installing or updating the krait binary, macOS may cache the old binary's security state. Overwriting a binary in-place with `cp` can trigger this.

**Fix:** Remove the old binary before copying:

```bash
rm /usr/local/bin/krait
cp ./target/release/krait /usr/local/bin/krait
```

### `spctl --assess` reports `rejected`

`spctl` checks against the App Store or Developer ID policy. Locally built binaries are always rejected by this check, but they still run fine. This is not an error.

---

## Command Errors

### `error: LSP servers still indexing`

The daemon just started and language servers are still loading. Wait a few seconds:

```bash
krait status    # check "pending" count
```

Retry the command once `krait status` shows no pending servers.

### `no results` from `krait find symbol`

- The symbol name may be misspelled or use wrong casing (symbol lookup is exact)
- The file may not be indexed yet — run `krait init`
- The language server may still be loading — check `krait status`

Try a text search as a fallback:

```bash
krait search MySymbol src/
```

### `krait check` returns no diagnostics on a file with errors

The language server may not have finished loading the file. Wait for `krait status` to show no pending servers, then retry. If the issue persists, the file may be outside the indexed workspace — verify with `krait status`.

---

## Getting More Information

Run any command with `RUST_LOG=krait=debug` to see detailed logs from the CLI:

```bash
RUST_LOG=krait=debug krait find symbol MyStruct
```

For daemon-level logs, run the daemon in the foreground:

```bash
krait daemon stop
RUST_LOG=krait=debug krait daemon start    # keep terminal open
```

Then run your command in a second terminal.
