# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |

## Reporting a Vulnerability

Please do **not** open a public GitHub issue for security vulnerabilities.

Instead, report them via [GitHub Security Advisories](https://github.com/Codestz/krait/security/advisories/new) or by emailing the maintainers directly.

Include:
- A description of the vulnerability
- Steps to reproduce
- Potential impact
- Any suggested fixes

You can expect an acknowledgment within 48 hours and a fix or mitigation plan within 14 days.

## Security Model

**Daemon socket**
The daemon communicates over a Unix domain socket at `~/.cache/krait/<project-hash>/daemon.sock`. The socket is only accessible to the current user (mode 0600). No network ports are opened.

**Language server processes**
Krait spawns language server processes (vtsls, gopls, rust-analyzer, etc.) as child processes communicating over stdin/stdout. These processes run with the same permissions as the current user. Krait does not sandbox them.

**Auto-installed binaries**
Language servers are installed to `~/.krait/servers/`. Krait downloads official releases from npm registries and the Go module proxy. Checksums are not currently verified — this is a known limitation targeted for improvement in a future release.

**No network access from the daemon**
The krait daemon itself makes no outbound network connections. Only the installer (`krait server install`) fetches packages.

**File writes**
Edit commands (`krait edit`, `krait format`, `krait fix`, `krait rename`) write to files in the current project only, using atomic temp-file-then-rename to prevent partial writes.

**Diagnostic output**
`krait check` output may include file paths and code snippets from the project. These are not logged or transmitted anywhere — they are printed to stdout only.
