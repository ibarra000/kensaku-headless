# Configuration reference

Kensaku stores its settings in `config.json` inside the data directory:

- Linux: `~/.config/kensaku/config.json`
- macOS: `~/Library/Application Support/kensaku/config.json`
- Windows: `%APPDATA%\kensaku\config.json`

`kensaku config path` prints the active path. Read and change keys with the CLI:

```bash
kensaku config get                   # the whole config
kensaku config get ocr_engine        # one key
kensaku config set ocr_engine tesseract
kensaku config set agent_scoped_roots '["/home/you/Projects"]'
```

`config set` parses VALUE as JSON and falls back to a plain string. An unknown key is
rejected with exit code 2. Changes are written to `config.json`.

The file is JSON. Keys are optional where noted; missing keys fall back to the
listed default. An unknown or invalid value falls back to the default instead of
stopping the daemon.

Two overrides apply to the location (config file, metadata database, content
index, OCR state, everything else under the data directory):

1. **`KENSAKU_DATA_DIR`** environment variable, or **`--data-dir <DIR>`** on any
   command. The flag sets the variable for that command and for the daemon it
   starts. Useful for tests and for Nix/Flatpak or other sandboxed packaging that
   must pin the data directory.
2. **Portable marker file.** If a file named `portable` (or `kensaku.portable`)
   sits next to the executable, Kensaku keeps everything in `<exe_dir>/data`.
   Copy the folder and keep your index.

## HTTPS certificate trust

Model and OCR downloads use the operating system's trusted CA certificates,
including corporate proxy CAs installed by your administrator. To supply a custom
trust bundle, set `SSL_CERT_FILE` to a PEM certificate bundle or `SSL_CERT_DIR` to an
OpenSSL-compatible certificate directory before starting Kensaku. If either variable
is set, only the specified locations are used instead of the system store. Include all
roots needed for your downloads in that bundle. Restart the daemon after changing these
variables. Certificate and hostname verification remain enabled; invalid or unreadable
bundles do not disable TLS verification.

## Keys

| Key | Type | Default | Meaning |
|---|---|---|---|
| `watched_dirs` | string[] | your home directory | Folders to index fully. Set with `kensaku setup --root` or `kensaku roots add`. |
| `excluded_dirs` | string[] | common build/cache names | Directory names or patterns to skip. Set with `kensaku exclude`. |
| `excluded_extensions` | string[] | media/archive/binary exts | File extensions to skip. |
| `skip_hidden` | bool | `true` | Skip dotfiles/dotdirs. |
| `max_results` | number | `50` | Stored, but not read by search. Use `search --limit` (default 10). |
| `is_setup_complete` | bool | `false` | First-run setup was configured. Set by `kensaku setup`. |
| `first_scan_done` | bool | `false` | First full crawl finished. |
| `semantic_enabled` | bool | `true` | Enable the semantic (embedding) tier. |
| `semantic_model` | string | `static` | `static`, `smart`, or `none`. |
| `semantic_f32_table` | bool | `false` | Keep the static table in f32 instead of i8. |
| `crawl_mode` | string | `balanced` | `full`, `balanced`, or `eco`. Balanced uses idle priority; eco also paces on battery. |
| `pause_indexing` | bool | `false` | Pause background crawling; renames/deletes still apply. Same as `kensaku index pause`. |
| `mcp_allow_reindex` | bool | `false` | Allow the MCP `reindex` write tool. Needs the daemon. |
| `ocr_enabled` | bool | `true` | Enable OCR for images. |
| `ocr_engine` | string | `accurate` | `accurate` (PP-OCRv6), `indra` (~27 MB pack), or `tesseract`. Set at setup with `--ocr-engine`. |
| `ocr_max_image_bytes` | number | `0` | Skip OCR above this size; `0` means no limit. |
| `ocr_languages` | string[] | `[]` | Tesseract language codes (e.g. `["eng", "hin"]`). Empty means English when installed. |
| `extractors` | object[] | `[]` | Opt-in external extractors; see [EXTRACTORS.md](EXTRACTORS.md). |
| `content_depth` | string | `balanced` | `quick`, `balanced`, or `deep` for large documents. |
| `content_max_file_size_mb` | number | `100` | Largest file that gets text extraction; above this it is name/metadata-only. |
| `content_max_text_bytes` | number | `4000000` | Maximum extracted text per document (~4 MB). |
| `content_csv_head_rows` | number | `200` | Data rows indexed per CSV/TSV/Parquet file after the header; larger files are sampled like `head`. Rows past it are not searchable by value. |
| `content_max_semantic_units` | number | `48` | Maximum embedding chunks per document. |
| `agent_scoped_roots` | string[] | `[]` | MCP/CLI/HTTP read-only scope roots. Empty means the whole index. |
| `include_ignored` | bool | `false` | Include files matched by `.gitignore` / `.cursorignore`. |
| `name_only_dirs` | string[] | `[]` | Directories (a watched root or any folder below it) indexed by file name and path only: no text extraction, OCR or semantic units. Set with `kensaku setup --name-only` or `kensaku roots add --name-only`. Changing the list reconciles the index on the next scan. |
| `aliases` | object[] | `resume → cv, curriculum vitae`; `invoice → receipt, bill`; `screenshot → screen shot, capture`; plus receipt, contract, photo | Personal vocabulary for plain-language queries. |
| `saved_searches` | object[] | `[]` | Saved queries. |
| `pins` | string[] | `[]` | Pinned file paths. Managed with `kensaku pin`. |
| `config_version` | number | internal | Schema version used for one-time migrations. Do not edit. |

