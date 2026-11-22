pub mod delimited;
pub mod email;
pub mod external;
pub mod image;
pub mod markdown;
pub mod office;
pub mod pdf;
pub mod parquet;
pub mod pdf_image;
pub mod text;

use crate::config::ExternalExtractor;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Process-wide OCR flag so extraction never takes the config lock.
static OCR_ENABLED: AtomicBool = AtomicBool::new(true);

/// Tesseract language codes; empty means pick from installed (English first).
static OCR_LANGUAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Updates the process-wide OCR setting on config load/save.
pub fn set_ocr_enabled(enabled: bool) {
    OCR_ENABLED.store(enabled, Ordering::SeqCst);
}

pub fn set_ocr_languages(languages: Vec<String>) {
    if let Ok(mut slot) = OCR_LANGUAGES.lock() {
        *slot = languages;
    }
}

pub fn ocr_languages() -> Vec<String> {
    OCR_LANGUAGES
        .lock()
        .map(|languages| languages.clone())
        .unwrap_or_default()
}

pub fn ocr_enabled() -> bool {
    OCR_ENABLED.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    /// Never read or index file content.
    NamesOnly,
    NameAndContent,
}

/// Normalized directory-mode rules, kept outside the config `RwLock`.
struct ModeRules {
    watched: Vec<String>,
    names_only: Vec<String>,
}

static MODE_RULES: std::sync::RwLock<ModeRules> =
    std::sync::RwLock::new(ModeRules { watched: Vec::new(), names_only: Vec::new() });

/// Rule comparison key: no trailing separator, forward slashes, case-insensitive on Windows.
pub fn rule_key(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let trimmed = raw.trim_end_matches(['/', '\\']);
    let unified = trimmed.replace('\\', "/");
    if cfg!(windows) {
        unified.to_lowercase()
    } else {
        unified
    }
}

/// `rule` is `path` or an ancestor; the separator check keeps `/media-notes` out of `/media`.
fn rule_covers(rule: &str, path: &str) -> bool {
    path == rule || path.strip_prefix(rule).is_some_and(|rest| rest.starts_with('/'))
}

/// Effective mode for `path`; the deepest matching rule wins. Pure so reconcile can diff rule sets.
pub fn mode_from_rules(watched: &[String], names_only: &[String], path: &Path) -> IndexMode {
    let key = rule_key(path);
    let mut best: Option<(usize, IndexMode)> = None;
    for rule in watched {
        if rule_covers(rule, &key) && best.is_none_or(|(len, _)| rule.len() > len) {
            let mode = if names_only.contains(rule) {
                IndexMode::NamesOnly
            } else {
                IndexMode::NameAndContent
            };
            best = Some((rule.len(), mode));
        }
    }
    // A hand-edited config can mark a directory that is not a watched root.
    for rule in names_only {
        if !watched.contains(rule)
            && rule_covers(rule, &key)
            && best.is_none_or(|(len, _)| rule.len() > len)
        {
            best = Some((rule.len(), IndexMode::NamesOnly));
        }
    }
    best
        .map(|(_, mode)| mode)
        .unwrap_or(IndexMode::NameAndContent)
}

pub fn set_path_modes(watched: &[PathBuf], names_only: &[PathBuf]) {
    if let Ok(mut slot) = MODE_RULES.write() {
        slot.watched = watched.iter().map(|p| rule_key(p)).collect();
        slot.names_only = names_only.iter().map(|p| rule_key(p)).collect();
    }
}

/// Sets names-only dirs, keeping watched roots from [`set_path_modes`].
pub fn set_name_only_dirs(dirs: Vec<PathBuf>) {
    let watched = MODE_RULES
        .read()
        .map(|rules| rules.watched.clone())
        .unwrap_or_default();
    if let Ok(mut slot) = MODE_RULES.write() {
        slot.watched = watched;
        slot.names_only = dirs.iter().map(|p| rule_key(p)).collect();
    }
}

pub fn mode_for_path(path: &Path) -> IndexMode {
    MODE_RULES
        .read()
        .map(|rules| mode_from_rules(&rules.watched, &rules.names_only, path))
        .unwrap_or(IndexMode::NameAndContent)
}

pub fn is_name_only_path(path: &Path) -> bool {
    mode_for_path(path) == IndexMode::NamesOnly
}

