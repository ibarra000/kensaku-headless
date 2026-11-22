use image::{DynamicImage, ImageReader, Limits};
use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Max decodable image size (50 MP); larger images are rejected before allocation.
pub const MAX_IMAGE_PIXELS: u64 = 50_000_000;

static THUMB_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Process-wide OCR image size cap in bytes (0 = no limit).
static OCR_MAX_IMAGE_BYTES: AtomicU64 = AtomicU64::new(0);

pub fn set_ocr_max_image_bytes(bytes: u64) {
    OCR_MAX_IMAGE_BYTES.store(bytes, Ordering::SeqCst);
}

pub fn ocr_max_image_bytes() -> u64 {
    OCR_MAX_IMAGE_BYTES.load(Ordering::SeqCst)
}

/// Longest side of every generated thumbnail and preview.
pub const THUMB_SIZE: u32 = 240;

/// Raster formats we decode cheaply (HEIC/HEIF and SVG deliberately excluded).
pub fn is_supported_image_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif"
    )
}

/// Snapshot of the local OCR engine and its language data.
#[derive(Debug, Clone, Serialize)]
pub struct OcrStatus {
    pub engine_present: bool,
    pub languages: Vec<String>,
    pub available: bool,
    pub tessdata_dir: String,
    pub error: Option<String>,
    /// Selected backend (`tesseract`, `accurate` or `indra`).
    pub engine: String,
    /// PP-OCR is compiled in, has its pack, loaded, and passed its self-test.
    pub accurate_available: bool,
    pub accurate_compiled: bool,
    pub accurate_problem: Option<String>,
    /// Indra is compiled in, has its pack, loaded, and passed its self-test.
    pub indra_available: bool,
    pub indra_problem: Option<String>,
    /// Engine actually reading now (`accurate`, `indra`, `tesseract`, `none`); may differ from `engine`.
    pub active_engine: String,
}

/// Kensaku's tessdata directory; created lazily by [`download_eng_tessdata`].
pub fn tessdata_dir() -> PathBuf {
    crate::config::KensakuConfig::config_dir().join("tessdata")
}

fn build_status(
    engine_present: bool,
    languages: Vec<String>,
    tessdata_dir: PathBuf,
    error: Option<String>,
) -> OcrStatus {
    OcrStatus {
        available: engine_present && !languages.is_empty(),
        engine_present,
        languages,
        tessdata_dir: tessdata_dir.to_string_lossy().to_string(),
        error,
        engine: crate::ocr::engine_name().to_string(),
        accurate_available: crate::ocr::accurate_available(),
        accurate_compiled: crate::ocr::accurate_compiled(),
        accurate_problem: crate::ocr::accurate_problem(),
        indra_available: crate::ocr_indra::available(),
        indra_problem: crate::ocr_indra::problem(),
        active_engine: crate::ocr::usable_engine().to_string(),
    }
}

fn last_non_empty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_string)
}

/// Probed once: spawning `tesseract --list-langs` per image doubles crawl process count.
static TESSERACT_LANGUAGES_CACHE: std::sync::Mutex<Option<Vec<String>>> =
    std::sync::Mutex::new(None);

fn installed_tesseract_languages() -> Vec<String> {
    if let Ok(slot) = TESSERACT_LANGUAGES_CACHE.lock()
        && let Some(languages) = slot.as_ref()
    {
        return languages.clone();
    }

    let mut command = tesseract_command();
    command.arg("--list-langs");
    if let Some(dir) = private_tessdata(&tessdata_dir()) {
        command.env("TESSDATA_PREFIX", dir);
    }
    let languages = match command.output() {
        Ok(output) if output.status.success() => {
            let mut langs = parse_langs(&String::from_utf8_lossy(&output.stdout));
            if langs.is_empty() {
                // Some builds print the list on stderr.
                langs = parse_langs(&String::from_utf8_lossy(&output.stderr));
            }
            langs
        }
        _ => Vec::new(),
    };

    if let Ok(mut slot) = TESSERACT_LANGUAGES_CACHE.lock() {
        *slot = Some(languages.clone());
    }
    languages
}

/// Parses `tesseract --list-langs` output (header, blanks, CRLF, dedup; order kept).
fn parse_langs(raw: &str) -> Vec<String> {
    let mut languages: Vec<String> = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty()
            || line
                .to_ascii_lowercase()
                .starts_with("list of available languages")
        {
            continue;
        }
        if !languages.iter().any(|existing| existing.as_str() == line) {
            languages.push(line.to_string());
        }
    }
    languages
}

