---
title: Server Commands
description: Manage LSP language servers.
---

Krait manages language server binaries automatically in `~/.krait/servers/`. These commands let you inspect and control them.

## server list

Show all configured language servers and their status.

```bash
krait server list
```

**Example:**
```
typescript  vtsls       /home/user/.krait/servers/vtsls  v0.2.35
go          gopls       /usr/local/bin/gopls             v0.16.2
rust        rust-analyzer  /home/user/.rustup/toolchains/...  1.75.0
```

## server install

Install or update a language server.

```bash
krait server install [lang]
```

Without `lang`, installs all servers for detected languages in the current project.

## server clean

Remove all managed language server binaries from `~/.krait/servers/`.

```bash
krait server clean
```

PATH-based servers (e.g., `gopls` installed globally) are unaffected.

## server status

Show which language servers are currently running inside the daemon.

```bash
krait server status
```

## Install Priority

Krait checks for servers in this order:

1. `PATH` — uses any globally installed server first
2. `~/.krait/servers/` — managed installs
3. Auto-download — if neither found, downloads automatically on first use