/// Process-wide content caps, read outside the config `RwLock`.
static CONTENT_MAX_FILE_SIZE_BYTES: AtomicU64 = AtomicU64::new(20 * 1024 * 1024);
static CONTENT_MAX_TEXT_BYTES: AtomicUsize = AtomicUsize::new(1_000_000);

static CSV_HEAD_ROWS: AtomicUsize = AtomicUsize::new(200);

pub fn csv_head_rows() -> usize {
    CSV_HEAD_ROWS.load(Ordering::SeqCst)
}

pub fn set_content_caps(max_file_size_mb: u64, max_text_bytes: usize, csv_head_rows: usize) {
    CSV_HEAD_ROWS.store(csv_head_rows.max(1), Ordering::SeqCst);
    CONTENT_MAX_FILE_SIZE_BYTES.store(
        max_file_size_mb.saturating_mul(1024 * 1024),
        Ordering::SeqCst,
    );
    CONTENT_MAX_TEXT_BYTES.store(max_text_bytes.max(1024), Ordering::SeqCst);
    clear_extraction_cache();
}

pub fn content_max_file_size_bytes() -> u64 {
    CONTENT_MAX_FILE_SIZE_BYTES.load(Ordering::SeqCst)
}

pub fn content_max_text_bytes() -> usize {
    CONTENT_MAX_TEXT_BYTES.load(Ordering::SeqCst)
}

/// Replaces the external extractor registry and clears the cache.
pub fn set_external_extractors(extractors: Vec<ExternalExtractor>) {
    external::set_extractors(extractors);
    clear_extraction_cache();
}

/// Drops every cached extraction so changed OCR output is actually re-read.
pub fn clear_cache() {
    clear_extraction_cache();
}

fn clear_extraction_cache() {
    if let Some(cache) = EXTRACTION_CACHE.get()
        && let Ok(mut cache) = cache.lock()
    {
        cache.map.clear();
        cache.order.clear();
    }
}

#[derive(Debug, Clone)]
pub struct DocumentExtract {
    pub text: String,
    pub page_count: Option<usize>,
    pub is_encrypted: bool,
    pub is_scanned: bool,
    pub format_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFormat {
    Pdf,
    Docx,
    Pptx,
    Xlsx,
    Epub,
    Odt,
    Ods,
    Odp,
    Rtf,
    Eml,
    /// Outlook OLE/MAPI email.
    Msg,
    /// CSV/TSV: one line of text per row.
    Delimited,
    /// Schema plus the first rows, never the whole file.
    Parquet,
    PlainText,
    Image,
    Unknown,
}

/// Sniffs the format by magic bytes, falling back to extension.
pub fn sniff_format(path: &Path) -> DocumentFormat {
    let mut header = [0u8; 16];
    if let Ok(mut f) = File::open(path)
        && let Ok(n) = f.read(&mut header)
        && n >= 4
    {
        if header.starts_with(b"%PDF-") {
            return DocumentFormat::Pdf;
        }
        if header.starts_with(b"{\\rtf") {
            return DocumentFormat::Rtf;
        }
        if header.starts_with(b"PAR1") {
            return DocumentFormat::Parquet;
        }
        if header.starts_with(b"\x89PNG")
            || header.starts_with(b"\xff\xd8\xff")
            || header.starts_with(b"GIF8")
            || header.starts_with(b"RIFF")
            || header.starts_with(b"BM")
        {
            return DocumentFormat::Image;
        }
        if header.starts_with(b"PK\x03\x04") {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            return match ext.as_str() {
                "docx" => DocumentFormat::Docx,
                "pptx" => DocumentFormat::Pptx,
                "xlsx" => DocumentFormat::Xlsx,
                "epub" => DocumentFormat::Epub,
                "odt" => DocumentFormat::Odt,
                "ods" => DocumentFormat::Ods,
                "odp" => DocumentFormat::Odp,
                _ => DocumentFormat::Docx, // Default zip-office
            };
        }
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "pdf" => DocumentFormat::Pdf,
        "docx" => DocumentFormat::Docx,
        "pptx" => DocumentFormat::Pptx,
        "xlsx" => DocumentFormat::Xlsx,
        "epub" => DocumentFormat::Epub,
        "odt" => DocumentFormat::Odt,
        "ods" => DocumentFormat::Ods,
        "odp" => DocumentFormat::Odp,
        "rtf" => DocumentFormat::Rtf,
        "eml" => DocumentFormat::Eml,
        "msg" => DocumentFormat::Msg,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" | "heic" | "heif" => {
            DocumentFormat::Image
        }
        "csv" | "tsv" => DocumentFormat::Delimited,
        "parquet" => DocumentFormat::Parquet,
        "txt" | "md" | "markdown" | "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "json"
        | "toml" | "yaml" | "yml" | "xml" | "html" | "htm" | "css" | "c" | "cpp" | "h" | "java"
        | "sh" | "log" | "sql" => DocumentFormat::PlainText,

        _ => DocumentFormat::Unknown,
    }
}