/// `dir` if it has a `*.traineddata`, so private data wins without overriding a working install.
fn private_tessdata(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("traineddata") && path.is_file() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Resolves tesseract: `KENSAKU_TESSERACT`, bundled, `PATH`, then well-known prefixes (inherited `PATH` may be short).
fn tesseract_program() -> PathBuf {
    if let Some(path) = std::env::var_os("KENSAKU_TESSERACT") {
        return PathBuf::from(path);
    }

    let name = if cfg!(windows) {
        "tesseract.exe"
    } else {
        "tesseract"
    };

    // A bundled copy wins so OCR behaves the same everywhere.
    for candidate in bundled_tesseract_paths(name) {
        if candidate.is_file() {
            return candidate;
        }
    }

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    for candidate in fallback_tesseract_paths() {
        if candidate.is_file() {
            return candidate;
        }
    }

    PathBuf::from(name)
}

fn bundled_tesseract_paths(name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(exe) = std::env::current_exe() else {
        return paths;
    };
    if let Some(dir) = exe.parent() {
        paths.push(dir.join(name));
        paths.push(dir.join("tesseract").join(name));
        // macOS: Contents/MacOS/kensaku -> Contents/Resources
        if let Some(contents) = dir.parent() {
            paths.push(contents.join("Resources").join("tesseract").join(name));
        }
    }
    paths
}

fn fallback_tesseract_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        for env in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(env) {
                paths.push(
                    PathBuf::from(base)
                        .join("Tesseract-OCR")
                        .join("tesseract.exe"),
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        for dir in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/opt/local/bin",
            "/usr/bin",
        ] {
            paths.push(PathBuf::from(dir).join("tesseract"));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for dir in [
            "/usr/bin",
            "/usr/local/bin",
            "/bin",
            "/snap/bin",
            "/run/current-system/sw/bin",
        ] {
            paths.push(PathBuf::from(dir).join("tesseract"));
        }
    }

    paths
}

fn tesseract_command() -> Command {
    let prog = tesseract_program();
    let mut command = Command::new(&prog);
    // AppImage's LD_LIBRARY_PATH breaks host binaries like /usr/bin/tesseract.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(orig) = std::env::var("LD_LIBRARY_PATH_ORIG") {
            command.env("LD_LIBRARY_PATH", orig);
        } else if let Ok(appdir) = std::env::var("APPDIR")
            && let Ok(current) = std::env::var("LD_LIBRARY_PATH")
        {
            let cleaned: Vec<_> = std::env::split_paths(&current)
                .filter(|p| !p.starts_with(&appdir))
                .collect();
            if cleaned.is_empty() {
                command.env_remove("LD_LIBRARY_PATH");
            } else if let Ok(joined) = std::env::join_paths(cleaned) {
                command.env("LD_LIBRARY_PATH", joined);
            }
        }
    }
    // OpenMP spawns a thread per core per process; the worker pool already parallelizes.
    command.env("OMP_THREAD_LIMIT", "1");
    command.env("OMP_NUM_THREADS", "1");
    crate::util::process::hide_console(&mut command);
    command
}

