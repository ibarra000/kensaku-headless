# Kensaku API

Kensaku has one API: a table of named methods that take a JSON object of
parameters and return a JSON value. Every way of driving Kensaku goes through
that same table, so a method behaves the same no matter which transport calls
it.

| Transport | How to call | Notes |
|---|---|---|
| CLI | `kensaku call <method> '<json params>'` | Most methods also have a friendlier command (`kensaku search`, `kensaku setup`, ...). Add `--json` to any command for the raw result. |
| Daemon JSON-RPC | newline-delimited JSON-RPC 2.0 over a Unix socket / Windows named pipe | What the CLI uses. Needs the token from `<data dir>/daemon.token`. |
| HTTP | `GET` / `POST` `http://127.0.0.1:8765/v1/<method>` | `kensaku serve`. Bearer token from `<data dir>/http.token`. |
| Rust | `kensaku_core::api::{Engine, Client}` + the `Kensaku` trait | `Engine` runs the index in your process; `Client` talks to the daemon. |
| MCP | `kensaku mcp` / `POST /mcp` | A curated, read-only subset for agents. See [AGENTS.md](AGENTS.md). |

`kensaku methods` prints the live list; `kensaku methods --json` includes the
access class of each method.

## Access classes

Each method has one of three access classes, which transports use for gating.

| Class | Meaning | HTTP behavior |
|---|---|---|
| `read` | Reads index state only. | Allowed (`GET` or `POST`). |
| `file_read` | Reads file contents (or probes the filesystem) for a caller-supplied `path`/`paths`. | Allowed only if every path is part of the index, inside `agent_scoped_roots` (when set), and not an excluded extension. Otherwise `403`. `validate_dir` skips the "indexed" requirement. |
| `write` | Changes config or the index, downloads data, or stops the daemon. | `403` unless the server was started with `kensaku serve --allow-writes`, and `POST` only. |

Additional HTTP rules, because a bearer token is easier to leak than the
daemon socket:

- `/v1/search` drops results that fall outside the agent scope or have an
  excluded extension, so snippets cannot leak out-of-scope content.
- `save_config` is not available over HTTP; use `patch_config`.
- `patch_config` over HTTP refuses `extractors` (external extractors run
  programs), `agent_scoped_roots`, `excluded_extensions` and
  `mcp_allow_reindex`. Change those with `kensaku config set`.

The CLI and the daemon socket are local, token-authenticated and owned by the
same user, so they allow every method.

## Methods

Parameters marked `?` are optional.

### Liveness and state

| Method | Class | Params | Result |
|---|---|---|---|
| `ping` | read | — | `{ ok, protocol_version }` |
| `methods` | read | — | Array of `{ name, access, summary }`. |
| `health` | read | — | `{ ok, daemon, pid, protocol_version, watcher_ok, setup_complete, db, index, model, state }` |
| `get_status` | read | — | Indexing progress: `{ is_indexing, phase, files_indexed, content_indexed, extract_done, extract_total, paused, current_status, ... }` |
| `get_stats` | read | — | `{ queries, index, ingestion, runtime }` (latency percentiles, sizes, throughput). |
| `index_state` | read | — | `{ files, dirs, content_docs, semantic_units, index_bytes, model_id, active_model_id, roots, ... }` |

### Search

| Method | Class | Params | Result |
|---|---|---|---|
| `search` | read | `query`, `limit?` (default 10), `offset?`, `start_after?`, `snippets?` (default true) | `SearchResponse`: `{ results: [{ path, score, snippet, match_type, size, modified }], total, ... }`. Returns `not_setup` before setup. |
| `get_snippets` | file_read | `paths` (≤ 50), `query` | `[{ path, snippet }]` |
| `read_snippet` | file_read | `path`, `query?`, `max_tokens?` (32–1024, default 256) | `{ path, line_start, line_end, text, truncated }`. The path must be indexed and inside the agent scope. |
| `contains_path` | read | `path` | `{ indexed }` |
| `get_scope_suggestions` | read | `prefix?`, `limit?` | Indexed directory paths. |