/// LRU of extraction results keyed by (path, size, mtime).
static EXTRACTION_CACHE: std::sync::OnceLock<Mutex<ExtractionCache>> = std::sync::OnceLock::new();

struct ExtractionCache {
    capacity: usize,
    map: HashMap<(std::path::PathBuf, u64, u64), DocumentExtract>,
    order: VecDeque<(std::path::PathBuf, u64, u64)>,
}

impl ExtractionCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &(std::path::PathBuf, u64, u64)) -> Option<DocumentExtract> {
        if let Some(val) = self.map.get(key) {
            if let Some(pos) = self.order.iter().position(|k| k == key) {
                self.order.remove(pos);
            }
            self.order.push_back(key.clone());
            return Some(val.clone());
        }
        None
    }

    fn insert(&mut self, key: (std::path::PathBuf, u64, u64), val: DocumentExtract) {
        if self.map.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.map.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.map.insert(key, val);
    }
}

fn get_extraction_cache() -> &'static Mutex<ExtractionCache> {
    // Small on purpose: entries can be ~1 MB each and the crawl doesn't rely on hits.
    EXTRACTION_CACHE.get_or_init(|| Mutex::new(ExtractionCache::new(16)))
}

/// Cache key `(path, size, mtime)`, shared by insert and lookup.
fn cache_key(path: &Path) -> Option<(PathBuf, u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((path.to_path_buf(), size, mtime))
}

/// Cache-only lookup for query-time snippets; never extracts anything.
pub fn cached_document(path: &Path) -> Option<DocumentExtract> {
    let key = cache_key(path)?;
    let mut cache = get_extraction_cache().lock().ok()?;
    cache.get(&key)
}

#[cfg(test)]
pub fn insert_cached_document(path: &Path, document: DocumentExtract) {
    let Some(key) = cache_key(path) else {
        return;
    };
    let mut cache = get_extraction_cache().lock().unwrap();
    cache.insert(key, document);
}

/// Per-document extraction timeout: 15 s plain, 45 s for OCR candidates.
fn extraction_timeout_for(file_size: u64, is_ocr_candidate: bool) -> Duration {
    let base = if is_ocr_candidate { 45 } else { 15 };
    let extra = file_size / (5 * 1024 * 1024) * 2;
    let secs = (base + extra).min(120);
    Duration::from_secs(secs)
}

/// Backpressure wait for a free slot, so a stalled document cannot hold every slot forever.
const SLOT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Concurrent extractions (and so concurrent OCR processes).
pub(crate) fn extraction_concurrency() -> usize {
    extraction_concurrency_for(
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
    )
}

fn extraction_concurrency_for(cores: usize) -> usize {
    // One CPU thread per OCR child; leave headroom for the crawler and embedder.
    cores.saturating_sub(2).clamp(1, 8)
}

/// Bounded slot pool (condvar + atomic count) paired with the extraction thread pool.
struct SlotPool {
    available: Mutex<usize>,
    cvar: Condvar,
}

impl SlotPool {
    fn new(slots: usize) -> Self {
        Self {
            available: Mutex::new(slots),
            cvar: Condvar::new(),
        }
    }

    fn acquire(&'static self) -> Option<ExtractionSlot> {
        self.acquire_timeout(SLOT_WAIT_TIMEOUT)
    }

    fn acquire_timeout(&'static self, timeout: Duration) -> Option<ExtractionSlot> {
        let mut available = self.available.lock().ok()?;
        let start = Instant::now();
        while *available == 0 {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return None;
            }
            let remaining = timeout - elapsed;
            let (next, wait_res) = self.cvar.wait_timeout(available, remaining).ok()?;
            available = next;
            if wait_res.timed_out() && *available == 0 {
                return None;
            }
        }
        *available -= 1;
        Some(ExtractionSlot { pool: self })
    }
}