/// True when `tesseract --version` runs (cheaper than [`ocr_status`]).
pub fn tesseract_available() -> bool {
    tesseract_command()
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Inspects the tesseract binary and its language data. Never panics.
pub fn ocr_status() -> OcrStatus {
    // Doubles as "re-check": pick up a freshly installed Tesseract.
    reset_tesseract_probe();
    let dir = tessdata_dir();

    // Some builds print version information to stderr.
    let (engine_present, mut error) = match tesseract_command().arg("--version").output() {
        Ok(output) if output.status.success() => (true, None),
        Ok(output) => (
            false,
            last_non_empty_line(&output.stderr).or_else(|| last_non_empty_line(&output.stdout)),
        ),
        Err(e) => (false, Some(e.to_string())),
    };

    let mut languages = Vec::new();
    let mut list = tesseract_command();
    list.arg("--list-langs");
    // Private tessdata takes precedence, mirroring `extract_image_ocr`.
    if dir.is_dir() {
        list.env("TESSDATA_PREFIX", &dir);
    }

    match list.output() {
        Ok(output) if output.status.success() => {
            languages = parse_langs(&String::from_utf8_lossy(&output.stdout));
            if languages.is_empty() {
                languages = parse_langs(&String::from_utf8_lossy(&output.stderr));
            }
        }
        Ok(output) => {
            if error.is_none() {
                error = last_non_empty_line(&output.stderr)
                    .or_else(|| last_non_empty_line(&output.stdout));
            }
        }
        Err(e) => {
            if error.is_none() {
                error = Some(e.to_string());
            }
        }
    }

    build_status(engine_present, languages, dir, error)
}

/// Downloads `eng.traineddata` (reusing one >1 MB); writes to `.part` and renames after a size check.
pub fn download_eng_tessdata() -> Result<PathBuf, String> {
    download_eng_tessdata_to(&tessdata_dir())
}

fn download_eng_tessdata_to(dir: &Path) -> Result<PathBuf, String> {
    const URL: &str = "https://github.com/tesseract-ocr/tessdata_fast/raw/main/eng.traineddata";
    const MIN_BYTES: u64 = 1_000_000;

    let target = dir.join("eng.traineddata");
    if let Ok(metadata) = std::fs::metadata(&target)
        && metadata.is_file()
        && metadata.len() > MIN_BYTES
    {
        return Ok(target);
    }

    std::fs::create_dir_all(dir).map_err(|e| {
        format!(
            "Failed to create tessdata directory {}: {}",
            dir.display(),
            e
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Failed to secure {}: {}", dir.display(), e))?;
    }

    let part = dir.join("eng.traineddata.part");
    let _ = std::fs::remove_file(&part);

    let download = (|| -> Result<u64, String> {
        let response = match ureq::get(URL).timeout(Duration::from_secs(600)).call() {
            Ok(response) => response,
            Err(ureq::Error::Status(code, _)) => {
                return Err(format!("Download failed: HTTP {}", code));
            }
            Err(e) => return Err(format!("Download failed: {}", e)),
        };
        let status = response.status();
        let mut reader = response.into_reader();
        let mut file = File::create(&part)
            .map_err(|e| format!("Failed to create {}: {}", part.display(), e))?;

        let written = std::io::copy(&mut reader, &mut file)
            .map_err(|e| format!("HTTP {}: download interrupted: {}", status, e))?;
        file.flush()
            .map_err(|e| format!("Failed to flush {}: {}", part.display(), e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to sync {}: {}", part.display(), e))?;
        Ok(written)
    })();

    let written = match download {
        Ok(written) => written,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };

    if written <= MIN_BYTES {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "Downloaded eng.traineddata is too small ({} bytes; expected more than {})",
            written, MIN_BYTES
        ));
    }

    if let Err(first) = std::fs::rename(&part, &target) {
        // Windows refuses to replace an existing file.
        let _ = std::fs::remove_file(&target);
        if let Err(second) = std::fs::rename(&part, &target) {
            let _ = std::fs::remove_file(&part);
            return Err(format!(
                "Failed to finalize {}: {} (initial rename: {})",
                target.display(),
                second,
                first
            ));
        }
    }

    Ok(target)
}

/// Cached [`tesseract_available`] (0 unknown, 1 yes, 2 no); avoids a spawn per image.
static TESSERACT_PROBE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn tesseract_usable() -> bool {
    match TESSERACT_PROBE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let usable = tesseract_has_language();
            TESSERACT_PROBE.store(if usable { 1 } else { 2 }, Ordering::Relaxed);
            usable
        }
    }
}

/// Tesseract runs *and* has a language; without data it fails on every image.
fn tesseract_has_language() -> bool {
    let mut list = tesseract_command();
    list.arg("--list-langs");
    if let Some(dir) = private_tessdata(&tessdata_dir()) {
        list.env("TESSDATA_PREFIX", dir);
    }
    match list.output() {
        Ok(output) if output.status.success() => {
            !parse_langs(&String::from_utf8_lossy(&output.stdout)).is_empty()
                || !parse_langs(&String::from_utf8_lossy(&output.stderr)).is_empty()
        }
        Ok(output) => {
            let err = String::from_utf8_lossy(&output.stderr);
            if !err.trim().is_empty() {
                eprintln!("[Kensaku OCR] tesseract --list-langs failed: {}", err.trim());
            }
            false
        }
        Err(e) => {
            eprintln!("[Kensaku OCR] failed to execute tesseract: {e}");
            false
        }
    }
}

/// Forgets the cached Tesseract probe.
pub fn reset_tesseract_probe() {
    TESSERACT_PROBE.store(0, Ordering::Relaxed);
    if let Ok(mut slot) = TESSERACT_LANGUAGES_CACHE.lock() {
        *slot = None;
    }
}

/// OCRs an image with whichever engine can run (`Unavailable` = retry later, `NoText` = final).
pub fn ocr_image(path: &Path) -> crate::ocr::OcrOutcome {
    use crate::ocr::{ACCURATE, INDRA, OcrOutcome, TESSERACT};

    // Skipping over the cap is final; changing the cap re-queues via the OCR signature.
    let cap = ocr_max_image_bytes();
    if cap > 0
        && let Ok(meta) = std::fs::metadata(path)
        && meta.len() > cap
    {
        return OcrOutcome::NoText;
    }

    match crate::ocr::usable_engine() {
        INDRA => match crate::ocr_indra::extract_image(path) {
            OcrOutcome::Unavailable if crate::ocr::accurate_available() => {
                crate::ocr::extract_image_ocr(path)
            }
            OcrOutcome::Unavailable if tesseract_usable() => tesseract_ocr(path),
            outcome => outcome,
        },
        ACCURATE => match crate::ocr::extract_image_ocr(path) {
            OcrOutcome::Unavailable if tesseract_usable() => tesseract_ocr(path),
            outcome => outcome,
        },
        TESSERACT => tesseract_ocr(path),
        _ => OcrOutcome::Unavailable,
    }
}

pub fn ocr_decoded_image(image: &DynamicImage) -> crate::ocr::OcrOutcome {
    use crate::ocr::{ACCURATE, INDRA, OcrOutcome, TESSERACT};

    let via_tesseract = |image: &DynamicImage| {
        // Tesseract needs a file: unique temp PNG, removed right after.
        let tmp = std::env::temp_dir().join(format!(
            "kensaku-ocr-{}-{}.png",
            std::process::id(),
            THUMB_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if image
            .save_with_format(&tmp, image::ImageFormat::Png)
            .is_err()
        {
            return OcrOutcome::NoText;
        }
        let outcome = tesseract_ocr(&tmp);
        let _ = std::fs::remove_file(&tmp);
        outcome
    };
    match crate::ocr::usable_engine() {
        INDRA => match crate::ocr_indra::extract_decoded(image) {
            OcrOutcome::Unavailable if crate::ocr::accurate_available() => {
                crate::ocr::extract_decoded_image(image)
            }
            OcrOutcome::Unavailable if tesseract_usable() => via_tesseract(image),
            outcome => outcome,
        },
        ACCURATE => match crate::ocr::extract_decoded_image(image) {
            OcrOutcome::Unavailable if tesseract_usable() => via_tesseract(image),
            outcome => outcome,
        },
        TESSERACT => via_tesseract(image),
        _ => OcrOutcome::Unavailable,
    }
}

/// OCR text, or `None` for no text or no engine (use [`ocr_image`] to distinguish).
pub fn extract_image_ocr(path: &Path) -> Option<String> {
    match ocr_image(path) {
        crate::ocr::OcrOutcome::Text(text) => Some(text),
        _ => None,
    }
}

/// Tesseract `-l` argument (e.g. `eng+hin`), limited to installed languages (unknown ones fail the run).
fn tesseract_lang_arg() -> Option<String> {
    let available = installed_tesseract_languages();

    let configured = crate::indexer::extract::ocr_languages();
    let mut chosen: Vec<String> = configured
        .iter()
        .map(|lang| lang.trim().to_ascii_lowercase())
        .filter(|lang| !lang.is_empty())
        .filter(|lang| available.iter().any(|have| have.eq_ignore_ascii_case(lang)))
        .collect();

    if chosen.is_empty() {
        // Fallback: English if installed, else the single available language.
        if available.iter().any(|lang| lang.eq_ignore_ascii_case("eng")) {
            chosen.push("eng".to_string());
        } else if available.len() == 1 {
            chosen.push(available[0].clone());
        }
    }

    if chosen.is_empty() {
        return None;
    }
    chosen.dedup();
    Some(chosen.join("+"))
}

fn tesseract_ocr(path: &Path) -> crate::ocr::OcrOutcome {
    use crate::ocr::OcrOutcome;
    // macOS: pipe bytes via stdin so tesseract never opens files in TCC-protected
    // folders (a prompt per file); other platforms pass the path.
    let stdin_bytes = if cfg!(target_os = "macos") {
        match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(_) => return OcrOutcome::NoText,
        }
    } else {
        None
    };

    let mut command = tesseract_command();
    if stdin_bytes.is_some() {
        command.arg("stdin").stdin(Stdio::piped());
    } else {
        command.arg(path).stdin(Stdio::null());
    }
    command
        .args(["stdout", "--psm", "3"]) // Fully automatic page segmentation
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    // Without `-l`, Tesseract reads everything as English.
    if let Some(langs) = tesseract_lang_arg() {
        command.arg("-l").arg(langs);
    }

    // Prefer private language data, but never override a working system install.
    if let Some(dir) = private_tessdata(&tessdata_dir()) {
        command.env("TESSDATA_PREFIX", dir);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            // Forget the cached probe so the next status check is accurate.
            eprintln!("[Kensaku OCR] could not start tesseract: {e}");
            reset_tesseract_probe();
            return OcrOutcome::Unavailable;
        }
    };
    if let (Some(bytes), Some(mut stdin)) = (stdin_bytes, child.stdin.take()) {
        // Write on a thread to avoid pipe-buffer deadlock; dropping stdin sends EOF.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let Ok(output) = child.wait_with_output() else {
        return OcrOutcome::NoText;
    };

    if !output.status.success() {
        return OcrOutcome::NoText;
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        OcrOutcome::NoText
    } else {
        OcrOutcome::Text(text)
    }
}

/// Image dimensions without decoding; `None` if unreadable or over [`MAX_IMAGE_PIXELS`].
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let (width, height) = image::image_dimensions(path).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return None;
    }
    Some((width, height))
}

