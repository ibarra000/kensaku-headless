# FAQ

**Is anything sent to the cloud?**
No. There is no telemetry and no account, and search queries or file contents
never leave your machine. The only outbound requests are model and OCR pack downloads.
The OCR pack for the engine you selected downloads automatically when OCR is on and
the pack is missing. The static embedding table and the optional smart, and visual
packs download only when you run `kensaku packs download`.

**Why is the first run slow?**
It builds the index. `kensaku setup --wait` returns once the first scan finishes. Without
`--wait`, setup returns at once and the scan continues in the background; search works
while it runs, and `kensaku status --follow` shows progress.

**What does setup need?**
At least one folder: `kensaku setup --root <dir>`. Optional flags add name-only folders
(`--name-only`), exclusions (`--exclude`), and the OCR engine (`--ocr-engine`).
`kensaku setup suggest` lists common folders. Setup can be redone with
`kensaku setup again`, which marks setup incomplete and stops the running scan.

**Search exits with code 3. What does that mean?**
There is no index yet, or setup has not completed. Run `kensaku setup --root <dir>`. Exit
code 3 is the same for both cases; `kensaku state` shows what is configured.

**Why does search return nothing for a word I know is in a file?**
Check: is the folder watched (`kensaku roots list`)? Is the extension in the excluded list
(`kensaku config get excluded_extensions`)? Is the file over the content cap (extraction up
to 100 MB, 4 MB of text indexed)? Is it binary? Run `kensaku doctor`, and `kensaku health`
for the daemon and watcher. Use `kensaku search "..." --json` to see the `interpretation`
and `active_filters` fields, which show which filters were applied to your query.

**Does it index hidden files?**
No by default. `skip_hidden` is on; set it to `false` with `kensaku config set skip_hidden false`.

**Does it index PDFs and Office documents?**
Yes — PDF, DOCX/PPTX/XLSX, EPUB/ODT, RTF. Scanned PDFs need OCR; encrypted PDFs are
flagged, not indexed.

**Does it search images?**
By filename, and by OCR text: PP-OCRv6 by default (runs inside Kensaku on ONNX Runtime
shipped next to the binary), or the optional higher-quality Indra engine (~27 MB pack,
deskew and multi-script recognition), or Tesseract where neither can run (Intel Macs)
or if you pick it. Scanned PDFs are OCR'd too, one page at a time. Results also group
perceptual near-duplicates.

Natural-language visual search ("red cap", "system design diagram"),
similar-image lookup, and an optional local "this is me" identity are **not**
part of the search path. The `visual` pack can be downloaded, but no production visual
encoder is wired in, so `similar_images` self-hides.

**Can I just type "latest resume" or "screenshots from last week"?**
Yes. A deterministic, local intent compiler turns recency, size, type, and place phrases
into the normal filters and sort. Each inference is reported in the response's
`interpretation` field. The explicit `key:value` grammar (for example `type:pdf` and
`modified:<7d`) always works too.

**OCR is on but no image text is indexed.**
Run `kensaku packs list` to see whether the selected OCR pack is present. If it is still
downloading, wait for it. On an Intel Mac the engine is Tesseract; it needs its binary and
language data, which `kensaku packs download tessdata` fetches. Existing images are
re-scanned when OCR settings change. `kensaku doctor` repeats the check and names the
repair (`download-ocr-pack`, `download-indra-pack`) when one applies.

**Can it search across multiple drives?**
Yes. Add each root with `kensaku roots add <path>`. Unplugged drives are preserved rather
than pruned.

**How much memory does it use?**
The metadata layer keeps 128 bytes per `FileEntry` (12.2 MB measured for 100,201
entries), plus a path index and the ~7.3 MiB i8 static embedding table when the
semantic pack is installed (the f32 switch is ~30 MB). The crawl is bounded:
extracted text is written through a small queue instead of accumulating in
memory, perceptual image hashing runs in a separate background pass that decodes
one image at a time, and the Tantivy writer arena is 16 MB. On the development
machine the indexer process peaked at roughly 72–73 MB for both text-only and
mixed image/text corpora (before adding the embedding table). Measure your own
corpus with the `mem_probe` example; see [BENCHMARKS.md](BENCHMARKS.md).

**Does it conflict with Spotlight / Everything / Recoll?**
No. It builds its own index and does not read or write theirs.

**Can two daemons run for the same data directory?**
No. The daemon takes the exclusive writer lock `kensaku.lock`. A second `kensaku daemon run`
(or an `Engine` in another process) gets `AlreadyRunning` and exits. Commands from any
terminal still reach the one running daemon.

**What runs when I type a command?**
The first command starts the daemon if one is not running, and every later command talks to
it. `kensaku daemon status` shows whether it is up, `kensaku daemon log` prints the log path,
and `kensaku daemon install` starts it at login.

**Does it update itself?**
No. The binary never contacts GitHub or any update service. Download new releases from the
[releases page](https://github.com/ibarra000/kensaku-headless/releases) and check them against
`SHA256SUMS`. See [INSTALL.md](INSTALL.md#release-archives).

**Is the binary signed?**
The Windows `kensaku.exe` is Authenticode-signed through SignPath Foundation. macOS
binaries are not signed or notarised yet, so Gatekeeper blocks the first run. Clear the
quarantine flag with `xattr -d com.apple.quarantine ./kensaku`. See
[INSTALL.md](INSTALL.md#release-archives).

**Where is my data?**
`~/.config/kensaku/` on Linux, `~/Library/Application Support/kensaku/` on macOS,
`%APPDATA%\kensaku\` on Windows, or wherever `--data-dir` or `KENSAKU_DATA_DIR` points.
`kensaku config path` prints the config file. `kensaku reset --yes` wipes the index,
database, and settings on the next daemon start; `kensaku remove-data --yes` deletes the
whole directory now. Downloaded packs are kept by reset unless you add `--remove-packs`.

**Can I use it from an AI agent?**
Yes — that's the point of `kensaku mcp` and `kensaku serve`. See
[AGENTS.md](AGENTS.md) and the method reference in [API.md](API.md). The binary itself
contains no LLM.

**Why not bundle an LLM / "ask my files"?**
A local LLM is a multi-gigabyte dependency and a different product. Kensaku is
the fast, trustworthy index: plain-language intent is compiled deterministically
on-device, and there is no chat interface and no `/ask` route. Agents bring their
own model through MCP and get the same read-only index. File contents never leave
the machine during search.

**How do I contribute?**
See [`CONTRIBUTING.md`](CONTRIBUTING.md). Tests, clippy, and the smoke test must pass.
