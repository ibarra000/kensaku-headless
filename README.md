<p align="center">
  <img src="assets/icon.png" width="112" height="112" alt="Kensaku" />
</p>

<h1 align="center">Kensaku Headless <span title="kensaku = search, in Japanese">検索</span></h1>

<p align="center">
  <b>Fast, light, and efficient. Find anything on your computer.</b>
</p>

<p align="center">
  Search 100,000 file names in milliseconds, with about 12 MB of metadata.<br/>
  Search inside documents, images, and code too. Nothing leaves your machine.
</p>

<p align="center">
  <a href="https://github.com/ibarra000/kensaku-headless/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/ibarra000/kensaku-headless?style=for-the-badge" /></a>
  <a href="https://github.com/ibarra000/kensaku-headless/releases/latest"><img alt="Downloads" src="https://img.shields.io/github/downloads/ibarra000/kensaku-headless/total?style=for-the-badge" /></a>
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-111111?style=for-the-badge" /></a>
  <a href="https://github.com/ibarra000/kensaku-headless/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/ibarra000/kensaku-headless/ci.yml?branch=main&style=for-the-badge" /></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#what-kensaku-does-that-the-others-dont">Why Kensaku</a> ·
  <a href="#features">Features</a> ·
  <a href="#performance">Performance</a> ·
  <a href="#connect-ai-agents-mcp">MCP</a> ·
  <a href="docs/README.md">Docs</a> ·
  <a href="docs/LANGUAGES.md">Languages</a> ·
  <a href="#contributing">Contribute</a>
</p>

---

Kensaku Headless is a headless local search tool. A background daemon owns the
index, and the `kensaku` command, the MCP server, the loopback HTTP API, and
Rust applications (through `kensaku-core`) all talk to it. There is no GUI:
it is built to be embedded in other applications and scripts.