`query` accepts the full query language (`type:`, `ext:`, `in:`, `modified:`,
`size:`, `re:"..."`, `mode:exhaustive`, quoted phrases, `-negation`); see
[USAGE.md](USAGE.md). For listings past 10,000 results, use `mode:exhaustive`
and pass the last path you received as `start_after`.

### Files

| Method | Class | Params | Result |
|---|---|---|---|
| `preview` | file_read | `path` | `{ path, filename, file_size, modified, is_dir, preview_type, content, total_lines, child_count, width, height }` |
| `details` | file_read | `path` | `{ width, height }` (images only). |
| `thumbnail` | file_read | `path` | `{ thumbnail }`: path of a cached 240 px JPEG. |

### Library

| Method | Class | Params | Result |
|---|---|---|---|
| `get_home_data` | read | — | `{ recent, pins, saved_searches }` |
| `toggle_pin` | write | `path` | `{ pinned }` |
| `record_open` | write | `path` | `{ ok }`. Feeds frecency ranking; call it when your app opens a result. |
| `find_duplicates` | read | `max_distance?` (0–64, default 10), `limit?` | Groups of exact duplicates and near-duplicate images. |
| `find_similar_images` | read | `path`, `limit?` | Empty unless a visual encoder is installed. |

### Configuration and setup

| Method | Class | Params | Result |
|---|---|---|---|
| `get_config` | read | — | The full config ([CONFIG.md](CONFIG.md)). |
| `save_config` | write | `config` (full object) | The saved config. Roots are validated and canonicalized. |
| `patch_config` | write | `patch` (object of top-level keys) | The saved config. Unknown keys are rejected; roots are validated. |
| `setup` | write | `watched_dirs`, `name_only_dirs?`, `excluded_dirs?`, `ocr_engine?` | `{ started, watched_dirs }`. Paths are validated and canonicalized; the first crawl starts in the background. |
| `setup_again` | write | — | Marks setup incomplete and stops the scan. |
| `suggest_roots` | read | — | Existing common folders under the home directory. |
| `validate_dir` | read | `path` | `{ path }`, canonicalized (`~` expanded). |
| `roots_add` | write | `path`, `name_only?` | The saved config. |
| `roots_remove` | write | `path` | The saved config. Refuses to remove the last folder. |
| `exclude_add` / `exclude_remove` | write | `pattern` | The saved config. |

### Indexing control

| Method | Class | Params | Result |
|---|---|---|---|
| `pause` / `resume` | write | — | Content extraction pauses; the watcher keeps applying renames and deletes. |
| `rescan` | write | — | Reloads config from disk and re-crawls. |
| `reindex` | write | — | Agent-facing re-crawl. Requires `mcp_allow_reindex`. |
| `rebuild` | write | — | Wipes the index and rebuilds it from scratch. |
| `cancel_setup_scan` | write | — | Stops the first crawl and marks setup incomplete. |
| `reset` | write | `remove_packs?` | Schedules a full wipe and stops the daemon; the next start applies it. |
| `shutdown` | write | — | Stops the daemon. |

Changing roots or exclusions (`roots_*`, `exclude_*`, `save_config`,
`patch_config`) queues a crawl automatically; it waits for a crawl that is
already running. `rescan` fails with `invalid_params` if `config.json` on disk
is unreadable, instead of replacing it with defaults.

Long operations (`setup`, `rescan`, `rebuild`, `pack_download`) return
immediately. To follow them, poll `get_status`, stream `GET /v1/events`, or
use the CLI's `--wait` / `status --follow`.

### Packs and diagnostics

| Method | Class | Params | Result |
|---|---|---|---|
| `packs_status` | read | — | Download state of `models`, `ocr`, `indra`, `smart`, `visual`, `tessdata`. |
| `pack_download` | write | `pack` | `{ started, pack }`. The download runs in the background. |
| `pack_delete` | write | `pack` (`models` only) | `{ ok }` |
| `doctor` | read | — | `{ checks: [{ id, label, status, detail, fix, action }] }` |
| `doctor_fix` | write | `action` (one of the actions `doctor` reports) | `{ message }` |

## Errors

