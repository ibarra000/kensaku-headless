# Usage

> This page documents the `kensaku` command line in 2.0.0-dev. The MCP server and
> the HTTP API are covered in [AGENTS.md](AGENTS.md) and [API.md](API.md).

## Command line

Every command talks to the background daemon, which owns the index. The first command
in a session starts the daemon if it is not running. See [INSTALL.md](INSTALL.md#the-daemon).

### Global flags

| Flag | Meaning |
|---|---|
| `--data-dir <DIR>` | Use this data directory. Env: `KENSAKU_DATA_DIR`. |
| `--json` | Print the raw API result as JSON. |
| `--no-daemon` | Never start or use the daemon. Reads the index directly. Read-only commands only. |

### Exit codes

| Code | Meaning |
|---|---|
| 0 | OK |
| 1 | Failure |
| 2 | Usage error, or unknown method or key |
| 3 | No index, or setup incomplete |
| 4 | Daemon unavailable |
| 5 | Forbidden |
| 6 | Busy (conflicts with work already running) |

Default output is human-readable. Search prints one result per line as
`path<TAB>score<TAB>snippet`, which is stable for scripts. Use `--json` when you need
fields other than those three.

### Commands

| Group | Commands |
|---|---|
| Search | `search`, `snippet`, `preview`, `details`, `thumbnail` |
| Index state | `status [--follow]`, `stats`, `health`, `state` |
| Setup and folders | `setup [--root ...]`, `setup suggest`, `setup again`, `roots list\|add\|rm`, `exclude list\|add\|rm` |
| Configuration | `config get [KEY]`, `config set KEY VALUE`, `config path` |
| Indexing | `index pause\|resume\|rescan [--wait]\|rebuild [--wait]\|cancel` |
| Maintenance | `doctor [--fix ACTION]`, `packs list\|download PACK\|delete models`, `reset --yes [--remove-packs]`, `remove-data --yes` |
| Recents and discovery | `home`, `pin PATH`, `record-open PATH`, `dupes [--max-distance N] [--limit N]`, `similar PATH [--limit N]`, `scopes [PREFIX] [--limit N]` |
| Daemon | `daemon run\|start\|stop\|status\|install\|uninstall\|log` |
| Agents and API | `mcp`, `serve [--port N] [--allow-writes]`, `connect CLIENT [--write]`, `call METHOD [JSON]`, `methods` |
| Other | `completions SHELL` |

Run `kensaku <command> --help` for the full flag list.

---

## Searching

```bash
kensaku search "docker error"
kensaku search invoice --ext pdf --since 30d
kensaku search "type:pdf invoice" --path ~/Documents
kensaku search "resume" --json
```

| Flag | Meaning |
|---|---|
| `-n, --limit <N>` | Results per page. Default 10. |
| `--offset <N>` | Skip this many ranked results. |
| `--after <PATH>` | Continue an exhaustive listing after this path (keyset cursor). |
| `--exhaustive` | Enumerate every match by path instead of ranking. |
| `--path <DIR>` | Restrict to a folder. Same as `in:"<DIR>"`. |
| `--ext <EXT>` | Restrict to an extension. Same as `ext:<EXT>`. |
| `--since <WINDOW>` | Modified within a window: `1h`, `7d`, `30d`. |
| `--no-snippets` | Skip snippet extraction (faster). |

Multiple words are joined into one query. Quote the query when it contains shell
characters such as `<`, `>`, or `*`.

The `--json` output is the raw search response. Its fields include `results`, `total`,
`mode`, `interpretation` (the plain-language chips), `active_filters`, `hints`, and
`facets`. `facets` counts are computed over the full matched set, not the visible page.
Ranked paging stops at an offset of 10,000; past that, use `--exhaustive` with `--after`
to keep going.

### Snippets, previews, and details

```bash
kensaku snippet ~/Notes/meeting.md --query "action items" --max-tokens 256
kensaku preview ~/Notes/meeting.md     # text head, image size, or folder count
kensaku details ~/Photos/IMG_0042.jpg  # image dimensions
kensaku thumbnail ~/Photos/IMG_0042.jpg  # generate (or reuse) a 240px JPEG and print its path
```

`snippet` returns a window of at most `--max-tokens` (32 to 1024, default 256) around the
first match. It never returns a whole large file. `thumbnail` is a cached JPEG; it covers
image files.

---

## Setup and folders

First-run setup is described in [INSTALL.md](INSTALL.md#first-run). After it, folders and
exclusions are managed with these commands:

```bash
kensaku roots list
kensaku roots add ~/Archive                 # index names and content
kensaku roots add ~/Media --name-only       # index names and paths only
kensaku roots rm ~/Archive

kensaku exclude list
kensaku exclude add node_modules
kensaku exclude rm node_modules
```

`roots rm` and `exclude rm` also accept `remove`. Removing a root takes it off the watched
list; no files are deleted from disk.

---

## Configuration

```bash
kensaku config path                     # where config.json lives
kensaku config get                      # the whole config
kensaku config get ocr_engine           # one key
kensaku config set ocr_engine indra     # VALUE is parsed as JSON, falling back to a string
kensaku config set content_depth deep
```

Keys, defaults, and migrations are in [CONFIG.md](CONFIG.md). Unknown keys are rejected
with exit code 2.

---

## Indexing and health

```bash
kensaku status                # indexing progress
kensaku status --follow       # keep printing until indexing is idle
kensaku stats                 # query latency, index sizes, runtime statistics
kensaku health                # daemon, database, index, and watcher health
kensaku state                 # index counts, roots, and model ids

kensaku index pause           # pause content extraction (renames and deletes still apply)
kensaku index resume
kensaku index rescan --wait   # re-crawl watched folders
kensaku index rebuild --wait  # wipe and rebuild the index
kensaku index cancel          # stop the first-run scan
```

### Doctor

```bash
kensaku doctor                       # run every health check
kensaku doctor --fix download-ocr-pack
```

`doctor` names each check that failed and, where one exists, the `--fix` action that
repairs it. Known actions include `reindex`, `cleanup-storage`, `restart-watcher`,
`download-models`, `download-ocr-pack`, and `download-indra-pack`. An unknown action is
rejected with exit code 2.

### Packs

```bash
kensaku packs list
kensaku packs download models      # static embedding table
kensaku packs download ocr         # PP-OCRv6 pack
kensaku packs download indra       # Indra OCR pack
kensaku packs download smart       # optional smart embedding model
kensaku packs download visual      # beta visual pack (not wired into search yet)
kensaku packs download tessdata    # eng.traineddata for Tesseract
kensaku packs delete models
```

Downloads run in the background. Packs are verified against pinned SHA-256 checksums.

---

## Recents, pins, and discovery

```bash
kensaku home                         # recent files, pins, and saved searches
kensaku pin ~/Projects/roadmap.md    # pin or unpin a path
kensaku record-open ~/Projects/roadmap.md   # record an open (feeds frecency ranking)
kensaku dupes --max-distance 4       # exact duplicates and near-duplicate images
kensaku similar ~/Photos/IMG_0042.jpg       # visually similar images (needs a visual encoder)
kensaku scopes ~/Projects            # indexed folders matching a prefix
```

`similar` depends on a visual encoder that the current build does not provide, so it is
not active. See [ARCHITECTURE.md §12](ARCHITECTURE.md#12-deliberate-non-goals).

---

## Reset and removal

```bash
kensaku reset --yes                  # wipe index, database, thumbnails, and settings on next daemon start
kensaku reset --yes --remove-packs   # also delete downloaded OCR and model packs
kensaku remove-data --yes            # stop the daemon and delete the data directory now
```

`reset` requires `--yes`. The wipe runs when the daemon next starts. `remove-data` replaces
the old desktop uninstall step and is immediate.

---

## Daemon

```bash
kensaku daemon status
kensaku daemon start
kensaku daemon stop
kensaku daemon run        # foreground; useful under a service manager or for debugging
kensaku daemon log        # prints the path to daemon.log
kensaku daemon install    # start at login
kensaku daemon uninstall
```

---

## Agents and the API

| Command | Purpose |
|---|---|
| `kensaku mcp` | MCP server over stdio. See [AGENTS.md](AGENTS.md). |
| `kensaku serve [--port 8765]` | Loopback HTTP API under `/v1`, MCP Streamable HTTP, and SSE. Writes need `--allow-writes`. See [API.md](API.md). |
| `kensaku connect claude-desktop\|claude-code\|cursor [--write]` | Print the MCP config for a client, or merge it into the client's config (with a `.bak` backup). |
| `kensaku call <METHOD> '<JSON>'` | Call any API method directly. |
| `kensaku methods` | List every API method. |
| `kensaku completions bash\|zsh\|fish\|powershell` | Print shell completions. |

```bash
kensaku call search '{"query":"invoice","limit":5}'
kensaku methods
```

---

## Query language

Everything is optional. Filters never replace the text; they narrow the set.

### Filters

| Syntax | Meaning |
|---|---|
| `type:pdf` | category: `image`, `doc`, `sheet`, `presentation`, `code`, `video`, `audio`, `archive`, `dir`, or an extension |
| `ext:rs` | exact extension |
| `in:~/Projects` | scope to a folder (quote paths with spaces) |
| `path:src` | substring match on the path |
| `modified:<7d` | changed within a window: `1h`, `7d`, `30d`, `7d..30d` |
| `size:>2mb` | size range: `>`, `<`, or `a..b` (`kb`, `mb`, `gb`) |
| `is:image`, `is:dir` | aliases for common categories |
| `lang:rust` | language family of the extension |
| `name:config` | match the filename only |
| `content:timeout` | match file contents only |
| `re:"fn \\w+"` | regular expression over content + filename |
| `-node_modules` | exclude term or folder |
| `"exact phrase"` | phrase |
| `mouse near[5] button` | terms within 5 words |
| `mode:exhaustive` | return every match, path-sorted (no ranked tiers) |

Examples:

```
type:pdf invoice
in:~/Projects kensaku indexer
modified:<7d config
size:>5mb is:image sunset
name:Cargo content:workspace
-node_modules docker error
re:"TODO|FIXME" ext:rs
```

### Plain language

Kensaku compiles common phrasing locally, without a model. Every inference is
reported in the `interpretation` field of the JSON search output, so you can see which
words became filters. If the query already contains an explicit `filter:`, the compiler leaves
it alone.

| You type | It becomes |
|---|---|
| `latest resume` | `resume` + sort newest |
| `screenshots from last week` | `type:image modified:<7d` |
| `big videos in Projects` | `size:>100mb type:media in:Projects` |
| `tiny images` | `size:<1mb type:image` |
| `oldest invoices` | `invoices` + sort oldest |
| `biggest files` | `files` + sort largest |

Time words: `today`, `yesterday`, `last week`, `last month`, `last year`.
Size words: `big`/`large`/`huge` (>100 MB), `small`/`tiny` (<1 MB), and
`under 5 MB` / `over 10 MB`.

**Aliases.** Personal vocabulary lives in the `aliases` config key. The
defaults include `resume → cv`, `invoice → receipt, bill`, and
`screenshot → capture`; aliases compile to a case-insensitive OR.

### How ranking works

Three tiers run in parallel — fuzzy filename, BM25 content, and static-vector
semantic — then fuse with reciprocal rank fusion. When exact tiers leave a
shortfall, a folded/trigram fallback catches look-alikes and OCR misreads
(`ContentFolded`). On top of that: exact/prefix
name bonuses, smooth recency, frecency from files you open, and a bounded depth
penalty. Semantic-only hits below a cosine floor are dropped, so a keyword query
doesn't return vaguely related documents.

Results are deterministic: score, then path.

---

## Indexing behavior

- **Hidden entries** are skipped by default, along with common junk directories.
- **Sensitive paths and credentials** are default-denied: `.ssh`, `.gnupg`, `.aws`,
  `.kube`, `.docker`, `.password-store`, password-manager folders, `.env*`, and
  credential extensions (`.pem`, `.key`, `.pfx`, …).
- **Incremental** — unchanged files (same size + mtime) are not re-read.
- **Renames** are tracked by `(device, inode)`, so moves do not duplicate entries.
- **Startup** is search-first: the snapshot loads, then a stat-only reconcile runs
  in the background. The first `setup --wait` returns once the initial scan is done.
- **Unavailable roots** (unplugged drive, unmounted share) are preserved, not pruned.
- **Content policy** — text is extracted for files up to **100 MB**; up to
  **4 MB** of text is indexed per document, and up to **48** semantic units
  (overlapping windows) are embedded. `content_depth` defaults to `balanced`.
  Files above the size cap are still findable by name, path, type, size, and
  date. Extraction has a per-file timeout; dense images and scanned PDFs get up
  to 45 s.
- **Documents** — PDF, DOCX/PPTX/XLSX, EPUB/ODT, RTF; malformed files are skipped
  after their timeout. Scanned PDFs are OCR'd one page at a time instead of
  decoding the whole document up front.
- **Images** — OCR runs with PP-OCRv6 by default, on ONNX Runtime; the optional Indra
  engine (higher quality, ~27 MB pack) is selected with `setup --ocr-engine` or
  `config set ocr_engine`. Tesseract with language data is the fallback where ONNX
  cannot run (Intel Macs, or a build without `accurate-ocr`). Set `ocr_languages` for
  Tesseract. Thumbnails and dimensions are cached; near-duplicates use a perceptual hash,
  and screenshot bursts also compare OCR line overlap so a shared sidebar does not merge
  distinct pages.
- **Retries** — a file that fails extraction is retried at most three times and
  is not re-decoded forever. The counter resets when the file changes or the
  OCR settings change.
- **Cloud placeholders** (Windows OneDrive/iCloud, and macOS iCloud Drive via
  the APFS `SF_DATALESS` flag) are indexed by name only, so reading doesn't
  hydrate the file. Their sparse folders are listed but not descended into.
  Once a file is downloaded (OneDrive: *Always keep on this device*), the next
  crawl or file event reads its content, even though its size and date are
  unchanged. Files indexed as placeholders before 2.0.1 need one
  `kensaku index rebuild`.
- **Network drives** (Windows) — add a mapped drive (`Z:\Docs`) or a UNC path
  (`\\server\share\Docs`); both are stored as the UNC path. A drive letter is
  mapped per logon session, so a daemon running elevated or as another user
  cannot see `Z:` — use the UNC path there.
