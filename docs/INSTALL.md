# Install & build

## Release archives

Download from [Releases](https://github.com/ibarra000/kensaku-headless/releases). Each target has
one archive containing the `kensaku` binary, the ONNX Runtime library used by OCR,
and the LICENSE and README.

| Target | Asset | Notes |
|---|---|---|
| Linux x86_64 | `kensaku-<ver>-x86_64-unknown-linux-gnu.tar.gz` | Bundles `libonnxruntime.so`. |
| macOS Apple silicon | `kensaku-<ver>-aarch64-apple-darwin.tar.gz` | Bundles `libonnxruntime.dylib`. |
| macOS Intel | `kensaku-<ver>-x86_64-apple-darwin.tar.gz` | No bundled ONNX Runtime. OCR falls back to Tesseract. |
| Windows x86_64 | `kensaku-<ver>-x86_64-pc-windows-msvc.zip` | Contains `kensaku.exe` and `onnxruntime.dll`. |
| Windows ARM64 | `kensaku-<ver>-aarch64-pc-windows-msvc.zip` | The ONNX Runtime DLL matches the process architecture. |

Unpack the archive into a folder you control and put the binary on your `PATH`.
The rest of this page uses `kensaku` for the binary.

### Verifying a download

Every release ships `SHA256SUMS`. Check the archive against it before you run it:

```bash
sha256sum -c SHA256SUMS --ignore-missing     # Linux, macOS (or: shasum -a 256 -c)
```

```powershell
Get-FileHash .\kensaku-<ver>-x86_64-pc-windows-msvc.zip -Algorithm SHA256
```

**Windows.** `kensaku.exe` is not code-signed yet, so SmartScreen may warn on first
run. Choose **More info → Run anyway**, or unblock the file with `Unblock-File .\kensaku.exe`.

**macOS.** Binaries are not signed or notarised yet, so Gatekeeper blocks the first
run. Clear the quarantine flag on the unpacked binary:

```bash
xattr -d com.apple.quarantine ./kensaku
```

**Linux.** No signing step. `SHA256SUMS` is the integrity check.

Signing is planned; see [SIGNING.md](SIGNING.md).

---

## Build from source

### Prerequisites

- Rust 1.98. The workspace sets `rust-version = "1.98"`, and CI uses `dtolnay/rust-toolchain@1.98`.

  ```bash
  rustup toolchain install 1.98
  ```

- A C/C++ toolchain for the native dependencies. On Windows that is the Visual Studio
  Build Tools with the C++ workload. On macOS, `xcode-select --install`.

### Build

```bash
git clone https://github.com/ibarra000/kensaku-headless
cd kensaku
cargo +1.98 build --release -p kensaku
```

The binary is written to `target/release/kensaku` (`kensaku.exe` on Windows).

To put it on your `PATH` on Linux or macOS:

```bash
install -m 755 target/release/kensaku ~/.local/bin/kensaku
```

On Windows, copy `target\release\kensaku.exe` into a folder on your `PATH`.

The `accurate-ocr` feature (on by default) loads ONNX Runtime at run time (`ort` with
`load-dynamic`), so a source build does not bundle it. Put the runtime library next to
`kensaku` (the release archives do this) or point `ORT_DYLIB_PATH` at it. Without the
library, the PP-OCR path is unavailable and OCR falls back to Tesseract. `kensaku doctor`
reports it.

### Makefile tasks

```bash
make            # list tasks
make build      # release build of the kensaku binary
make ort        # fetch ONNX Runtime (OCR) next to it, from NuGet
make test       # cargo test --all-targets
make lint       # clippy with warnings as errors
make fmt        # cargo fmt --all
make smoke      # end-to-end headless smoke test (throwaway data dir)
make bench BENCH_DIR=~/Projects   # filename-search benchmark
make clean      # remove build outputs (not your data)
```

`tools/headless-smoke.sh` runs against a temporary data directory. It starts its own
daemon, runs setup and searches, and stops the daemon on exit. It never touches your
real index.

---

## Data directory

Everything Kensaku stores lives in one data directory:

| OS | Default path |
|---|---|
| Linux | `~/.config/kensaku/` |
| macOS | `~/Library/Application Support/kensaku/` |
| Windows | `%APPDATA%\kensaku\` |

Override it with `--data-dir <DIR>` on any command, or with the `KENSAKU_DATA_DIR`
environment variable. If a file named `portable` (or `kensaku.portable`) sits next to
the executable, the data directory is `<exe_dir>/data`. Print the active config file
path with `kensaku config path`.

Contents are listed in [ARCHITECTURE.md §2](ARCHITECTURE.md#2-on-disk-layout).

---

## The daemon

The index has one owner: a background process started with `kensaku daemon run`. It holds
the exclusive writer lock (`kensaku.lock`) on every OS, including Windows. Every other
`kensaku` command talks to it and starts it on demand if it is not running.

```bash
kensaku daemon status       # whether the daemon is running
kensaku daemon start        # start in the background if not running
kensaku daemon stop
kensaku daemon log          # print the log path (daemon.log in the data directory)
kensaku daemon run          # run in the foreground (what start launches)
```

Start at login:

```bash
kensaku daemon install      # register the daemon to start at login
kensaku daemon uninstall
```

| OS | Registration |
|---|---|
| Windows | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value `KensakuDaemon` (no UAC) |
| Linux | systemd user unit `~/.config/systemd/user/kensaku.service` |
| macOS | LaunchAgent `~/Library/LaunchAgents/dev.kensaku.daemon.plist` (label `dev.kensaku.daemon`) |

The daemon endpoint is per data directory: a Unix socket under `$XDG_RUNTIME_DIR` (or in the
data directory), or a named pipe `kensaku-<hash>` on Windows. Requests carry the token in
`daemon.token`.

If you upgrade from a build that predates protocol v2, stop the old daemon first
(`kensaku daemon stop`, or end the old `kensaku-daemon` process). A client and daemon
with different protocol versions refuse each other.

---

## OCR

OCR works out of the box. The PP-OCRv6 engine (`accurate`) is compiled into normal
builds, and its ~6.1 MB model pack is downloaded when OCR is enabled and the pack is
missing. Release archives bundle ONNX Runtime for Windows, Apple silicon, and Linux x64.
Source builds need the library too: `make ort` (or `tools/fetch-onnxruntime.sh`) downloads the
[`Microsoft.ML.OnnxRuntime`](https://www.nuget.org/packages/Microsoft.ML.OnnxRuntime/1.31.0)
NuGet package, checks its SHA-256 and puts the library next to `target/release/kensaku`.
Elsewhere, point `ORT_DYLIB_PATH` at it.

Pick the engine at setup, or change it later with configuration:

```bash
kensaku setup --root ~/Documents --ocr-engine indra
kensaku config set ocr_engine indra
```

| Engine | Value | Download | Notes |
|---|---|---|---|
| PP-OCRv6 | `accurate` | ~6.1 MB (`kensaku packs download ocr`) | Default. Runs in-process on ONNX Runtime. |
| Indra | `indra` | ~27 MB (`kensaku packs download indra`) | Higher quality: deskew, orientation, multi-script routing. |
| Tesseract | `tesseract` | System binary plus language data | Fallback where ONNX cannot run (Intel Macs, `--no-default-features`). |

Packs are verified against pinned SHA-256 checksums. If the selected PP-OCR or Indra pack is
missing, extraction falls back to Tesseract. `kensaku doctor` reports the OCR state.

Tesseract needs the binary **and** language data. `kensaku packs download tessdata`
fetches `eng.traineddata` (~4 MB) into the data directory, so no root access is needed.
Use `kensaku config set ocr_languages '["eng","hin"]'` to choose languages. A system
install also works:

```bash
# Linux (Debian/Ubuntu)
sudo apt install tesseract-ocr tesseract-ocr-eng

# macOS
brew install tesseract

# Windows
winget install UB-Mannheim.TesseractOCR
```

Models: PaddleOCR PP-OCRv6 tiny (Apache-2.0), published by PaddlePaddle; Indra
weights by Datalab (indra-ocr), under a modified OpenRAIL-M license. The Rust
ONNX wrapper is `paddleocr_rs_onnx` (MIT).

---

## Semantic packs

The static embedding table used by the meaning tier is a separate download:

```bash
kensaku packs download models   # static embedding table (background)
kensaku packs download smart    # optional smart embedding model (needs the smart-embeddings build feature)
kensaku packs download visual   # beta visual-search pack; not wired into search yet
kensaku packs list              # what is present, and sizes
kensaku packs delete models     # the only pack that can be deleted
```

Downloads run in the background. Check progress with `kensaku packs list`.

---

## First run

The first run needs at least one folder to index:

```bash
kensaku setup --root ~/Documents --root ~/Projects --wait
```

| Flag | Meaning |
|---|---|
| `--root <DIR>` | Index a folder fully: names and content. Repeatable. |
| `--name-only <DIR>` | Index a folder by name and path only. No text, OCR, or embeddings. Repeatable. |
| `--exclude <PATTERN>` | Skip matching folders or paths. Repeatable. |
| `--ocr-engine <ENGINE>` | `tesseract`, `accurate`, or `indra`. |
| `--wait` | Block until the first scan finishes. Without it, setup returns and the scan continues. |

Other setup commands:

```bash
kensaku setup suggest       # list common folders worth indexing (Documents, Downloads, ...)
kensaku setup again         # mark setup incomplete and stop the running scan
kensaku index cancel        # stop the first-run scan
```

Search before setup has been completed exits with code 3. `kensaku status --follow` shows
scan progress.

---

## Reset and removal

`kensaku reset --yes` deletes the index, metadata database, thumbnails, and settings. The
daemon applies the wipe on its next start. Downloaded packs are kept unless you add
`--remove-packs`.

`kensaku remove-data --yes` stops the daemon and deletes the whole data directory now.

To uninstall, remove the start-at-login entry, delete the data, then delete the binary:

```bash
kensaku daemon uninstall
kensaku remove-data --yes
rm ~/.local/bin/kensaku         # or delete the unpacked folder on Windows
```

Nothing else is written outside the data directory and the start-at-login entry.