> **Credit.** Kensaku Headless is based on [Kensaku](https://github.com/yjid/kensaku)
> by [Yash Jangid (@yjid)](https://github.com/yjid), MIT-licensed. The indexing,
> search, OCR and agent engine are theirs; this fork removes the desktop app and
> exposes everything through the CLI, daemon, HTTP and Rust APIs. If you want
> the desktop app, use the original project.

## Find it the way you remember it

You don't remember the filename. You remember what was in it.

- The PDF about database indexing, saved as `notes-final-2.pdf`.
- The log line from the failed deploy, somewhere in a folder of logs.
- The screenshot with a CUDA error, which has no useful name at all.

Type what you remember. Kensaku searches names, text inside files, and meaning,
and prints the matches with a snippet from each.

## What Kensaku does that the others don't

Filename tools are everywhere. Reading *inside* your files — locally, instantly, and for free — is not.

| | Kensaku | Everything | Listary | PowerToys Run |
|---|---|---|---|---|
| Filename search, milliseconds | ✅ | ✅ | ✅ | ✅ |
| Full text inside PDF/DOCX/EPUB… | ✅ | ❌ | ❌ | ❌ |
| Text inside screenshots & images (OCR) | ✅ | ❌ | ❌ | ❌ |
| Multilingual & CJK text (auto-tokenized) | ✅ | ❌ | ❌ | ❌ |
| Find by meaning ("the PDF about indexing") | ✅ | ❌ | ❌ | ❌ |
| Natural-language time ("screenshots from last week") | ✅ | ❌ | partial | ❌ |
| Your AI agents can search it (MCP) | ✅ | ❌ | ❌ | ❌ |
| Open source (MIT) | ✅ | ❌ | ❌ | ✅ |

*Spotlight and Raycast are excellent but macOS-only; Alfred's content search and AI are paid tiers. Kensaku is one binary for Linux, macOS, and Windows, free and MIT-licensed.*

**Three things worth trying on day one:**

1. **Screenshot archaeology.** You saw that error in a screenshot. Kensaku read the pixels: `kensaku search "CUDA error"`.
2. **Describe, don't recall.** `kensaku search "big videos from last month"` is a valid query. No filters to type.
3. **Give your AI agent eyes.** Claude or Cursor can search this same index over MCP — read-only, local, no cloud.

## Features

**Fast**
- Filename search across ~100,000 files in milliseconds on a warm cache. Method, hardware, and per-platform numbers are in [Performance](#performance).
- The daemon keeps the index warm between commands. Each command reuses it instead of reloading it.
- Incremental indexing: changed files are picked up without rescanning the whole disk.

**Finds more**
- Names, full text, fuzzy matches that tolerate typos, and meaning, in one search.
- **Documents & Office**: PDF, DOCX, PPTX, XLSX, EPUB, ODT/ODS/ODP, RTF, CSV/TSV, email (`.eml`, `.msg`), and plain text.
- **Multilingual & CJK by default**: Custom unigram + bigram tokenizer for Chinese (Hanzi/Bopomofo), Japanese (Kanji/Kana), and Korean (Hangul), plus mixed-script boundary handling. Detailed breakdown in [docs/LANGUAGES.md](docs/LANGUAGES.md).
- **50+ Code & Data Formats**: Rust, Python, TypeScript, Go, C/C++, Java, Shell, SQL, JSON, YAML, TOML, Dockerfile, and more.
- **Images & OCR**: Text is read with PP-OCRv6 by default (bundled ONNX Runtime; the ~6.1 MB model pack downloads once). The Indra engine (deskew, multi-script recognition, ~27 MB pack) and Tesseract are selectable with `kensaku setup --ocr-engine`.
- **Content depth you can tune**: up to 100 MB per file, 4 MB of text, and 48 semantic chunks by default; `quick` / `balanced` / `deep` presets.

**Precise**
- Filters and sort: `--path`, `--ext`, `--since`, and `--exhaustive` on `search`.
- Time windows: `--since 1h`, `7d`, `30d`, or `modified:` inside the query.
- A query language when you want it (below).

**Scriptable**
- `--json` on every command returns the raw API result.
- Documented exit codes, so shell scripts can tell "no index yet" from "daemon unavailable".
- `kensaku call <method> '<json>'` reaches any API method, and `kensaku methods` lists them. See [docs/API.md](docs/API.md).
- Shell completions: `kensaku completions bash|zsh|fish|powershell`.

**Works with AI agents**
- Compatible agents can search the same local index over MCP. Read-only by default. See [Connect AI agents](#connect-ai-agents-mcp).
- Also: `find_symbol` for tree-sitter definitions, bounded regex search, scoped roots, and cursor pagination.

**Private**
- No account, no telemetry, no cloud search. Your index stays in your OS config directory.

### The query language

For scripts and precise searches:

```
type:pdf invoice          ext:md in:"~/Notes"       modified:7d
size:>10mb                path:"client/2026"        lang:rust
name:resume               content:"timeout"         re:"fn \w+_test"
-draft                    mode:exhaustive
```

Full reference: [docs/USAGE.md](docs/USAGE.md) · Language & format support: [docs/LANGUAGES.md](docs/LANGUAGES.md).

## Install

Every release attaches one archive per platform to the
[latest release](https://github.com/ibarra000/kensaku-headless/releases/latest). Each archive
contains the `kensaku` binary, the ONNX Runtime library it loads for OCR, and the
LICENSE and README.

| Platform | Archive |
|---|---|
| Linux (x86_64) | `kensaku-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Apple silicon) | `kensaku-<version>-aarch64-apple-darwin.tar.gz` |
| macOS (Intel) | `kensaku-<version>-x86_64-apple-darwin.tar.gz` (no bundled ONNX Runtime; OCR uses Tesseract) |
| Windows (x86_64) | `kensaku-<version>-x86_64-pc-windows-msvc.zip` |
| Windows (ARM64) | `kensaku-<version>-aarch64-pc-windows-msvc.zip` |

Every release also ships `SHA256SUMS`. Check a download with `sha256sum -c SHA256SUMS`
(Linux and macOS) or `Get-FileHash` (Windows PowerShell).

The Windows `kensaku.exe` is Authenticode-signed through [SignPath Foundation](https://signpath.org/).
macOS binaries are not signed or notarised yet, so Gatekeeper blocks them the first time.
Unpack the archive, then run `xattr -d com.apple.quarantine ./kensaku` if macOS refuses to start it.

[docs/INSTALL.md](docs/INSTALL.md) has the full steps, including the start-at-login service.

### Build from source

```bash
git clone https://github.com/ibarra000/kensaku-headless
cd kensaku
cargo build --release -p kensaku
./target/release/kensaku --help
```

You need Rust 1.98 (the workspace sets `rust-version = "1.98"`). A `Makefile` covers
the common tasks: `make build`, `make test`, `make lint`, `make fmt`, `make smoke`,
and `make bench`. See [docs/INSTALL.md](docs/INSTALL.md).

## Quick start

```bash
# 1. Choose the folders to index and start the first scan.
kensaku setup --root ~/Documents --root ~/Projects --wait

# 2. Search names, contents, and meaning.
kensaku search "docker error"
kensaku search "invoice" --ext pdf --since 30d
kensaku search "resume" --json

# 3. Check on the index and the daemon.
kensaku status
kensaku health
```

The first `kensaku` command in a session starts the background daemon if it is not
running. Search before `setup` has been run exits with code 3. Setup is covered in
[docs/INSTALL.md](docs/INSTALL.md#first-run).

## Connect AI agents (MCP)

Point an MCP client at the CLI:

```json
{
  "mcpServers": {
    "kensaku": { "command": "kensaku", "args": ["mcp"] }
  }
}
```

`kensaku connect <claude-desktop|claude-code|cursor>` prints that entry for the
client, or merges it into the client's config with `--write` (a `.bak` backup is
kept).

For clients that connect over HTTP, run `kensaku serve`. It listens on loopback
only and requires a bearer token stored in your config directory. Writes over
HTTP are off unless you pass `--allow-writes`.

Responses are budgeted in size, and file content is returned as data, never as instructions.
Full guide: [docs/AGENTS.md](docs/AGENTS.md). HTTP routes and method reference: [docs/API.md](docs/API.md).

## Performance

Speed is the point of Kensaku. Every number below was measured; the full method, corpus, hardware, and caveats are in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).
The numbers were measured on 1.0.16. Re-run the harnesses on 2.0 before quoting them as 2.0 figures.

| What | Result |
|---|---|
| Name search, 100k files (Windows 11, 1.0.16, default threads) | **18 ms average** (best 8.3 ms) |
| Name search, 100k files (Windows 11, 4 threads) | **3.1 ms average** |
| Name search, 100k files (Linux reference, warm) | **1.3 ms average** (best 0.5 ms) |
| Metadata for 100k files | **12.2 MB** (128 B per entry) |
| First run over 101k mixed files (earlier Linux reference) | 184 s, peak 183 MB |
| Re-index with nothing changed (same corpus) | 0.56 s |

The Windows and Linux rows are the same workload on different machines, not a
regression: Windows NTFS metadata reads and antivirus make the walk much slower,
and a 16-thread rayon pool adds scheduling overhead to a scan that the
char-bloom prefilter already makes cheap. On the same 100k corpus, 4–8 threads
average 3–5 ms. The Linux number is the historical warm-cache reference. Neither
is a cross-platform "fastest" claim.

Same folder class, filename search, cold process each run (Linux reference):

| Tool | "invoice" | "deploy_error" |
|---|---|---|
| Kensaku (in memory, no start-up) | 1.3 ms avg | 1.3 ms avg |
| plocate | 34 ms | 6 ms |
| fd | 109 ms | 98 ms |

The two rows aren't a perfect match: Kensaku's search is fuzzy and runs inside a running daemon, while plocate and fd start a new program for each search. Spotlight, Everything, and Listary haven't been measured, and these numbers come from Linux, not macOS or Windows.

**Memory.** The in-memory metadata layer is 128 bytes per `FileEntry`: 12.2 MB for 100,201 entries. The index itself, Tantivy, and the optional embedding table are separate.

Indexing is a single pass: each file is read once and feeds both the name and text indexes. Work is queued in bounded batches, so memory stays flat on large folders.

## Privacy and security

- **Local by default.** No telemetry, no account, no cloud search. Search queries and file contents never leave your machine. The only outbound requests are model and OCR pack downloads, and only when you ask for them or when the OCR engine you selected needs its pack.
- **You choose what is indexed.** Watched folders and exclusions are yours. Hidden files and common build folders are skipped by default, and credential files such as `.pem`, `.key`, and `.pfx` are excluded.
- **Bounded processing.** Each file has a time limit and a size limit, so one bad file can't stall the index.
- **Read-only agent access.** The agent tools cannot change files. `reindex` is off unless you enable it.
- **Treat the index as sensitive.** It contains file names and extracted text. Don't sync it to a shared location.

Full threat model: [SECURITY.md](SECURITY.md).

## Status

**Shipping now (2.0.0, unreleased)**

- Headless `kensaku` binary: search, inspection, setup, roots and exclusions, configuration, indexing control, packs, doctor, and daemon management
- A background daemon that owns the index on Linux, macOS, and Windows, and starts on demand
- An API with one method table, reachable from the CLI (`kensaku call`), the daemon's JSON-RPC socket, a loopback HTTP API (`/v1/<method>`), and the Rust crate
- MCP over stdio (`kensaku mcp`) and MCP Streamable HTTP through `kensaku serve`
- Start-at-login for the daemon: a Run key on Windows, a systemd user unit on Linux, and a LaunchAgent on macOS
- Search by name, content, meaning, and typo-tolerant matches
- Documents, code, and images (with OCR), CJK-aware
- Incremental indexing that tracks renames and deletes
- `.gitignore` and `.cursorignore` respected by default

**Known limitations**

- macOS binaries are not signed or notarised yet, so Gatekeeper asks for approval on first run.
- OCR: PP-OCRv6 by default (bundled runtime, ~6.1 MB model pack); optional Indra engine with deskew and multi-script recognition (~27 MB pack). Intel Macs fall back to Tesseract. JPEG 2000, fax, and JBIG2 scans are not decoded yet.
- Thumbnails cover image files only.
- Visual search is a beta model download and is not part of search results yet.
- Windows gets the most real-world testing today. macOS and Linux builds ship from CI and would benefit from more hands on them — see below.

## What might come next

Directions for the headless build, in no fixed order:

- **Typed client libraries.** Thin wrappers over the HTTP `/v1` API for Python and TypeScript, generated from the method table.
- **Push notifications over the daemon socket.** Stream status and watcher events to long-lived clients instead of polling `get_status`.
- **More CLI coverage of agent tools.** `regex` and `symbol` commands alongside the existing `search`.
- **macOS signing and notarization.** An Apple Developer ID for warning-free first runs.
- **Visual and related-file lookups.** Text-to-image and similar-image search once a real visual encoder is wired in; `find_related` for same-stem/import/neighbour lookups.

Open a [discussion](https://github.com/ibarra000/kensaku-headless/discussions) with your use case, or upvote an idea in the [issue tracker](https://github.com/ibarra000/kensaku-headless/issues).

## Contributing

Kensaku Headless is MIT-licensed and builds on the original [Kensaku](https://github.com/yjid/kensaku). Every kind of help moves it forward:

- **Use it and break it.** Bug reports with steps to reproduce are the single most valuable contribution right now — especially from macOS and Linux users.
- **Shape the roadmap.** Feature ideas land in [Discussions](https://github.com/ibarra000/kensaku-headless/discussions); the ones with real use cases behind them get built first.
- **Good first issues.** Check the [issue tracker](https://github.com/ibarra000/kensaku-headless/issues) for tasks scoped for a first contribution.
- **Search quality.** Ranking, typo tolerance, and new extractors ([docs/EXTRACTORS.md](docs/EXTRACTORS.md) shows how to add one).
- **Agents and integrations.** MCP tooling and guides ([docs/AGENTS.md](docs/AGENTS.md)).
- **Docs and examples.** [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) has the setup: `cargo build -p kensaku`, `make test`, `make lint`, and `make smoke`. CI runs the workspace tests and clippy on Linux, macOS, and Windows, plus the headless smoke test.
- **Security.** Please report vulnerabilities privately via [SECURITY.md](SECURITY.md).

If Kensaku saves you ten minutes, [star the repository](https://github.com/ibarra000/kensaku-headless/stargazers) — it's the only marketing an independent tool has, and it's how the next contributor finds us.


## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=ibarra000/kensaku-headless&type=Date)](https://star-history.com/#ibarra000/kensaku-headless&Date)
