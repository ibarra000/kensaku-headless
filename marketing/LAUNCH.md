# Kensaku Headless launch kit (2.0.0)

Kensaku Headless is a fork of [Kensaku](https://github.com/yjid/kensaku) by
Yash Jangid (@yjid). Credit the original project in every post: the indexing,
search, OCR and agent engine come from it, and the fork's contribution is the
headless packaging (one binary, daemon, HTTP and Rust APIs, no GUI).

Tone rules (unchanged from the original kit): concrete over adjectives, honest
limits included, every post ends with a question or an ask for feedback. Never
crosspost the same day to overlapping audiences.

Audience: developers who want local file and content search **inside their own
tools**. That means scripts, editors, self-hosted dashboards, AI agents, and
Rust apps. It is not for people looking for a desktop launcher; send them to the
original Kensaku.

## The 2-line pitch (use verbatim)

> Kensaku Headless: local search over file names, document text, OCR'd images and
> meaning, as one binary you can script, call over HTTP, embed in Rust, or plug
> into AI agents via MCP. A headless fork of Kensaku; nothing leaves your machine.

## What to show

Record a terminal session (asciinema or a short GIF). Don't use screenshots of the
old desktop app.

```bash
kensaku setup --root ~/Documents --wait
kensaku search "invoice from march"
kensaku search "type:pdf database indexing" --json | jq '.results[0]'
kensaku serve &            # loopback HTTP: /v1/search, /v1/events
kensaku connect claude-code --write
```

## Show HN

**Title:** `Show HN: Kensaku Headless – local file and content search as a CLI, daemon and API`

**Body (~120 words):**

> Kensaku is a local search tool by @yjid that indexes file names, text inside
> PDFs/Office files/code, text in screenshots (on-device OCR), and meaning (static
> embeddings, no GPU). I wanted to use it from other programs, so I made a headless
> fork: the GUI is gone, and one `kensaku` binary is the CLI, a background daemon
> that owns the index, an MCP server, and a loopback HTTP API. Every surface shares
> one method table, so `kensaku call`, `POST /v1/<method>` and the Rust `Engine`
> behave identically. Writes over HTTP are off by default; file reads are limited to
> indexed, in-scope paths. MIT, Rust, Windows/macOS/Linux.
> https://github.com/ibarra000/kensaku-headless

**First comment:** link [docs/API.md](../docs/API.md) and
[docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md), and explain the daemon model:
single writer lock, auto-spawn, per-data-dir endpoint, token auth.

## Reddit

| Subreddit | Angle |
|---|---|
| r/rust | One method table behind four transports; `Engine` vs `Client`; removing Tauri from a library crate. |
| r/selfhosted | Loopback HTTP API and SSE status stream for dashboards; no cloud, no telemetry. |
| r/LocalLLaMA, r/ClaudeAI | MCP server for local files: read-only by default, scoped reads, no bundled LLM. |
| r/commandline | `--json` everywhere, stable exit codes, shell completions. |

Title template: `Kensaku Headless: search names, contents and screenshots of your local files from the CLI / HTTP / MCP (headless fork of Kensaku)`.

## Short posts (X, Mastodon, Bluesky)

> Made a headless fork of @yjid's Kensaku: local file + content + OCR search as one
> binary. CLI with --json, background daemon, HTTP API, MCP for agents, Rust
> embedding. MIT. github.com/ibarra000/kensaku-headless
>
> #rust #opensource #cli #mcp #selfhosted

## Before posting

- [ ] A tagged release exists with archives for all five targets and `SHA256SUMS`.
- [ ] Windows `kensaku.exe` is signed (see [docs/SIGNING.md](../docs/SIGNING.md)).
- [ ] CI is green on `main`.
- [ ] A new social preview image (terminal-based, credits Kensaku) is uploaded
      in repo Settings → General → Social preview (GitHub has no API for this).
- [ ] README quick start has been run start to finish on a clean machine.
- [ ] Discussions are enabled for questions and roadmap votes.