Every failure carries a stable string code. Transports map it as follows.

| Code | Meaning | JSON-RPC | HTTP | CLI exit |
|---|---|---|---|---|
| `invalid_params` | Missing or malformed parameters | -32602 | 400 | 2 |
| `method_not_found` | No such method | -32601 | 404 | 2 |
| `no_index` | No index exists yet | -32010 | 404 | 3 |
| `not_setup` | Setup not complete | -32011 | 409 | 3 |
| `daemon_unavailable` | Daemon not running and could not start | -32012 | 503 | 4 |
| `forbidden` | Scope, write gate, or bad token | -32003 | 403 | 5 |
| `busy` | Conflicts with work in progress | -32013 | 409 | 6 |
| `internal` | Anything else | -32000 | 500 | 1 |

- **JSON-RPC:** `{"error": {"code": -32602, "message": "...", "data": {"code": "invalid_params"}}}`
- **HTTP:** `{"error": {"code": "invalid_params", "message": "..."}}`
- **CLI:** the message goes to stderr (as `{"error": {...}}` with `--json`), and the process exits with the code shown.

## Daemon protocol

- **Endpoint.** On Windows, the named pipe `kensaku-<id>`. On Unix,
  `$XDG_RUNTIME_DIR/kensaku-<id>.sock`, or `<data dir>/kensaku.sock` when that
  variable is unset. `<id>` is the first 12 hex characters of the SHA-256 of
  the lower-cased data-dir path, so each data directory gets its own daemon.
- **Framing.** One JSON-RPC 2.0 object per line in each direction.
- **Authentication.** Every request's `params` must include `token` (the
  contents of `<data dir>/daemon.token`) and `protocol_version` (currently
  `2`). A mismatch returns `-32001` (bad token) or `-32002` (version); stop an
  old daemon with `kensaku daemon stop`.

```json
{"jsonrpc":"2.0","id":1,"method":"search","params":{"query":"invoice","limit":5,"token":"…","protocol_version":2}}
```

## HTTP

```bash
kensaku serve                      # read-only /v1, MCP at /mcp
kensaku serve --allow-writes       # also allow write methods
TOKEN=$(cat "$(dirname "$(kensaku config path)")/http.token")

curl -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:8765/v1/search?query=invoice&limit=5"
curl -H "Authorization: Bearer $TOKEN" -X POST http://127.0.0.1:8765/v1/setup \
     -d '{"watched_dirs":["/home/me/Documents"]}'
curl -N -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8765/v1/events
```

- **GET parameters** come from the query string. Each value is parsed as JSON
  when it can be (`limit=5`, `snippets=false`); otherwise it is a string.
- **POST parameters** come from a JSON object body.
- **Daemon.** The server starts one on demand.
- **Binding.** Loopback only, and the peer address is checked on every
  connection.

## Rust

```rust
use kensaku_core::api::{Client, Engine, Kensaku};
use kensaku_core::ops::setup::SetupRequest;

// Talk to (and if needed start) the daemon:
let kensaku = Client::connect()?;
// ...or own the index in this process (exclusive with a daemon):
// let kensaku = Engine::open()?;

kensaku.setup(&SetupRequest { watched_dirs: vec!["/home/me/notes".into()], ..Default::default() })?;
let page = kensaku.search("meeting notes", 10, 0)?;
let stats = kensaku.call("get_stats", serde_json::json!({}))?;
```

- **`Engine`** takes the same writer lock as the daemon. It also accepts
  `set_status_listener` for push-style progress updates.
- **Data directory.** Both `Engine` and `Client` use `KENSAKU_DATA_DIR` when it
  is set, otherwise the platform default.
- **Example.** A runnable program is in
  [`crates/kensaku-core/examples/embed.rs`](../crates/kensaku-core/examples/embed.rs).

## Versioning

- **Method table.** Within a protocol version, methods and result fields are
  only ever added, never removed or renamed.
- **Protocol version.** Any breaking change bumps `PROTOCOL_VERSION`
  (`crates/kensaku-core/src/daemon/mod.rs`).
- **Error codes.** The string codes above are stable.