struct ExtractionSlot {
    pool: &'static SlotPool,
}

impl Drop for ExtractionSlot {
    fn drop(&mut self) {
        if let Ok(mut count) = self.pool.available.lock() {
            *count += 1;
            self.pool.cvar.notify_one();
        }
    }
}

static SLOT_POOL: std::sync::OnceLock<SlotPool> = std::sync::OnceLock::new();

fn slot_pool() -> &'static SlotPool {
    SLOT_POOL.get_or_init(|| SlotPool::new(extraction_concurrency()))
}

static EXTRACTION_POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();

fn extraction_pool() -> Option<&'static rayon::ThreadPool> {
    EXTRACTION_POOL
        .get_or_init(|| {
            // Share the slot budget, including timed-out work still holding a slot.
            match rayon::ThreadPoolBuilder::new()
                .num_threads(extraction_concurrency())
                .thread_name(|index| format!("kensaku-extract-{}", index))
                .build()
            {
                Ok(pool) => Some(pool),
                Err(e) => {
                    eprintln!(
                        "[Kensaku] Could not create extraction thread pool ({}); extraction runs inline without a timeout",
                        e
                    );
                    None
                }
            }
        })
        .as_ref()
}

/// Paths already warned about timing out, so a stuck document is reported once.
static TIMEOUT_WARNED: std::sync::OnceLock<Mutex<HashSet<PathBuf>>> = std::sync::OnceLock::new();

fn warn_extraction_timeout(path: &Path, timeout: Duration) {
    let warned = TIMEOUT_WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut warned = warned.lock().unwrap();
    // Bounded on systems with many broken documents.
    if warned.len() >= 4096 {
        warned.clear();
    }
    if warned.insert(path.to_path_buf()) {
        eprintln!(
            "[Kensaku] Text extraction timed out after {}s; leaving pending: {}",
            timeout.as_secs(),
            path.display()
        );
    }
}

/// Rate-limited per path and per process.
fn warn_external_failure(path: &Path, error: &str) {
    external::record_failure(path, error);
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    static WARNED: std::sync::OnceLock<Mutex<HashSet<PathBuf>>> = std::sync::OnceLock::new();
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut warned = warned.lock().unwrap();
    if warned.len() >= 4096 {
        warned.clear();
    }
    if warned.insert(path.to_path_buf()) && COUNT.fetch_add(1, Ordering::Relaxed) < 20 {
        eprintln!(
            "[Kensaku] External extraction failed for {}: {}",
            path.display(),
            error
        );
    }
}

/// Rate-limited so a saturated crawl does not log once per file.
fn warn_slot_saturation(path: &Path) {
    use std::sync::atomic::AtomicUsize;
    static WARNED: AtomicUsize = AtomicUsize::new(0);
    let count = WARNED.fetch_add(1, Ordering::Relaxed);
    if count < 5 {
        eprintln!(
            "[Kensaku] Extraction slots saturated; leaving pending: {}",
            path.display()
        );
    }
}

pub fn extract_document(path: &Path, max_bytes: u64) -> Option<DocumentExtract> {
    extract_document_checked(path, max_bytes).0
}

/// Like [`extract_document`], plus whether it was incomplete (timeout, no slot, OCR unavailable) and should be retried.
pub fn extract_document_checked(path: &Path, max_bytes: u64) -> (Option<DocumentExtract>, bool) {
    let retry = std::sync::Arc::new(AtomicBool::new(false));
    let document = extract_document_tracked(path, max_bytes, &retry);
    (document, retry.load(Ordering::SeqCst))
}

