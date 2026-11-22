# Kensaku for AI agents

Kensaku exposes its index to external agents. It is **read-only by default,
local-only, and shares the same index as the `kensaku` command** — no API keys and no
outbound network. An opt-in `reindex` tool is the only write action.

Two surfaces, one binary:

- **MCP** over stdio — for Claude Code, Cursor, and any MCP client (`kensaku mcp`).
- **CLI** — for shell-capable agents and one-off queries (`kensaku search --json`, ...).

Both use the same index as the CLI: `~/.config/kensaku` on Linux,
`%APPDATA%\kensaku` on Windows, and `~/Library/Application Support/kensaku` on
macOS. The MCP server pings the daemon and forwards searches/state to it when
available; with no daemon it opens the index read-only and loads
metadata/vectors once per session.

For the HTTP API (`kensaku serve`, routes under `/v1`) and the full method table, see
[API.md](API.md).

---

## MCP setup

Use the absolute path if `kensaku` is not on your `PATH`.

```json
{
  "mcpServers": {
    "kensaku": { "command": "kensaku", "args": ["mcp"] }
  }
}
```

`kensaku connect <client>` prints this entry for `claude-desktop`, `claude-code`, or
`cursor`. Add `--write` to merge it into the client's config file; the previous file is
kept as a `.bak` backup.

Protocol: JSON-RPC 2.0, newline-delimited, over stdin/stdout. Only protocol
messages are written to stdout; logs go to stderr.

### Tools

#### `search`

```json
{
  "query": "docker compose error",
  "limit": 10,
  "mode": "ranked",
  "path": "~/Projects",
  "ext": "rs",
  "modified_after": "7d"
}
```

| field | type | notes |
|---|---|---|
| `query` | string, required | full query language: `type:`, `ext:`, `in:`, `modified:`, `size:`, `re:"..."`, `-negation` |
| `page_size` | 1–50, default 10 | results per page (`limit` remains a deprecated alias) |
| `cursor` | string | opaque `next_cursor` from the previous page |
| `mode` | `ranked` \| `exhaustive` | ranked for relevance; exhaustive for completeness (path-sorted, cap 10k) |
| `path` | string | becomes `in:"<path>"` |
| `ext` | string | becomes `ext:<ext>` |
| `modified_after` | string | lookback window, becomes `modified:<window>` (e.g. `7d`, `30d`, `1h`) |

Returns JSON in `content[0].text`:

```json
{
  "results": [
    { "path": "...", "score": 0.91, "snippet": "...", "match_type": "Both",
      "size": 1024, "modified": 1761400000 }
  ],
  "total": 12,
  "mode": "ranked",
  "returned": 10,
  "page_size": 10,
  "next_cursor": "k1-a",
  "has_more": true,
  "token_budget": 1500,
  "as_of": 1761400000
}
```

Snippets are capped at 100 characters and flagged with `snippet_truncated`.
`match_type` is `FileName`, `Content`, `ContentFolded`, `Semantic`, or `Both`.
Use `next_cursor` until `has_more` is false. Offsets are capped at 10,000; a page
at the cap reports `cursor_limit_reached: true` instead of materialising more.

#### `read_snippet`

```json
{ "path": "/abs/file.rs", "query": "timeout", "max_tokens": 256 }
```

`max_tokens` is 32–1024 (default 256). Returns `{ path, line_start,
line_end, text, truncated, token_budget }`. Text is capped at
`max_tokens × 4` characters and centered on the first match. Never returns a
whole large file.

#### `health_check`

No arguments. Returns `{ ok, daemon, db, index, model, model_real,
protocol_version, watcher_ok, files, content_docs, semantic_units, phase,
is_indexing, paused, error, as_of }`. An unhealthy index returns `ok: false`
with a reason, not a tool error.

#### `index_state`

No arguments. Returns:

```json
{ "files": 1204553, "dirs": 84210, "content_docs": 186420, "semantic_units": 402118,
  "index_bytes": 759000000, "model_id": 2, "active_model_id": 2,
  "model_name": "potion-base-8M", "roots": ["/home/you"], "as_of": 1761400000 }
```

`model_id` is the model that produced the stored vectors (`null` if none);
`active_model_id` is what new queries use. If they differ, a re-embed is pending.
The payload also carries `phase`, `is_indexing`, `paused`, extraction progress,
`model_real` and `last_reconcile`.

#### `regex_search`