/// Alias for [`dimensions`].
pub fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    dimensions(path)
}

/// Decodes for OCR/embedding, reserving decode budget before allocating; hold the permit while in use.
pub(crate) fn decode_for_recognition(
    path: &Path,
) -> Option<(DynamicImage, crate::util::budget::Permit)> {
    let (width, height) = dimensions(path)?;
    let permit = crate::util::budget::decode_budget()
        .acquire(crate::util::budget::rgba_bytes(width, height));
    let image = decode_guarded(path)?;
    Some((image, permit))
}

/// Decodes with the 50 MP budget enforced via decoder limits.
pub(crate) fn decode_guarded(path: &Path) -> Option<DynamicImage> {
    let (width, height) = dimensions(path)?;

    let mut reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 5);
    reader.limits(limits);

    reader.decode().ok()
}

/// 64-bit dHash (9x8 grayscale); `None` on decode failure or over budget.
pub fn phash64(path: &Path) -> Option<u64> {
    hash_image(&decode_guarded(path)?)
}

fn hash_image(image: &DynamicImage) -> Option<u64> {
    let small = image
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .into_luma8();

    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let left = small.get_pixel(x, y)[0];
            let right = small.get_pixel(x + 1, y)[0];
            hash <<= 1;
            if left > right {
                hash |= 1;
            }
        }
    }
    Some(hash)
}

