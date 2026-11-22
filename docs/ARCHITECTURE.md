# Architecture

> Status: `2.0.0-dev`. This documents the code in this repository, not a future
> plan. Anything marked **not shipped** is called out explicitly.

Kensaku is a local index with four retrieval tiers and a headless command-line
program. The Rust workspace has two crates:

- `crates/kensaku-core` (library `kensaku_core`) holds the index, extraction, OCR,
  query pipeline, daemon, API, and the MCP and HTTP servers.
- `crates/kensaku` builds one binary, `kensaku`: the CLI, the daemon launcher, and the
  entry points for the MCP and HTTP servers.

There is no GUI and no webview. Indexing logic lives only in `kensaku-core`, and every
client reaches it through the same API.

## 1. Process model

| Process | Started by | Role |
|---|---|---|
| `kensaku daemon run` | `kensaku daemon start`, auto-spawned by any command, or a start-at-login entry | The index owner. Holds the writer lock, runs the crawl and watcher, serves the API over JSON-RPC. |
| `kensaku <command>` | the user or a script | A thin client. Sends one API call to the daemon and prints the result. |
| `kensaku mcp` | an MCP client | MCP over stdio. Forwards to the daemon when one is running; otherwise reads the index in-process, read-only. |
| `kensaku serve` | the user or a service manager | Loopback HTTP API (`/v1`), MCP Streamable HTTP, and SSE. Forwards to the daemon. |

**Daemon-first, with a read-only fallback.** Every command calls
`daemon::ensure_running()`. That pings the endpoint and, if nothing answers, spawns
`kensaku daemon run` detached (`crates/kensaku-core/src/daemon/mod.rs`). The binary is
found next to the running executable, through `KENSAKU_BIN`, or on `PATH`. The daemon's
output goes to `<data dir>/daemon.log`. The spawn waits up to 15 seconds for the endpoint.

With `--no-daemon`, a read-only command opens the index in-process instead. A command
that changes state needs the daemon.

The daemon takes the exclusive writer lock `kensaku.lock` in the data directory when it
starts (`crates/kensaku-core/src/runtime.rs`). The same lock is taken on Windows, so the
daemon owns the index on every OS. A second owner gets `AlreadyRunning` and exits.

```
┌───────────────┐  ┌───────────────┐  ┌──────────────────┐  ┌──────────────────┐
│ kensaku CLI   │  │ kensaku mcp   │  │ kensaku serve    │  │ Rust apps        │
│ (commands)    │  │ (stdio)       │  │ (HTTP /v1, MCP)  │  │ api::Client      │
└──────┬────────┘  └──────┬────────┘  └────────┬─────────┘  └────────┬─────────┘
       │ JSON-RPC 2.0 over Unix socket or named pipe, token + protocol v2
       ▼                  ▼                    ▼                     ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ kensaku daemon run  (single writer; holds kensaku.lock)                     │
│ api::METHODS → api::dispatch → ops/*                                        │
├──────────────────┬───────────────────┬──────────────────────────────────────┤
│ metadata         │ content           │ semantic                             │
│ SQLite WAL       │ Tantivy BM25      │ i8 static table (+ optional smart)   │
│ + arena          │ + folded field    │                                      │
├──────────────────┴───────────────────┴──────────────────────────────────────┤
│ walker · extractors · OCR · watcher · embed queue                           │
└─────────────────────────────────────────────────────────────────────────────┘
```

The IPC protocol is newline-delimited JSON-RPC 2.0. Each request carries the token from
`<data dir>/daemon.token` and `protocol_version` (currently 2). The daemon refuses a
mismatched token or version, so an old daemon and a new client cannot talk. Stop the old
daemon after an upgrade.

Endpoints are per data directory, so a test's `KENSAKU_DATA_DIR` never collides with the
real daemon:

| OS | Endpoint |
|---|---|
| Unix | `$XDG_RUNTIME_DIR/kensaku-<id>.sock`, falling back to `<data dir>/kensaku.sock` (`<id>` is a short hash of the data directory path) |
| Windows | named pipe `kensaku-<id>` |

## 2. On-disk layout

Everything lives under the data directory. Its default is the OS config directory:

| OS | Default path |
|---|---|
| Linux | `~/.config/kensaku/` |
| macOS | `~/Library/Application Support/kensaku/` |
| Windows | `%APPDATA%\kensaku\` |

`--data-dir` and `KENSAKU_DATA_DIR` override it. A `portable` or `kensaku.portable`
marker next to the executable switches everything to `<exe_dir>/data`.

| Path | Contents |
|---|---|
| `config.json` | Settings (see [CONFIG.md](CONFIG.md)). |
| `metadata.db` | SQLite in WAL mode: roots, nodes, semantic units, usage, meta. |
| `index/` | Tantivy content index, schema v4. Old schemas are kept as `index.legacy-*` and rebuilt once. |
| `models/` | Static embedding table and the optional smart model. |
| `ocr/`, `ocr-indra/`, `tessdata/` | Downloaded OCR packs and Tesseract language data. |
| `thumbs/` | 240 px JPEG thumbnails from the `thumbnail` method. |
| `kensaku.log`, `crash.log` (`crash.log.1` after rotation) | Startup milestones, and the failure-only crash log. |
| `daemon.log` | Output of the auto-spawned daemon. |
| `daemon.token` | Token for the daemon endpoint (32 random bytes, hex). |
| `http.token` | Bearer token for `kensaku serve`, created on first use (0600 on Unix). |
| `kensaku.lock` | The writer lock held by the daemon. |
| `.reset-requested` | Marker for a pending `reset`, applied on the next daemon start. |

The index is sensitive: it contains file names and extracted text. See
[PRIVACY.md](PRIVACY.md).

## 3. Metadata and the in-memory arena

SQLite is durable truth. Startup builds a compact in-memory arena for the hot
path instead of materializing a `PathBuf` per entry:

- a 64-byte `Node` per file/directory;
- concatenated folded names plus a path index;
- a 64-bit character bloom per name used as a cheap prefilter;
- a small delta buffer for watcher mutations, merged at idle.

Identity is `(device, inode)` / Windows `file_id`, never the path, so a rename
updates one row and never re-extracts content. Unavailable roots (unplugged
drives, unmounted shares) are preserved rather than pruned.

`FileEntry` is the materialized query-time view. Its exact size is printed by
the `bench` harness; the reference footprint is ~128 bytes per entry, which is
the number used in [BENCHMARKS.md](BENCHMARKS.md).

## 4. Index tiers

Four tiers run or short-circuit in this order. Exact work always gets the first
chance to fill the result limit.

### 4.1 Fuzzy filename (`search/fuzzy.rs`)

`nucleo-matcher` over the arena, with path/name weighting, the character-bloom
prefilter, and top-k selection. This is the tier measured by the `bench`
example and the one that answers most keystrokes.

### 4.2 BM25 content (`indexer/content.rs`, Tantivy)

Schema v4 stores:

| Field | Type | Purpose |
|---|---|---|
| `path` | string, stored | delete-by-path, join to metadata, result identity |
| `filename` | text, stored | filename relevance |
| `content` | text, not stored | BM25 |
| `content_folded` | text, trigram, freqs only | look-alike and substring tier |
| `content_raw` | stored, not indexed | verify folded candidates |
| `modified` | u64, stored + fast | `modified:` |
| `ext` | string, indexed | pushdown for `ext:` |
| `size` | u64, fast | pushdown for `size:` |

Schema v4 dropped trigram positions because folded search only issues boolean
term queries and verifies hits against `content_raw`; positions were the largest
single per-file index cost. `lang:` is handled in the metadata layer rather than
stored as a Tantivy fast field.

The custom `kensaku` tokenizer emits interleaved unigrams and bigrams for Han,
Hiragana, Katakana, Bopomofo, and Hangul, with exact offsets and sequential
positions. Latin, code, and paths keep simple tokenizer behavior. Details:
[LANGUAGES.md](LANGUAGES.md).

Positive `ext:` and `size:` filters become `Must` clauses before top-k, so
BM25 never scores documents the metadata filter would reject. Other filters
remain post-filters; the metadata check is authoritative.

### 4.3 Semantic (static vectors, `embed/`)

The default model is `potion-base-8M`-style static embeddings: tokenize →
table lookup → weighted mean → normalize. The shipped table is i8 with per-row
scales (~7.3 MiB); `semantic_f32_table` keeps the f32 form (~30 MB) for
comparison or rollback. No ONNX graph and no GPU at query time. The table is
fetched with `kensaku packs download models`.

Units are 240-token windows with 48 overlap, up to `content_max_semantic_units`
(48) per document. Files under the window size get one doc-level unit. Vectors
are deduplicated by content hash, and `model_id` is stored with every unit so a
model change never mixes vector spaces. Exact scan is used at desktop scale; an
ANN index is deliberately not built below hundreds of thousands of units.

An optional `smart` model compiles behind the `smart-embeddings` feature. It is
opt-in, uses ONNX Runtime, and falls back to the static table with a visible
reason if missing.

### 4.4 Folded / substring fallback

When exact tiers leave a shortfall, look-alike normalization (`0/o`, `1/l/i`,
`5/s`, `8/b`, `m/rn`, `w/vv`, `d/cl`) plus Tantivy's trigram tokenizer finds
partial words and OCR misreads. Each candidate is verified against
`content_raw`; a misread is allowed only within the word-length rule. Folded
hits always rank after exact hits and are marked `ContentFolded`.

## 5. Query pipeline

```
input → lexer → AST → router → planner → tiers → RRF → rank → snippet → response
```

- **Grammar** (`query/parser.rs`): `type:`, `ext:`, `in:`, `path:`, `modified:`,
  `size:`, `is:`, `lang:`, `name:`, `content:`, `re:`, `mode:exhaustive`,
  quoted phrases, `near[n]`, and `-negation`. Unknown `key:` is literal text;
  parse errors produce a hint, never zero results.
- **Intent** (`query/intent.rs`): deterministic, local plain-language rules turn
  "screenshots from last week" or "biggest files" into filters and sort. Every
  inference is reported in the response's `interpretation` field. No model is
  involved. Explicit grammar wins.
- **Scope** (`query/scope_trie.rs`): `in:` resolves through a folded directory
  trie (exact > prefix > fuzzy) and searches a cached DFS range.
- **Fusion**: reciprocal rank fusion over the tiers, then exact/prefix name
  bonuses, smooth recency, frecency from opens, and a bounded depth penalty.
  Ties break on path, so results are deterministic.
- **Exhaustive mode** skips ranked tiers and enumerates by path, capped at 10k.
  Agent cursors continue past the cap with keyset pagination (`k2-` cursors);
  offset cursors (`k1-`) cover ranked pages. `cursor_limit_reached` is explicit
  rather than a silent truncation.

Filters never replace the text; they narrow it. Category counts (facets) are
computed server-side over the full matched set, not the visible page.

## 6. Indexing and extraction

The walker uses `jwalk`, skips hidden entries by default, applies the configured
exclusions, and honors `.gitignore` / `.cursorignore` unless `include_ignored`
is set. Sensitive paths and credential extensions are default-denied. Cloud
placeholders are indexed by name only, so previews do not hydrate them.

Extraction is one pass feeding both the content index and the semantic index:

| Input | Method |
|---|---|
| Text / code / config | Encoding detection (UTF-16 BOM, Latin-1 fallback) → UTF-8. |
| CSV / TSV | Delimiter sniff → rows as `column = "value"`, header repeated. |
| DOCX / PPTX / XLSX / EPUB / ODT / ODS / ODP | ZIP + `quick-xml`; DTDs disabled. |
| PDF | `lopdf` text layer; scanned pages OCR'd one page at a time. |
| Images | PP-OCRv6 by default; optional Indra; Tesseract fallback. |
| Archives | Metadata only. |
| External | Opt-in config-defined executable; no shell, bounded output/time, process-group kill. Zero plugins by default. |

The extraction pool is bounded (up to 8 workers), each file has a timeout, and
extracted text goes through a bounded channel instead of accumulating in memory.
Content policy defaults are **100 MB** maximum file size, **4 MB** extracted
text, **48** semantic units, and `content_depth = "balanced"`. Files above the
file-size cap are still searchable by name and metadata. Retries are capped at
three and reset when the file or OCR settings change.

The watcher (`notify`) coalesces a 400 ms settle window, re-checks freshly
modified files before trusting them, handles rename/delete while paused, and
schedules a scoped rescan on overflow (with a cooldown so a network drive cannot
restart a full walk forever). Perceptual hashing is a separate one-decode-at-a-
time background pass, not part of the crawl.

## 7. OCR

Three local engines, one selected at a time:

| Engine | Config value | Notes |
|---|---|---|
| PP-OCRv6 tiny | `accurate` (default) | ~6.1 MB pack, ONNX Runtime loaded at run time, in-process. |
| Indra | `indra` | ~27 MB pack; deskew, orientation, multi-script routing. |
| Tesseract | `tesseract` | System binary + language data; fallback where ONNX cannot run (Intel Macs, builds without `accurate-ocr`). |

PP-OCRv6 ships in normal builds via the default `accurate-ocr` feature. Release
archives bundle ONNX Runtime 1.31 next to the binary (Linux x64, Apple silicon, Windows
x64 and ARM64); the Intel macOS archive does not. The Tesseract language list is
configurable with `ocr_languages`. The daemon repairs a missing selected pack in the
background when OCR is enabled. Native platform OCR is **not** implemented.

## 8. Agent surface

Two transports, one read-only contract. Both forward to the daemon.

- **MCP over stdio** — `kensaku mcp`, newline-delimited JSON-RPC 2.0. Only
  protocol messages go to stdout.
- **Loopback HTTP** — `kensaku serve [--port 8765]`, bound to `127.0.0.1`:

  | Route | Purpose |
  |---|---|
  | `POST /mcp` | MCP Streamable HTTP (stateless JSON-RPC). |
  | `GET /health` | Health summary. |
  | `GET /search?q=` | Search by the `q` query parameter. |
  | `GET /events` | SSE keepalive (liveness). |
  | `GET\|POST /v1/<method>` | The full API method table. Access classes are enforced; see below. |
  | `GET /v1/events` | SSE stream of `status` events while indexing changes. |

  Every request needs `Authorization: Bearer <token>`. The token lives in
  `<data dir>/http.token`, is created on first use, and is printed at startup. The listener
  checks the peer address on each connection.

Write methods on `/v1` return `forbidden` unless `kensaku serve --allow-writes` is given.
`/v1` file-reading methods honor `agent_scoped_roots`.

Default MCP tools: `health_check`, `search`, `read_snippet`, `index_state`,
`regex_search`, and `find_symbol`. `similar_images` is listed only when a real visual
encoder exists, which the shipped build does not have. `reindex` is listed only when
`mcp_allow_reindex = true` and the daemon is running, and it is never enabled on the HTTP
transport. Search pages are capped at 50, snippets at 100 characters, token budgets are
explicit, and result offsets stop at 10,000 with `cursor_limit_reached`.

`agent_scoped_roots` restricts results and single-path reads. Paths are resolved
before comparison, so `..` and symlink escapes do not pass. Credential files and
excluded extensions are refused even inside a scope. File content is returned as
data, never instructions. See [AGENTS.md](AGENTS.md).

## 9. API, daemon, and errors

The API is one method table, `crates/kensaku-core/src/api/methods.rs` (`METHODS`). Each
entry has a name, an access class, and a one-line summary:

| Access | Meaning |
|---|---|
| `Read` | Reads index state only. |
| `FileRead` | Reads file contents for a caller-supplied path. HTTP applies the agent scope. |
| `Write` | Changes config, the index, or downloads data. Disabled on HTTP unless writes are allowed. |

`METHODS` is the source of truth for `kensaku methods`, the HTTP write gate, and
[API.md](API.md). `api/dispatch.rs` handles every entry, and a test fails when one is
missing. Nothing else defines a method.

Four ways to reach the table:

| Entry point | Who uses it |
|---|---|
| Daemon JSON-RPC | The CLI and the MCP and HTTP servers, through `daemon::call`. |
| `kensaku call <method> '<json>'` | Scripts and manual use. |
| HTTP `/v1/<method>` | Non-Rust clients. Gated as described in §8. |
| `api::Engine` and `api::Client` | Rust code. `Engine` runs the API in-process and owns the index (take the writer lock). `Client` talks to a running daemon. |

Errors have stable codes (`crates/kensaku-core/src/api/error.rs`). The string form is
part of the contract and appears in JSON-RPC `error.data.code` and in HTTP error bodies.

| Code | Exit | HTTP | Meaning |
|---|---|---|---|
| `invalid_params` | 2 | 400 | Missing or malformed parameters. |
| `method_not_found` | 2 | 404 | No such method. |
| `no_index` | 3 | 404 | No index exists yet. Run `kensaku setup`. |
| `not_setup` | 3 | 409 | Setup has not been completed. |
| `daemon_unavailable` | 4 | 503 | The daemon is not running and could not be started. |
| `forbidden` | 5 | 403 | Scope, write gating, or token refusal. |
| `busy` | 6 | 409 | Conflicts with work already in progress. |
| `internal` | 1 | 500 | Anything else. |

## 10. Releases

`.github/workflows/release.yml` builds `kensaku` on tag pushes (`v*`) for x86_64 Linux,
aarch64 and x86_64 macOS, and x86_64 and aarch64 Windows. Each target produces one
archive, `kensaku-<version>-<target>.tar.gz` or `.zip`, with the binary, the ONNX Runtime
library where one is bundled, and the LICENSE and README. The workflow also writes
`SHA256SUMS` and creates a draft GitHub release for a maintainer to review.

Release binaries are not code-signed yet; Windows signing through SignPath is
prepared in the workflow and runs once configured. There is no self-updater: the
binary does not contact GitHub or any other update service. See
[RELEASING.md](RELEASING.md) and [SIGNING.md](SIGNING.md).

## 11. Tests and benchmarks

- **About 600 test functions** across `crates/`. CI runs `cargo test
  --all-targets --locked`, `cargo clippy --all-targets --locked -- -D warnings`,
  and `tools/headless-smoke.sh` against a built `kensaku` on Linux, macOS, and Windows.
- **Eight benchmark harnesses** in `crates/kensaku-core/examples/`: `bench`, `bench_ocr`,
  `crawl_bench`, `embed`, `facet_bench`, `index_bench`, `index_format_bench`, and
  `mem_probe`. `make bench BENCH_DIR=...` runs `bench`. Method, corpus, and honest caveats
  are in [BENCHMARKS.md](BENCHMARKS.md).

## 12. Deliberate non-goals

- No cloud, accounts, telemetry, sync, or cross-device index.
- No GUI, desktop shell, or installer.
- No bundled LLM and no chat. Agents bring their own model over MCP.
- No write operations through the agent API except the opt-in `reindex`.
- No visual search in the production search path yet. The `visual` pack can be
  downloaded, but `active_encoder()` is `None` in non-test builds, so the visual tier is
  skipped and `similar_images` self-hides.
- No native macOS/Windows OCR.
- No ANN index at desktop scale; exact scan is faster and exact.
- No code signing or notarisation yet.

## 13. Module map

```
Cargo.toml                 workspace (resolver 3, rust-version 1.98)
crates/kensaku/src/
  main.rs       entry point, exit codes
  args.rs       clap grammar (the CLI surface)
  run.rs        command execution: daemon call or --no-daemon read path
  output.rs     human-readable rendering (--json bypasses it)
