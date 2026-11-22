<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
  <img src="assets/logo-light.svg" width="96" height="96" alt="Kensaku Headless logo">
</picture>

# Kensaku Headless

[![CI](https://img.shields.io/github/actions/workflow/status/ibarra000/kensaku-headless/ci.yml?branch=main)](https://github.com/ibarra000/kensaku-headless/actions/workflows/ci.yml)
[![MIT license](https://img.shields.io/badge/license-MIT-111111)](LICENSE)

Local search over file names, file contents, text in images (OCR), and meaning,
packaged as one `kensaku` binary: a CLI, a background daemon, a loopback HTTP
API, an MCP server, and a Rust crate. No GUI.

> **Credit.** Kensaku Headless is based on [Kensaku](https://github.com/yjid/kensaku)
> by [Yash Jangid (@yjid)](https://github.com/yjid), MIT-licensed. The indexing,
> search, OCR and agent engine are theirs; this fork removes the desktop app and
> exposes everything through the CLI, daemon, HTTP and Rust APIs. If you want
> the desktop app, use the original project.
> The logo is a wireframe adaptation of Kensaku's mark.

## Install

Download the archive for your platform from the
[latest release](https://github.com/ibarra000/kensaku-headless/releases/latest)
(Linux x86_64, macOS arm64/x86_64, Windows x86_64/ARM64) and check it against
`SHA256SUMS`. Binaries are not code-signed yet: Windows SmartScreen may warn,
and on macOS run `xattr -d com.apple.quarantine ./kensaku` if Gatekeeper blocks it.

From source (Rust 1.98):

```bash
git clone https://github.com/ibarra000/kensaku-headless
cd kensaku-headless
cargo build --release -p kensaku
```

Details: [docs/INSTALL.md](docs/INSTALL.md).

## Quick start

```bash
kensaku setup --root ~/Documents --root ~/Projects --wait
kensaku search "docker error"
kensaku search "invoice" --ext pdf --since 30d --json
kensaku status
```

The first command starts the daemon if it isn't running.

## Interfaces

- **CLI**: `--json` on every command, stable exit codes, `kensaku call <method> '<json>'` for any API method, `kensaku methods` to list them.
- **Daemon**: single writer for the index, auto-spawned per data directory; `kensaku daemon install` starts it at login.
- **HTTP**: `kensaku serve` listens on loopback with a bearer token; `/v1/<method>` and `/v1/events` (SSE). Writes need `--allow-writes`.
- **MCP**: `kensaku mcp` over stdio; `kensaku connect <claude-desktop|claude-code|cursor> --write` registers it. Read-only by default.
- **Rust**: `kensaku-core` exposes `Engine` (in-process) and `Client` (daemon) behind one `Kensaku` trait; see `crates/kensaku-core/examples/embed.rs`.

Method reference, HTTP rules and error codes: [docs/API.md](docs/API.md). Agent setup: [docs/AGENTS.md](docs/AGENTS.md).

## Query language

```
type:pdf invoice          ext:md in:"~/Notes"       modified:7d
size:>10mb                path:"client/2026"        lang:rust
name:resume               content:"timeout"         re:"fn \w+_test"
-draft                    mode:exhaustive
```

Reference: [docs/USAGE.md](docs/USAGE.md). Formats and languages: [docs/LANGUAGES.md](docs/LANGUAGES.md).

## More

- Docs: [docs/](docs/README.md)
- Contributing: [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md)
- Security: [SECURITY.md](SECURITY.md)
- License: [MIT](LICENSE)
