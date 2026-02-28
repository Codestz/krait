---
title: Daemon Commands
description: Manage the krait background daemon.
---

The daemon is a persistent background process that keeps LSP servers alive between queries. It starts automatically when you run any krait command.

## daemon start

Start the daemon in the foreground (usually not needed — it auto-starts).

```bash
krait daemon start
```

## daemon stop

Stop the running daemon for the current project.

```bash
krait daemon stop
```

## daemon status

Show daemon status.

```bash
krait daemon status
```

## Lifecycle

- **Auto-start:** The daemon starts automatically when you run any krait command in a project directory.
- **Per-project:** One daemon per project root. Multiple projects = multiple daemons.
- **Socket:** Communicates over a Unix domain socket at `.krait/daemon.sock`.
- **Shutdown:** The daemon shuts down automatically after a period of inactivity.