`{ "pattern": "fn \\w+", "page_size": 20 }`. Patterns are capped at 512 bytes,
compiled before execution, and return the same pagination/snippet shape as
`search`.

#### `reindex` (opt-in write)

No arguments. Hidden from `tools/list` and disabled by default. To enable it,
run `kensaku config set mcp_allow_reindex true` and restart the MCP
client. The daemon re-checks the setting on every call; without a daemon the
tool is unavailable.

---

## CLI

```bash
kensaku search "docker error" --json --limit 10
kensaku search "type:pdf invoice" --path ~/Documents
kensaku search 're:"fn \w+"' --ext rs --since 30d
kensaku search "config" --exhaustive --json
kensaku state --json
kensaku mcp
```

Default search output is `path<TAB>score<TAB>snippet`; `--json` prints the raw
search response. Exit codes: `0` ok, `1` failure, `2` usage, `3` no index or setup
incomplete (run `kensaku setup --root <dir>`), `4` daemon unavailable, `5` forbidden,
`6` busy. Scripts should branch on these codes rather than on message text.

Global flags: `--json`, `--data-dir <DIR>`, and `--no-daemon` (read-only commands
only, reads the index without the daemon).

---

## Behavior and limits

- **Read-only by default.** No shell and no network. `reindex` is the only
  write tool and requires the explicit config opt-in plus the daemon.
- Honors the watched folders and exclusions configured with `kensaku setup`,
  `kensaku roots`, and `kensaku exclude`.
- Ranked mode fuses fuzzy filename, BM25 content, and static-vector tiers with
  RRF; semantic-only hits below a model-dependent cosine floor are dropped.
- `exhaustive` skips the ranked tiers and enumerates by path, capped at 10,000.
- No index → tools return a clear error; they never create one.
- Daemon-first sessions avoid opening SQLite, Tantivy or the embedding table in
  the MCP process. Without a daemon, startup cost is once per session; a
  one-shot CLI call pays metadata load, so prefer the MCP session.

---

## Other shipped tools and limits

- **`find_symbol`** — tree-sitter definitions (Rust, Python, JavaScript,
  TypeScript): `{ query, language?, path?, page_size≤50 }` → path, kind, line
  range, one-line signature. Candidates come from the content index; up to
  200 files are parsed per call.
- **`similar_images`** — similar-image lookup by indexed image path:
  `{ path, limit≤50 }`. It is listed only when a real visual encoder exists.
  In shipped builds `active_encoder()` is `None`, so the tool self-hides and
  the visual tier is not part of search.
- **Keyset pagination past the 10k cap** — exhaustive cursors `k2-<hex path>`
  return matches strictly after the last path; `total` is then a windowed
  lower bound (`total_is_lower_bound: true`). Offset cursors (`k1-`) still
  work for ranked pages.
- **`.gitignore` / `.cursorignore` parity** — common pattern syntax, honored
  by default so agents see the same tree developers do; `include_ignored`
  opts out. An excluded parent directory cannot be re-included (git
  semantics).
- **Read-only scoping per root** — `agent_scoped_roots` in config restricts
  search results and `read_snippet`/`find_symbol` paths to those roots;
  out-of-scope requests are rejected, not silently trimmed. Paths are resolved
  before comparison, so `..` and symlink escapes do not pass the check. Set it with
  `kensaku config set agent_scoped_roots '["/home/you/Projects"]'`.
- **Loopback HTTP** — `kensaku serve [--port 8765]`, bound to `127.0.0.1`:
  - `POST /mcp` — MCP Streamable HTTP (stateless JSON-RPC).
  - `GET /health`, `GET /search?q=`, `GET /events` (SSE keepalive).
  - `GET|POST /v1/<method>` — the full API method table. Write methods need
    `kensaku serve --allow-writes`.
  - `GET /v1/events` — SSE stream of `status` events while indexing changes.

  Every request needs `Authorization: Bearer <token>`. The token is in
  `<data dir>/http.token`, created on first use, and printed at startup.
  `reindex` is never enabled on this transport. There is no `/ask` route and no
  chat endpoint; Kensaku does not bundle or call an LLM. The method reference is
  [API.md](API.md).

## Planned

- **`find_related`** — related-file lookups (same stem, imports, neighbors).
- **Visual agent tools** — text-to-image and similar-image lookups once a real
  visual encoder is wired into search. Read-only and local like everything else.

Technical shape: [ARCHITECTURE.md](ARCHITECTURE.md) §8.