fn extract_document_tracked(
    path: &Path,
    max_bytes: u64,
    retry: &std::sync::Arc<AtomicBool>,
) -> Option<DocumentExtract> {
    let meta = std::fs::metadata(path).ok()?;
    let file_size = meta.len();
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    // Metadata-only above the size cap, except head-sampled CSV/TSV/Parquet.
    let head_sampled = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ["csv", "tsv", "parquet"].iter().any(|s| ext.eq_ignore_ascii_case(s)));
    if file_size > content_max_file_size_bytes() && !head_sampled {
        return None;
    }

    // Resolve external extractors before the OCR gate so plugins can handle images with OCR off.
    let external = external::find(path);

    // OCR off: images never produce text; checked before the cache so stale OCR text isn't reused.
    if external.is_none() && !ocr_enabled() && sniff_format(path) == DocumentFormat::Image {
        return None;
    }

    let cache_key = (path.to_path_buf(), file_size, mtime);
    {
        let mut cache = get_extraction_cache().lock().unwrap();
        if let Some(cached) = cache.get(&cache_key) {
            return Some(cached);
        }
    }

    let capped_bytes = (max_bytes as usize).min(content_max_text_bytes());
    let format = sniff_format(path);
    let is_ocr = format == DocumentFormat::Image || format == DocumentFormat::Pdf;
    let timeout = extraction_timeout_for(file_size, is_ocr);

    // Timed-out files are retried later and not cached. The slot moves into the
    // task so timed-out work can't exceed the pool size.
    let result = match (extraction_pool(), slot_pool().acquire()) {
        (Some(pool), Some(slot)) => {
            let (tx, rx) = std::sync::mpsc::channel();
            let worker_path = path.to_path_buf();
            let worker_external = external.clone();
            let worker_retry = std::sync::Arc::clone(retry);
            pool.spawn(move || {
                crate::util::priority::apply_current();
                // Hold the slot for the real duration, even if the caller stopped waiting.
                let _slot = slot;
                // A panic escaping `rayon::spawn` aborts; malformed documents must not crash.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    extract_document_inner(
                        &worker_path,
                        capped_bytes,
                        format,
                        file_size,
                        worker_external,
                        &worker_retry,
                    )
                }))
                .unwrap_or_else(|_| {
                    eprintln!(
                        "[Kensaku] Text extraction panicked; leaving pending: {}",
                        worker_path.display()
                    );
                    None
                });
                // The caller may have timed out already.
                let _ = tx.send(result);
            });

            match rx.recv_timeout(timeout) {
                Ok(result) => result,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    warn_extraction_timeout(path, timeout);
                    retry.store(true, Ordering::SeqCst);
                    None
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    retry.store(true, Ordering::SeqCst);
                    eprintln!(
                        "[Kensaku] Text extraction worker exited without a result; leaving pending: {}",
                        path.display()
                    );
                    None
                }
            }
        }
        // Every slot stayed busy: leave the file pending rather than queue unboundedly.
        (Some(_), None) => {
            warn_slot_saturation(path);
            retry.store(true, Ordering::SeqCst);
            None
        }
        // Pool creation failed (rare): extract inline.
        (None, _) => extract_document_inner(path, capped_bytes, format, file_size, external, retry),
    };

    // Very large documents are not cached.
    if let Some(ref res) = result
        && res.text.len() <= 256 * 1024
        && !retry.load(Ordering::SeqCst)
    {
        let mut cache = get_extraction_cache().lock().unwrap();
        cache.insert(cache_key, res.clone());
    }

    crate::util::memory::release_memory();
    result
}