pub fn phash64_from_thumbnail(thumb: &Path) -> Option<u64> {
    let image = image::open(thumb).ok()?;
    hash_image(&image)
}

/// Hashes an existing fresh thumbnail; never decodes the source.
pub fn phash64_reusing_fresh_thumbnail(path: &Path, thumbs_dir: &Path) -> Option<u64> {
    let thumb = thumbnail_path(path, thumbs_dir);
    if is_fresh(&thumb, path) {
        phash64_from_thumbnail(&thumb)
    } else {
        None
    }
}

/// Perceptual hash via a fresh thumbnail, else decodes once and writes the thumbnail.
pub fn phash64_cached(path: &Path, thumbs_dir: &Path) -> Option<u64> {
    let thumb = thumbnail(path, thumbs_dir)?;
    phash64_from_thumbnail(&thumb)
}

pub fn thumbnail_path(path: &Path, out_dir: &Path) -> PathBuf {
    let hash = fnv1a(path.to_string_lossy().as_bytes());
    out_dir.join(format!("{:016x}.jpg", hash))
}

/// Generates (or reuses a fresh) 240px JPEG thumbnail, written atomically via temp + rename.
pub fn thumbnail(path: &Path, out_dir: &Path) -> Option<PathBuf> {
    // Decoding a cloud-only file downloads it; thumbnails are opportunistic.
    if crate::indexer::walker::is_cloud_placeholder_path(path) {
        return None;
    }
    let target = thumbnail_path(path, out_dir);
    if is_fresh(&target, path) {
        return Some(target);
    }

    let image = decode_guarded(path)?;
    std::fs::create_dir_all(out_dir).ok()?;

    let temp = out_dir.join(format!(
        ".thumb-{}-{}.tmp",
        std::process::id(),
        THUMB_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| -> Option<()> {
        let rgb = image.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgb8();
        let mut writer = BufWriter::new(File::create(&temp).ok()?);
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 80);
        encoder
            .encode(
                &rgb,
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .ok()?;
        writer.flush().ok()?;
        Some(())
    })();

    if result.is_none() {
        let _ = std::fs::remove_file(&temp);
        return None;
    }

    // Windows `rename` won't overwrite: remove the stale file once and retry.
    if std::fs::rename(&temp, &target).is_err() {
        let _ = std::fs::remove_file(&target);
        if std::fs::rename(&temp, &target).is_err() {
            let _ = std::fs::remove_file(&temp);
            return None;
        }
    }

    Some(target)
}

fn is_fresh(target: &Path, source: &Path) -> bool {
    let Ok(target_meta) = std::fs::metadata(target) else {
        return false;
    };
    let Ok(source_meta) = std::fs::metadata(source) else {
        return false;
    };
    // Only thumbnails at the current size policy are reusable.
    let Ok((target_width, target_height)) = image::image_dimensions(target) else {
        return false;
    };
    if target_width.max(target_height) != THUMB_SIZE {
        return false;
    }
    let Ok(_) = image::image_dimensions(source) else {
        return false;
    };
    match (target_meta.modified(), source_meta.modified()) {
        (Ok(target_time), Ok(source_time)) => target_time >= source_time,
        _ => false,
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};
    use std::time::Duration;

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kensaku-image-test-{}-{}-{}",
                tag,
                std::process::id(),
                seq
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn save(image: &RgbImage, path: &Path) {
        image
            .save_with_format(path, ImageFormat::Png)
            .expect("write test image");
    }

    fn solid(path: &Path, color: [u8; 3]) {
        save(&RgbImage::from_pixel(64, 48, Rgb(color)), path);
    }

    /// Horizontal brightness ramp; `invert` flips every dHash comparison.
    fn gradient(path: &Path, invert: bool) {
        let image = RgbImage::from_fn(64, 48, |x, _| {
            let value = ((x * 255) / 63) as u8;
            let value = if invert { 255 - value } else { value };
            Rgb([value, value, value])
        });
        save(&image, path);
    }

    #[test]
    fn dimensions_reads_generated_png() {
        let dir = TempDir::new("dimensions");
        let path = dir.join("small.png");
        save(&RgbImage::from_pixel(10, 7, Rgb([1, 2, 3])), &path);

        assert_eq!(dimensions(&path), Some((10, 7)));
    }

    #[test]
    fn recognition_decode_reserves_exactly_the_decoded_size() {
        // Reservation equals the page's RGBA size and lives as long as the permit.
        let dir = TempDir::new("reserve");
        let path = dir.join("page.png");
        save(&RgbImage::from_pixel(64, 48, Rgb([9, 9, 9])), &path);

        let (image, permit) = decode_for_recognition(&path).expect("decodes");
        assert_eq!((image.width(), image.height()), (64, 48));
        assert_eq!(permit.bytes(), crate::util::budget::rgba_bytes(64, 48));
        drop(permit);
    }

    #[test]
    fn recognition_decode_refuses_corrupt_files_without_reserving() {
        let dir = TempDir::new("reserve-corrupt");
        let path = dir.join("broken.png");
        std::fs::write(&path, b"not an image").expect("write");
        assert!(decode_for_recognition(&path).is_none());
    }

    #[test]
    fn dimensions_rejects_corrupt_and_zero_byte_files() {
        let dir = TempDir::new("corrupt");
        let corrupt = dir.join("corrupt.png");
        std::fs::write(&corrupt, b"definitely not a png").expect("write corrupt file");
        let empty = dir.join("empty.png");
        std::fs::write(&empty, b"").expect("write empty file");

        assert_eq!(dimensions(&corrupt), None);
        assert_eq!(dimensions(&empty), None);
        assert_eq!(phash64(&corrupt), None);
        assert_eq!(thumbnail(&corrupt, &dir.path), None);
    }

    #[test]
    fn identical_images_have_zero_distance() {
        let dir = TempDir::new("phash-identical");
        let first = dir.join("one.png");
        let second = dir.join("two.png");
        solid(&first, [200, 30, 30]);
        solid(&second, [200, 30, 30]);

        let a = phash64(&first).expect("first phash");
        let b = phash64(&second).expect("second phash");
        assert_eq!(a, b);
        assert_eq!((a ^ b).count_ones(), 0);
    }

    #[test]
    fn inverted_pattern_has_large_distance() {
        let dir = TempDir::new("phash-pattern");
        let first = dir.join("ramp.png");
        let second = dir.join("inverted.png");
        gradient(&first, false);
        gradient(&second, true);

        let a = phash64(&first).expect("first phash");
        let b = phash64(&second).expect("second phash");
        assert!(
            (a ^ b).count_ones() >= 32,
            "inverted patterns should be far apart, distance {}",
            (a ^ b).count_ones()
        );
    }

    #[test]
    fn slight_modification_stays_close() {
        let dir = TempDir::new("phash-modified");
        let base = dir.join("base.png");
        let modified = dir.join("modified.png");
        gradient(&base, false);

        // Far-right quarter black reverses only the last comparisons.
        let image = RgbImage::from_fn(64, 48, |x, _| {
            let value = ((x * 255) / 63) as u8;
            if x >= 48 {
                Rgb([0, 0, 0])
            } else {
                Rgb([value, value, value])
            }
        });
        save(&image, &modified);

        let a = phash64(&base).expect("base phash");
        let b = phash64(&modified).expect("modified phash");
        let distance = (a ^ b).count_ones();
        assert!(
            distance > 0 && distance < 32,
            "a small edit should produce a small distance, got {distance}"
        );
    }

    #[test]
    fn solid_colors_are_color_invariant() {
        // Uniform images carry no dHash signal regardless of colour.
        let dir = TempDir::new("phash-solid");
        let red = dir.join("red.png");
        let blue = dir.join("blue.png");
        solid(&red, [255, 0, 0]);
        solid(&blue, [0, 0, 255]);

        let a = phash64(&red).expect("red phash");
        let b = phash64(&blue).expect("blue phash");
        assert_eq!((a ^ b).count_ones(), 0);
    }

    #[test]
    fn thumbnail_is_written_and_reused() {
        let dir = TempDir::new("thumbnail");
        let source = dir.join("photo.png");
        let out_dir = dir.join("thumbs");
        gradient(&source, false);

        let first = thumbnail(&source, &out_dir).expect("thumbnail generated");
        assert!(first.exists());
        assert_eq!(first.extension().and_then(|e| e.to_str()), Some("jpg"));

        let decoded = image::open(&first).expect("decode thumbnail");
        assert!(decoded.width() <= THUMB_SIZE && decoded.height() <= THUMB_SIZE);

        let first_mtime = std::fs::metadata(&first)
            .and_then(|meta| meta.modified())
            .expect("thumbnail mtime");

        std::thread::sleep(Duration::from_millis(60));
        let second = thumbnail(&source, &out_dir).expect("thumbnail reused");
        assert_eq!(first, second);

        let second_mtime = std::fs::metadata(&second)
            .and_then(|meta| meta.modified())
            .expect("thumbnail mtime");
        assert_eq!(
            first_mtime, second_mtime,
            "fresh thumbnail must not be rewritten"
        );
    }

    #[test]
    fn legacy_thumbnail_is_regenerated() {
        let dir = TempDir::new("legacy-thumb");
        let source = dir.join("photo.png");
        let out_dir = dir.join("thumbs");
        gradient(&source, false);

        let thumb = thumbnail_path(&source, &out_dir);
        std::fs::create_dir_all(&out_dir).expect("thumb dir");
        let legacy = image::RgbImage::new(480, 360);
        legacy
            .save_with_format(&thumb, image::ImageFormat::Jpeg)
            .expect("legacy thumbnail");
        let cached = phash64_cached(&source, &out_dir).expect("regenerated hash");
        let direct = phash64(&source).expect("direct hash");
        assert_eq!(cached, direct);

        let regenerated = image::open(&thumb).expect("regenerated thumbnail");
        assert_eq!(regenerated.width().max(regenerated.height()), THUMB_SIZE);
    }

    #[test]
    fn cached_hash_uses_thumbnail_and_is_deterministic() {
        let dir = TempDir::new("phash-cache");
        let source = dir.join("photo.png");
        let thumbs = dir.join("thumbs");
        gradient(&source, false);

        let cached = phash64_cached(&source, &thumbs).expect("cached hash");
        let direct = phash64(&source).expect("direct hash");
        assert_eq!(cached, direct);
        let thumb = thumbnail_path(&source, &thumbs);
        assert!(thumb.exists());

        let mtime = std::fs::metadata(&thumb)
            .and_then(|m| m.modified())
            .expect("mtime");
        std::thread::sleep(Duration::from_millis(60));
        let again = phash64_cached(&source, &thumbs).expect("cached hash again");
        assert_eq!(cached, again);
        let mtime2 = std::fs::metadata(&thumb)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(mtime, mtime2, "fresh thumbnail must be reused");
    }

    #[test]
    fn reusing_fresh_thumbnail_does_not_decode_source() {
        let dir = TempDir::new("phash-reuse");
        let source = dir.join("photo.png");
        let thumbs = dir.join("thumbs");
        gradient(&source, false);

        let cached = phash64_cached(&source, &thumbs).expect("cached hash");
        let reused =
            phash64_reusing_fresh_thumbnail(&source, &thumbs).expect("fresh thumbnail hash");
        assert_eq!(cached, reused);

        // Unchanged mtime proves no re-decode or rewrite.
        let mtime = std::fs::metadata(thumbnail_path(&source, &thumbs))
            .and_then(|m| m.modified())
            .expect("mtime");
        std::thread::sleep(Duration::from_millis(60));
        let reused_again =
            phash64_reusing_fresh_thumbnail(&source, &thumbs).expect("thumbnail-only hash");
        assert_eq!(cached, reused_again);
        let mtime2 = std::fs::metadata(thumbnail_path(&source, &thumbs))
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(mtime, mtime2, "fresh thumbnail must be reused");
    }

    #[test]
    fn parse_langs_skips_header_blanks_crlf_and_duplicates() {
        let raw = "List of available languages in /usr/share/tessdata (3):\r\n\
                   eng\r\n\
                   \r\n\
                   osd\r\n\
                   eng\r\n\
                   \r\n\
                   deu\r\n";

        assert_eq!(parse_langs(raw), vec!["eng", "osd", "deu"]);
        assert!(parse_langs("").is_empty());
        assert!(parse_langs("List of available languages (0):\n").is_empty());
    }

    #[test]
    fn status_can_report_a_missing_engine() {
        let status = build_status(
            false,
            Vec::new(),
            PathBuf::from("/nonexistent/tessdata"),
            Some("No such file or directory (os error 2)".to_string()),
        );

        assert!(!status.engine_present);
        assert!(!status.available);
        assert!(status.languages.is_empty());
        assert_eq!(
            status.error.as_deref(),
            Some("No such file or directory (os error 2)")
        );
        assert_eq!(status.tessdata_dir, "/nonexistent/tessdata");
    }

    #[test]
    fn private_tessdata_requires_a_traineddata_file() {
        let dir = TempDir::new("tessdata-empty");
        assert_eq!(private_tessdata(&dir.path), None);

        std::fs::write(dir.join("README.txt"), b"not language data").expect("write unrelated file");
        assert_eq!(private_tessdata(&dir.path), None);

        std::fs::write(dir.join("eng.traineddata"), b"x").expect("write language file");
        assert_eq!(private_tessdata(&dir.path), Some(dir.path.clone()));
    }

    #[test]
    fn existing_large_tessdata_is_reused_without_download() {
        let dir = TempDir::new("tessdata-existing");
        let target = dir.join("eng.traineddata");
        let file = std::fs::File::create(&target).expect("create sparse file");
        file.set_len(2_000_000).expect("size sparse file");
        drop(file);

        let resolved = download_eng_tessdata_to(&dir.path).expect("existing file is reused");

        assert_eq!(resolved, target);
        assert!(!dir.join("eng.traineddata.part").exists());
        assert_eq!(
            std::fs::metadata(&target).expect("metadata").len(),
            2_000_000
        );
    }
}