crates/kensaku-core/src/
  api/          methods.rs (METHODS), dispatch.rs, engine.rs, client.rs, error.rs
  daemon/       mod.rs (client, ensure_running, endpoints), server.rs (daemon side),
                service.rs (start-at-login: Run key, systemd unit, LaunchAgent)
  agent/        mcp.rs, http.rs, connect.rs (MCP config for clients), symbols.rs, mod.rs
  ops/          admin.rs, files.rs, library.rs, search.rs, setup.rs, stats.rs
  embed/        table, units, queue, provider, smart, visual, model, scanner
  indexer/      arena, metadata, metadata_db, content, walker, watcher, ignore,
                burst, tokenizer, extract/
  query/        ast, lexer, parser, router, intent, scope_trie
  search/       fuzzy, mod (fusion, ranking, snippets)
  ocr.rs, ocr_indra/     OCR engines and packs
  config.rs     settings, defaults, migrations
  runtime.rs    writer lock, reset and rebuild markers, start-up sequence
  maintenance.rs, doctor.rs, models.rs, preview.rs, crashlog.rs, util/
crates/kensaku-core/vendor/paddleocr_rs_onnx/   vendored crate (patched in the workspace)
crates/kensaku-core/examples/   benchmark harnesses
tools/headless-smoke.sh         end-to-end smoke test
```

Rule: the daemon is the only writer. Every client, including the MCP and HTTP servers, goes
through the same method table, so the CLI, the agent surface, and Rust callers see the same
behavior.