fn extract_document_inner(
    path: &Path,
    capped_bytes: usize,
    format: DocumentFormat,
    file_size: u64,
    external: Option<ExternalExtractor>,
    retry: &AtomicBool,
) -> Option<DocumentExtract> {
    // Retry only if an engine should be usable; enabling one later re-queues via the OCR signature.
    let ocr_retry = || {
        if crate::ocr::usable_engine() != crate::ocr::NONE {
            retry.store(true, Ordering::SeqCst);
        }
    };

    if let Some(extractor) = external {
        return match extractor.run(path, capped_bytes) {
            Ok(document) => Some(document),
            Err(error) => {
                warn_external_failure(path, &error);
                None
            }
        };
    }

    match format {
        DocumentFormat::Pdf => {
            let res = pdf::extract_pdf(path, capped_bytes)?;
            if res.ocr_unavailable {
                ocr_retry();
            }
            Some(DocumentExtract {
                text: res.text,
                page_count: Some(res.page_count),
                is_encrypted: res.is_encrypted,
                is_scanned: res.is_scanned,
                format_name: "PDF".to_string(),
            })
        }
        DocumentFormat::Docx => {
            let text = office::extract_docx(path, capped_bytes)?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "Word Document".to_string(),
            })
        }
        DocumentFormat::Pptx => {
            let text = office::extract_pptx(path, capped_bytes)?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "Presentation".to_string(),
            })
        }
        DocumentFormat::Xlsx => {
            let text = office::extract_xlsx(path, capped_bytes)?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "Spreadsheet".to_string(),
            })
        }
        DocumentFormat::Epub => {
            let text = office::extract_epub(path, capped_bytes)?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "EPUB eBook".to_string(),
            })
        }
        DocumentFormat::Odt | DocumentFormat::Ods | DocumentFormat::Odp => {
            let text = office::extract_odt(path, capped_bytes)?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "OpenDocument".to_string(),
            })
        }
        DocumentFormat::Rtf => {
            let text = text::extract_rtf(path, capped_bytes)?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "Rich Text".to_string(),
            })
        }
        DocumentFormat::Eml => {
            let text = email::extract_eml(path, capped_bytes)?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "Email".to_string(),
            })
        }
        DocumentFormat::Msg => {
            let text = email::extract_msg(path, capped_bytes)?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "Email".to_string(),
            })
        }
        DocumentFormat::Image => {
            // Also guards the inline fallback and direct callers.
            if !ocr_enabled() {
                return None;
            }
            let text = match image::ocr_image(path) {
                crate::ocr::OcrOutcome::Text(text) => text,
                // No text is a real answer: replaces stale OCR text and marks the image done.
                crate::ocr::OcrOutcome::NoText => String::new(),
                crate::ocr::OcrOutcome::Unavailable => {
                    ocr_retry();
                    return None;
                }
            };
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: true,
                format_name: "Image (OCR)".to_string(),
            })
        }
        DocumentFormat::Parquet => {
            let text = parquet::extract_parquet(path, capped_bytes, csv_head_rows())?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "Parquet".to_string(),
            })
        }
        DocumentFormat::Delimited => {
            // Rows are kept whole, so this arm never uses the head/tail split below.
            let text = delimited::extract_delimited(path, capped_bytes, csv_head_rows())?;
            Some(DocumentExtract {
                text,
                page_count: None,
                is_encrypted: false,
                is_scanned: false,
                format_name: "Delimited text".to_string(),
            })
        }
        DocumentFormat::PlainText | DocumentFormat::Unknown => {
            // Large plain text: head and tail within the configured content budget.
            let budget = capped_bytes.max(1024);
            if file_size > budget as u64 {
                let half = budget / 2;
                let mut f = File::open(path).ok()?;

                let mut head_bytes = vec![0u8; half];
                let n_head = f.read(&mut head_bytes).unwrap_or(0);
                head_bytes.truncate(n_head);
                // Never end the head mid-character.
                let head_end = match std::str::from_utf8(&head_bytes) {
                    Ok(_) => head_bytes.len(),
                    Err(error) => error.valid_up_to(),
                };

                let mut tail_bytes = vec![0u8; half];
                let n_tail = if f
                    .seek(std::io::SeekFrom::End(-(half as i64)))
                    .is_ok()
                {
                    f.read(&mut tail_bytes).unwrap_or(0)
                } else {
                    0
                };
                tail_bytes.truncate(n_tail);
                // Start the tail on a character boundary.
                let mut tail_start = 0;
                while tail_start < tail_bytes.len().min(3)
                    && (tail_bytes[tail_start] & 0xC0) == 0x80
                {
                    tail_start += 1;
                }

                let head_str = String::from_utf8_lossy(&head_bytes[..head_end]);
                let tail_str = String::from_utf8_lossy(&tail_bytes[tail_start..]);
                let combined = format!(
                    "{}\n\n... [content omitted] ...\n\n{}",
                    head_str, tail_str
                );

                let combined = if markdown::is_markdown(path) {
                    // Don't carry fence state across the omitted middle.
                    format!(
                        "{}\n\n... [content omitted] ...\n\n{}",
                        markdown::strip(&head_str),
                        markdown::strip(&tail_str)
                    )
                } else {
                    combined
                };
                Some(DocumentExtract {
                    text: combined,
                    page_count: None,
                    is_encrypted: false,
                    is_scanned: false,
                    format_name: "Text".to_string(),
                })
            } else {
                let text = text::extract_text(path, capped_bytes)?;
                Some(DocumentExtract {
                    text,
                    page_count: None,
                    is_encrypted: false,
                    is_scanned: false,
                    format_name: "Text".to_string(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new(extension: &str, bytes: &[u8]) -> Self {
            let path = Self::path_for(extension);
            std::fs::write(&path, bytes).expect("write temp file");
            Self { path }
        }

        fn empty(extension: &str) -> Self {
            let path = Self::path_for(extension);
            File::create(&path).expect("create temp file");
            Self { path }
        }

        fn path_for(extension: &str) -> PathBuf {
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!(
                "kensaku-extract-{}-{}.{}",
                std::process::id(),
                id,
                extension
            ))
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn extract(path: &Path) -> (Option<DocumentExtract>, Duration) {
        let started = Instant::now();
        let result = extract_document(path, 1_000_000);
        (result, started.elapsed())
    }

    #[test]
    fn markdown_cleanup_uses_plain_text_dispatch() {
        let source = include_bytes!("../../../tests/fixtures/markdown/note.md");
        for extension in ["md", "markdown", "MD"] {
            let file = TempFile::new(extension, source);
            assert_eq!(sniff_format(&file.path), DocumentFormat::PlainText);
            let document = extract(&file.path).0.unwrap();
            for value in [
                "Searchable title",
                "Alternate name",
                "let answer = 42;",
                "Diagram label",
            ] {
                assert!(document.text.contains(value));
            }
            assert!(!document.text.contains("https://example.com"));
            assert!(!document.text.contains("```"));
        }
    }

    #[test]
    fn markdown_reuses_bounded_text_decoding() {
        let source = "---\ntitle: 検索\n---\n[Label](hidden)\n";
        let bytes: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(source.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let file = TempFile::new("markdown", &bytes);
        let text = text::extract_text(&file.path, 1000).unwrap();
        assert!(text.contains("検索"));
        assert!(text.contains("Label"));
        assert!(!text.contains("hidden"));
        let text_file = TempFile::new("md", b"[Label](hidden) trailing");
        assert_eq!(
            text::extract_text(&text_file.path, 7).as_deref(),
            Some("[Label]")
        );
        let binary = TempFile::new("md", b"not\0text");
        assert!(text::extract_text(&binary.path, 100).is_none());
        let source = format!(
            "[HeadLabel](head-url)\n{}\n[TailLabel](tail-url)",
            "padding\n".repeat(1000)
        );
        let file = TempFile::new("md", source.as_bytes());
        let document = extract_document_inner(
            &file.path,
            1024,
            DocumentFormat::PlainText,
            source.len() as u64,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(document.text.contains("HeadLabel"));
        assert!(document.text.contains("TailLabel"));
        assert!(!document.text.contains("head-url"));
        assert!(!document.text.contains("tail-url"));
    }

    #[test]
    fn garbage_pdf_returns_none_without_hanging() {
        let file = TempFile::new("pdf", b"this is definitely not a pdf\n\x00\x01\x02\xff");
        let (result, elapsed) = extract(&file.path);

        assert!(
            result.is_none(),
            "garbage PDF should not extract: {result:?}"
        );
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    }

    #[test]
    fn truncated_pdf_header_returns_sane_result() {
        let file = TempFile::new("pdf", b"%PDF-1.4\n");
        let (result, elapsed) = extract(&file.path);

        // A header-only document may load or not, but must not panic or report content.
        if let Some(doc) = &result {
            assert_eq!(doc.page_count, Some(0));
            assert!(doc.text.is_empty(), "unexpected text: {:?}", doc.text);
        }
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    }

    #[test]
    fn docx_zip_without_document_xml_returns_none() {
        let file = TempFile::path_for("docx");
        {
            let handle = File::create(&file).expect("create zip");
            let mut zip = zip::ZipWriter::new(handle);
            zip.start_file("word/styles.xml", zip::write::SimpleFileOptions::default())
                .expect("start file");
            zip.write_all(b"<styles/>").expect("write entry");
            zip.finish().expect("finish zip");
        }
        let _cleanup = TempFile { path: file.clone() };

        let (result, elapsed) = extract(&file);

        assert!(
            result.is_none(),
            "docx without word/document.xml should not extract: {result:?}"
        );
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    }

    #[test]
    fn zero_byte_file_returns_none_without_hanging() {
        let file = TempFile::empty("txt");
        let (result, elapsed) = extract(&file.path);

        assert!(
            result.is_none(),
            "empty file should not extract: {result:?}"
        );
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    }

    fn cached_text(text: &str) -> DocumentExtract {
        DocumentExtract {
            text: text.to_string(),
            page_count: None,
            is_encrypted: false,
            is_scanned: false,
            format_name: "Text".to_string(),
        }
    }

    #[test]
    fn cached_document_only_consults_the_cache() {
        let file = TempFile::new("txt", b"on-disk text that must never be read");
        let path = &file.path;

        assert!(
            cached_document(path).is_none(),
            "a cold cache must not extract on lookup"
        );

        insert_cached_document(path, cached_text("cached needle text"));
        let hit = cached_document(path).expect("cached document");
        assert_eq!(hit.text, "cached needle text");
    }

    #[test]
    fn image_extraction_is_disabled_when_ocr_is_off() {
        // A valid PNG, so `None` is due to the setting; never asserts on OCR text.
        let path = TempFile::path_for("png");
        ::image::RgbImage::from_pixel(20, 20, ::image::Rgb([12, 34, 56]))
            .save_with_format(&path, ::image::ImageFormat::Png)
            .expect("write test png");
        let _cleanup = TempFile { path: path.clone() };

        let previous = ocr_enabled();
        set_ocr_enabled(false);
        let result = extract_document(&path, 1_000_000);
        set_ocr_enabled(previous);

        assert!(
            result.is_none(),
            "an image must not produce content while OCR is disabled: {result:?}"
        );
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::extraction_concurrency_for;

    #[test]
    fn extraction_reserves_cpu_capacity_without_starving_small_machines() {
        for cores in 1usize..=256 {
            let workers = extraction_concurrency_for(cores);
            assert!((1..=8).contains(&workers));
            assert!(workers <= cores.saturating_sub(2).max(1));
        }
        assert_eq!(extraction_concurrency_for(1), 1);
        assert_eq!(extraction_concurrency_for(2), 1);
        assert_eq!(extraction_concurrency_for(4), 2);
        assert_eq!(extraction_concurrency_for(16), 8);
    }
}

#[cfg(test)]
mod mode_tests {
    use super::{IndexMode, mode_from_rules, rule_key};
    use std::path::{Path, PathBuf};

    fn mode(watched: &[&str], names_only: &[&str], path: &str) -> IndexMode {
        let watched: Vec<String> = watched.iter().map(|p| rule_key(Path::new(*p))).collect();
        let names_only: Vec<String> = names_only.iter().map(|p| rule_key(Path::new(*p))).collect();
        mode_from_rules(&watched, &names_only, &PathBuf::from(path))
    }

    #[test]
    fn nested_watched_root_overrides_names_only_parent() {
        assert_eq!(
            mode(&["/media", "/media/work"], &["/media"], "/media/work/report.pdf"),
            IndexMode::NameAndContent
        );
        assert_eq!(
            mode(&["/media", "/media/work"], &["/media"], "/media/other/report.pdf"),
            IndexMode::NamesOnly
        );
        assert_eq!(
            mode(&["/media", "/media/work"], &["/media"], "/media/work"),
            IndexMode::NameAndContent
        );
    }

    #[test]
    fn names_only_subdirectory_overrides_content_root() {
        assert_eq!(
            mode(&["/home"], &["/home/photos"], "/home/photos/a.jpg"),
            IndexMode::NamesOnly
        );
        assert_eq!(
            mode(&["/home"], &["/home/photos"], "/home/docs/a.txt"),
            IndexMode::NameAndContent
        );
    }

    #[test]
    fn sibling_with_shared_prefix_is_not_covered() {
        assert_eq!(
            mode(&["/media"], &["/media"], "/media-notes.txt"),
            IndexMode::NameAndContent
        );
    }

    #[test]
    fn trailing_separators_and_legacy_names_only_rules() {
        assert_eq!(rule_key(Path::new("/media/")), rule_key(Path::new("/media")));
        assert_eq!(mode(&[], &["/media"], "/media/a.pdf"), IndexMode::NamesOnly);
        assert_eq!(mode(&[], &["/media"], "/other/a.pdf"), IndexMode::NameAndContent);
    }

    #[test]
    fn toggling_the_child_on_keeps_it_names_only() {
        assert_eq!(
            mode(
                &["/media", "/media/work"],
                &["/media", "/media/work"],
                "/media/work/a.pdf"
            ),
            IndexMode::NamesOnly
        );
        assert_eq!(
            mode(&["/media", "/media/work"], &["/media"], "/media/work/a.pdf"),
            IndexMode::NameAndContent
        );
    }
}