## Legacy keys (stored, not read)

These keys came from the desktop application. The headless build still loads and saves
them, so an existing `config.json` keeps working, but no code path reads them. Setting one
changes the file and nothing else.

| Key | Former meaning |
|---|---|
| `shortcut` | Global toggle shortcut (default `Alt+Space`). |
| `app_mode` | `launcher` or `app` window mode. |
| `close_to_background` | Keep the tray app running when the window closed. |
| `theme` | `light` or `dark` UI theme. |
| `language` | UI language code. |
| `results_view` | `grid` or `list` result layout. |
| `show_thumbnails` | Thumbnails on result cards. |
| `auto_update` | Background update checks. The headless build has no updater. |
| `visual_search_beta_enabled` | Showed the beta visual-pack download in Settings. |

Remove them from the file if you want a clean config. Nothing else depends on them.

## Example

```json
{
  "watched_dirs": ["/home/you/Projects", "/home/you/Documents"],
  "excluded_dirs": ["node_modules", ".git", "target", "dist"],
  "excluded_extensions": ["iso", "dmg", "vmdk"],
  "crawl_mode": "balanced",
  "semantic_enabled": true,
  "semantic_model": "static",
  "content_depth": "balanced",
  "content_max_file_size_mb": 100,
  "content_max_text_bytes": 4000000,
  "content_max_semantic_units": 48,
  "ocr_enabled": true,
  "ocr_engine": "accurate",
  "ocr_languages": ["eng"],
  "agent_scoped_roots": [],
  "mcp_allow_reindex": false
}
```

## Migrations and rollback

`config_version` drives one-time migrations at load (for example, the old
`paper`/`void` theme names and the 20 MB/1 MB/20-unit content caps). A config
written by an older release keeps working; new keys get their defaults. If you
hand-edit a key and want to undo it, delete the key and restart the daemon.

The index has its own schema version. A content-schema upgrade keeps the old
index as `index.legacy-*`, creates a fresh one, and queues every file for one
content re-extraction. The old copy is safe to delete after the rebuild.

## What is not in the config

- Watched-folder exclusions are also enforced by `.gitignore` /
  `.cursorignore` unless `include_ignored` is true.
- Sensitive paths and credential extensions are denied in code, not via this
  file.
- Model and OCR packs are verified against pinned SHA-256 checksums; there is no
  setting to disable that.
