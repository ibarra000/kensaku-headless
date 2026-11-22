pub mod arena;
pub mod burst;
pub mod content;
pub mod extract;
pub mod ignore;
pub mod metadata;
pub mod metadata_db;
pub mod tokenizer;
pub mod walker;
pub mod watcher;

use crate::config::KensakuConfig;
use crate::embed::units::chunk_text;
use crate::embed::{EMBEDDING_DIM, EmbeddingQueue, ModelRegistry, Unit, UnitStore};
use crate::indexer::content::{
    ContentIndex, is_indexable_extension, is_indexable_file_name, read_text_content_checked,
};
use crate::indexer::extract::IndexMode;
use crate::indexer::metadata::MetadataStore;
use crate::indexer::walker::{FileEntry, is_cloud_placeholder_path, walk_directories_until};
use crate::indexer::watcher::{WatcherHandle, start_file_watcher};
use crate::search::{SearchDispatcher, SearchResponse, SortOrder};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Push callback for indexing progress.
pub type StatusListener = Arc<dyn Fn(&IndexingStatus) + Send + Sync>;
pub type StatusSink = Arc<RwLock<Option<StatusListener>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingStatus {
    pub is_indexing: bool,
    pub files_indexed: usize,
    pub content_indexed: usize,
    pub current_status: String,
    #[serde(default)]
    pub is_embedding: bool,
    #[serde(default)]
    pub units_indexed: usize,
    #[serde(default)]
    pub semantic_ready: bool,
    /// Coarse pipeline phase: "idle", "walking", "extracting", "embedding", "committing".
    #[serde(default = "default_phase")]
    pub phase: String,
    /// Total entries discovered by the current crawl (0 until the walk ends).
    #[serde(default)]
    pub total_files: usize,
    /// Files whose text extraction has finished in the current crawl.
    #[serde(default)]
    pub extract_done: usize,
    /// Files queued for text extraction in the current crawl.
    #[serde(default)]
    pub extract_total: usize,
    /// True while the user has paused background indexing.
    #[serde(default)]
    pub paused: bool,
}

fn default_phase() -> String {
    "idle".to_string()
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_unix_secs() -> u64 {
    now_unix_ms() / 1000
}

/// True when an existing entry is unchanged and has no content re-read pending.
fn is_unchanged(existing: &FileEntry, entry: &FileEntry) -> bool {
    // (0, 0) is the walker's "could not stat" sentinel (e.g. a locked file), not a change.
    if entry.size == 0 && entry.modified == 0 {
        return true;
    }
    existing.modified == entry.modified && existing.size == entry.size && !existing.content_pending
}

/// One file scheduled for text extraction in the current crawl.
struct ExtractJob {
    node_id: u64,
    path: PathBuf,
    name: String,
    modified: u64,
    size: u64,
    is_image: bool,
    /// Pixels changed (or row is new), so any stored dHash is stale.
    phash_needs_refresh: bool,
}

/// A cancelled crawl never marks the first scan done, so setup returns to folder selection.
fn marks_first_scan_done(is_setup_complete: bool, first_scan_done: bool, cancelled: bool) -> bool {
    is_setup_complete && !first_scan_done && !cancelled
}

/// Crawl order: text before images, newest first within each class, then size.
fn extract_sort_key(job: &ExtractJob) -> (bool, Reverse<u64>, u64) {
    (job.is_image, Reverse(job.modified), job.size)
}

/// An extracted file plus its semantic vectors, sent over a bounded channel to keep memory flat.
struct ExtractedWork {
    node_id: u64,
    path: PathBuf,
    name: String,
    modified: u64,
    size: u64,
    content: Option<String>,
    units: Vec<(usize, usize, Vec<i8>)>,
    content_hash: Option<String>,
    /// Present for images, even when OCR produced no text.
    image_sig: Option<Vec<u8>>,
    /// Opportunistic hash from an already-fresh cached thumbnail.
    image_phash: Option<u64>,
    phash_needs_refresh: bool,
    /// Extraction did not finish (timeout, busy, OCR unavailable); retry next crawl.
    retry: bool,
}

/// Crawls a file may fail extraction on before it is left alone until it changes.
const MAX_EXTRACTION_ATTEMPTS: u32 = 3;

fn fnv_hash(data: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

static PHASH_BACKFILL_RUNNING: AtomicBool = AtomicBool::new(false);

/// Computes perceptual hashes the crawl skipped, one image at a time, pausing for crawls and queries.
fn start_phash_backfill(
    metadata: Arc<RwLock<MetadataStore>>,
    content_index: Arc<RwLock<Option<ContentIndex>>>,
    visual_store: Arc<RwLock<crate::embed::visual::VisualStore>>,
    crawl_running: Arc<AtomicBool>,
    query_in_flight: Arc<AtomicBool>,
) {
    if PHASH_BACKFILL_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }

    thread::spawn(move || {
        let thumbs_dir = KensakuConfig::config_dir().join("thumbs");
        struct ResetOnExit;
        impl Drop for ResetOnExit {
            fn drop(&mut self) {
                PHASH_BACKFILL_RUNNING.store(false, Ordering::SeqCst);
            }
        }
        let _reset = ResetOnExit;

        let Some(db) = metadata.read().db() else {
            return;
        };

        loop {
            // Never compete with a crawl or an interactive query.
            while crawl_running.load(Ordering::Relaxed) {
                thread::sleep(std::time::Duration::from_millis(500));
            }
            while query_in_flight.load(Ordering::Relaxed) {
                thread::sleep(std::time::Duration::from_millis(100));
            }

            let paths = match db.image_paths_missing_phash(16) {
                Ok(paths) => paths,
                Err(_) => return,
            };
            if !paths.is_empty() {
                for path in paths {
                    if crawl_running.load(Ordering::Relaxed) {
                        break;
                    }
                    if query_in_flight.load(Ordering::Relaxed) {
                        break;
                    }
                    let hash = crate::indexer::extract::image::phash64_cached(&path, &thumbs_dir)
                        .unwrap_or(u64::MAX);
                    let _ = db.set_image_phash(&path, hash);

                    if hash != u64::MAX {
                        let thumb =
                            crate::indexer::extract::image::thumbnail_path(&path, &thumbs_dir);
                        if thumb.is_file()
                            && let Ok(Some(node_id)) = db.get_node_id_by_path(&path)
                        {
                            let needs = visual_store.read().needs_embedding(node_id, hash);
                            if needs
                                && let Some(encoder) = crate::embed::visual::active_encoder()
                                && let Some(vec) = encoder.embed_image_from_thumbnail(&thumb)
                            {
                                visual_store.write().add_or_update(node_id, hash, &vec);
                                let _ =
                                    db.save_visual_unit(node_id, encoder.model_id(), hash, &vec);
                            }
                        }
                    }

                    crate::util::memory::release_memory();
                    // One decode at a time, with a yield so typing stays responsive.
                    thread::sleep(std::time::Duration::from_millis(20));
                }
                continue;
            }

            // Hashes are complete; assign bursts to old rows in small batches.
            let paths = match db.image_paths_missing_burst(50) {
                Ok(paths) => paths,
                Err(_) => return,
            };
            if paths.is_empty() {
                return;
            }
            for path in paths {
                if crawl_running.load(Ordering::Relaxed) {
                    break;
                }
                let signature = content_index
                    .read()
                    .as_ref()
                    .and_then(|index| index.get_stored_text(&path))
                    .map(|text| crate::indexer::burst::line_signature(&text))
                    .unwrap_or_default();
                let Ok(updates) = db.set_image_text_sig(&path, &signature) else {
                    return;
                };
                if !updates.is_empty() {
                    metadata.write().apply_bursts(&updates);
                }
                let Ok(updates) = db.assign_burst_for_path(&path) else {
                    return;
                };
                metadata.write().apply_bursts(&updates);
                crate::util::memory::release_memory();
                thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    });
}

/// Releases the crawl gate and resets the visible status if the crawl panics.
struct CrawlGateGuard {
    gate: Arc<AtomicBool>,
    status: Arc<RwLock<IndexingStatus>>,
    status_sink: StatusSink,
    armed: bool,
}

impl CrawlGateGuard {
    fn new(
        gate: Arc<AtomicBool>,
        status: Arc<RwLock<IndexingStatus>>,
        status_sink: StatusSink,
    ) -> Self {
        Self {
            gate,
            status,
            status_sink,
            armed: true,
        }
    }
}

impl Drop for CrawlGateGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.gate.store(false, Ordering::SeqCst);
        if !thread::panicking() {
            return;
        }
        // `parking_lot::RwLock` does not poison, so this cannot panic while unwinding.
        let mut st = self.status.write();
        st.is_indexing = false;
        st.paused = false;
        st.phase = "idle".to_string();
        st.extract_done = 0;
        st.extract_total = 0;
        st.current_status = "Indexing stopped unexpectedly — see logs, then retry.".to_string();
        IndexerManager::emit_status(&self.status_sink, &st);
        drop(st);
        crate::say!("[Kensaku Indexer] crawl aborted by a panic; status reset to idle");
    }
}

pub struct IndexerManager {
    pub metadata: Arc<RwLock<MetadataStore>>,
    pub content_index: Arc<RwLock<Option<ContentIndex>>>,
    pub unit_store: Arc<RwLock<UnitStore>>,
    pub visual_store: Arc<RwLock<crate::embed::visual::VisualStore>>,
    pub embedding_queue: Arc<Mutex<Option<EmbeddingQueue>>>,
    pub query_in_flight: Arc<AtomicBool>,
    pub config: Arc<RwLock<KensakuConfig>>,
    pub status: Arc<RwLock<IndexingStatus>>,
    /// Unix ms when the current/last crawl started, 0 if none.
    pub crawl_start_unix_ms: Arc<AtomicU64>,
    /// Unix secs of the last successful reconcile, 0 if never.
    pub last_reconcile_unix_secs: Arc<AtomicU64>,
    pub watcher_ok: Arc<AtomicBool>,
    is_running: Arc<AtomicBool>,
    /// Asks the running crawl to stop early; cleared when a crawl starts.
    cancel_requested: Arc<AtomicBool>,
    /// A `start_indexing_when_idle` waiter is already queued.
    crawl_pending: Arc<AtomicBool>,
    watcher_handle: Arc<Mutex<Option<WatcherHandle>>>,
    status_sink: StatusSink,
}

impl IndexerManager {
    pub fn new(config: KensakuConfig) -> Self {
        crate::util::priority::set_mode(config.crawl_mode);
        crate::indexer::extract::set_external_extractors(config.extractors.clone());
        crate::indexer::extract::set_path_modes(&config.watched_dirs, &config.name_only_dirs);
        crate::indexer::extract::image::set_ocr_max_image_bytes(config.ocr_max_image_bytes);
        crate::indexer::extract::set_content_caps(config.content_max_file_size_mb, config.content_max_text_bytes, config.content_csv_head_rows);
        crate::embed::set_max_semantic_units(config.content_max_semantic_units);
        let index_dir = KensakuConfig::index_dir();
        let content_index = match ContentIndex::new(&index_dir) {
            Ok(idx) => Some(idx),
            Err(e) => {
                crate::say_err!("[Kensaku] Failed to create Tantivy index: {}", e);
                None
            }
        };
        let content_rebuilt = content_index.as_ref().is_some_and(|index| index.rebuilt);

        let metadata_store = Arc::new(RwLock::new(MetadataStore::new()));
        let unit_store = Arc::new(RwLock::new(UnitStore::new(EMBEDDING_DIM)));
        let visual_store = Arc::new(RwLock::new(crate::embed::visual::VisualStore::new(
            crate::embed::visual::VISUAL_DIM,
            crate::embed::visual::MOCK_VISUAL_MODEL_ID,
        )));
        let query_in_flight = Arc::new(AtomicBool::new(false));

        // The embedding table is only loaded when semantic search is enabled.
        let semantic_enabled = config.semantic_enabled;

        // Discard units from a different embedding model; the worker re-queues every node.
        if semantic_enabled && let Some(db) = metadata_store.read().db() {
            let expected_model_id = ModelRegistry::global().model_id;
            match db.units_model_id() {
                Ok(Some(existing_model_id)) if existing_model_id != expected_model_id => {
                    match db.reset_semantic_index() {
                        Ok(()) => {
                            unit_store.write().clear();
                            log::info!(
                                "[Kensaku] Semantic model changed (units id {} -> {}); reset semantic index",
                                existing_model_id,
                                expected_model_id
                            );
                        }
                        Err(e) => {
                            crate::say_err!("[Kensaku] Failed to reset semantic index: {}", e);
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    crate::say_err!("[Kensaku] Failed to read semantic model id: {}", e);
                }
            }
        }

        // Units are always restored (the tier can be enabled later); only the embedding table is lazy.
        let mut units_count = 0;
        if let Some(db) = metadata_store.read().db() {
            let mut store = unit_store.write();
            match db.load_units_into(&mut store) {
                Ok(count) => {
                    units_count = count;
                    if units_count > 0 {
                        crate::say!(
                            "[Kensaku] Loaded {} semantic units from database",
                            units_count
                        );
                    }
                }
                Err(e) => {
                    crate::say_err!("[Kensaku] Failed to load semantic units: {}", e);
                }
            }
            let mut vstore = visual_store.write();
            let _ = db.load_visual_units_into(&mut vstore);
        }

        if content_rebuilt {
            let marked = metadata_store.write().mark_all_content_pending();
            crate::say_err!(
                "[Kensaku] Content index schema upgraded; {} file(s) queued for re-indexing",
                marked
            );
        }

        Self {
            metadata: metadata_store,
            content_index: Arc::new(RwLock::new(content_index)),
            unit_store,
            visual_store,
            embedding_queue: Arc::new(Mutex::new(None)),
            query_in_flight,
            config: Arc::new(RwLock::new(config)),
            status: Arc::new(RwLock::new(IndexingStatus {
                is_indexing: false,
                files_indexed: 0,
                content_indexed: 0,
                current_status: "Idle".to_string(),
                is_embedding: false,
                units_indexed: units_count,
                semantic_ready: units_count > 0,
                phase: "idle".to_string(),
                total_files: 0,
                extract_done: 0,
                extract_total: 0,
                paused: false,
            })),
            crawl_start_unix_ms: Arc::new(AtomicU64::new(0)),
            last_reconcile_unix_secs: Arc::new(AtomicU64::new(0)),
            watcher_ok: Arc::new(AtomicBool::new(false)),
            is_running: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            crawl_pending: Arc::new(AtomicBool::new(false)),
            watcher_handle: Arc::new(Mutex::new(None)),
            status_sink: Arc::new(RwLock::new(None)),
        }
    }

    /// Read-only handle: no writer lock or watcher, so it can coexist with a daemon.
    pub fn new_readonly(config: KensakuConfig) -> Self {
        crate::indexer::extract::set_external_extractors(config.extractors.clone());
        crate::indexer::extract::set_path_modes(&config.watched_dirs, &config.name_only_dirs);
        crate::indexer::extract::image::set_ocr_max_image_bytes(config.ocr_max_image_bytes);
        crate::indexer::extract::set_content_caps(config.content_max_file_size_mb, config.content_max_text_bytes, config.content_csv_head_rows);
        crate::embed::set_max_semantic_units(config.content_max_semantic_units);
        let index_dir = KensakuConfig::index_dir();
        let content_index = ContentIndex::open_read_only(&index_dir).ok();
        let metadata_store = Arc::new(RwLock::new(MetadataStore::new()));
        let unit_store = Arc::new(RwLock::new(UnitStore::new(EMBEDDING_DIM)));
        let visual_store = Arc::new(RwLock::new(crate::embed::visual::VisualStore::new(
            crate::embed::visual::VISUAL_DIM,
            crate::embed::visual::MOCK_VISUAL_MODEL_ID,
        )));
        let query_in_flight = Arc::new(AtomicBool::new(false));

        let mut units_count = 0;
        if let Some(db) = metadata_store.read().db() {
            let mut store = unit_store.write();
            if let Ok(count) = db.load_units_into(&mut store) {
                units_count = count;
            }
            let mut vstore = visual_store.write();
            let _ = db.load_visual_units_into(&mut vstore);
        }

        Self {
            metadata: metadata_store,
            content_index: Arc::new(RwLock::new(content_index)),
            unit_store,
            visual_store,
            embedding_queue: Arc::new(Mutex::new(None)),
            query_in_flight,
            config: Arc::new(RwLock::new(config)),
            status: Arc::new(RwLock::new(IndexingStatus {
                is_indexing: false,
                files_indexed: 0,
                content_indexed: 0,
                current_status: "Daemon-backed (read-only client)".to_string(),
                is_embedding: false,
                units_indexed: units_count,
                semantic_ready: units_count > 0,
                phase: "idle".to_string(),
                total_files: 0,
                extract_done: 0,
                extract_total: 0,
                paused: false,
            })),
            crawl_start_unix_ms: Arc::new(AtomicU64::new(0)),
            last_reconcile_unix_secs: Arc::new(AtomicU64::new(0)),
            watcher_ok: Arc::new(AtomicBool::new(false)),
            is_running: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            crawl_pending: Arc::new(AtomicBool::new(false)),
            watcher_handle: Arc::new(Mutex::new(None)),
            status_sink: Arc::new(RwLock::new(None)),
        }
    }

    /// Registers a push callback for every `IndexingStatus` update (pollers read the shared lock).
    pub fn set_status_listener(&self, listener: StatusListener) {
        *self.status_sink.write() = Some(listener);
    }

    fn emit_status(status_sink: &StatusSink, status: &IndexingStatus) {
        if let Some(listener) = status_sink.read().as_ref() {
            listener(status);
        }
    }

    /// Blocks extraction workers while paused; the watcher keeps applying renames/deletes.
    fn wait_while_paused(
        config: &Arc<RwLock<KensakuConfig>>,
        status: &Arc<RwLock<IndexingStatus>>,
        status_sink: &StatusSink,
    ) {
        loop {
            let paused = config.read().pause_indexing;
            if !paused {
                let mut st = status.write();
                if st.paused {
                    st.paused = false;
                    st.phase = "extracting".to_string();
                    st.current_status = "Resuming indexing…".to_string();
                    Self::emit_status(status_sink, &st);
                }
                return;
            }

            let mut st = status.write();
            if !st.paused {
                st.paused = true;
                st.phase = "paused".to_string();
                st.current_status = "Indexing paused".to_string();
                Self::emit_status(status_sink, &st);
            }
            drop(st);
            thread::sleep(Duration::from_millis(150));
        }
    }

    /// Holds extraction while a search is in flight. Polls so a stuck flag slows, never hangs, the crawl.
    fn yield_to_queries(query_in_flight: &Arc<AtomicBool>) {
        if !query_in_flight.load(Ordering::Relaxed) {
            return;
        }
        while query_in_flight.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Eco mode: limits how often extraction jobs start while on battery.
    fn pace_on_battery(
        config: &Arc<RwLock<KensakuConfig>>,
        battery: &Mutex<(Instant, bool)>,
        gate: &Mutex<Instant>,
    ) {
        let crawl_mode = config.read().crawl_mode;
        let should_pace = {
            let mut cached = battery.lock();
            if cached.0.elapsed() >= Duration::from_secs(30) {
                cached.1 = crate::util::power::should_pace(crawl_mode);
                cached.0 = Instant::now();
            }
            cached.1
        };
        if !should_pace {
            return;
        }

        let mut last = gate.lock();
        let interval = Duration::from_millis(75);
        if let Some(wait) = interval.checked_sub(last.elapsed()) {
            thread::sleep(wait);
        }
        *last = Instant::now();
    }

    pub fn start_indexing(&self) {
        // Never crawl before setup: the default roots (home directory) are the wrong thing to scan.
        if !self.config.read().is_setup_complete {
            return;
        }
        if !self
            .content_index
            .read()
            .as_ref()
            .map(|index| index.is_writable())
            .unwrap_or(false)
        {
            return;
        }
        if self.is_running.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);

        // Stop any previous watcher so directories are never watched twice.
        let previous_watcher = self.watcher_handle.lock().take();
        if let Some(mut handle) = previous_watcher {
            handle.stop();
        }
        self.watcher_ok.store(false, Ordering::SeqCst);

        let metadata = Arc::clone(&self.metadata);
        let content_index = Arc::clone(&self.content_index);
        let unit_store = Arc::clone(&self.unit_store);
        let visual_store = Arc::clone(&self.visual_store);
        let embedding_queue = Arc::clone(&self.embedding_queue);
        let query_in_flight = Arc::clone(&self.query_in_flight);
        let config_arc = Arc::clone(&self.config);
        let status = Arc::clone(&self.status);
        let is_running = Arc::clone(&self.is_running);
        let cancel_requested = Arc::clone(&self.cancel_requested);
        let watcher_handle = Arc::clone(&self.watcher_handle);
        let status_sink = Arc::clone(&self.status_sink);
        let crawl_start_unix_ms = Arc::clone(&self.crawl_start_unix_ms);
        let last_reconcile_unix_secs = Arc::clone(&self.last_reconcile_unix_secs);
        let watcher_ok = Arc::clone(&self.watcher_ok);

        thread::spawn(move || {
            let mut gate_guard = CrawlGateGuard::new(
                Arc::clone(&is_running),
                Arc::clone(&status),
                Arc::clone(&status_sink),
            );

            // Capture the OCR signature before the sync rewrites it, for the crawl report.
            let ocr_signature_before = read_ocr_signature();
            let ocr_queued = Self::sync_ocr_state(&config_arc, &metadata);
            // Reconcile name-only dirs before the walk so files leaving that mode are flagged pending.
            let name_only_queued = Self::sync_name_only_state(
                &config_arc,
                &metadata,
                &content_index,
                &unit_store,
                &visual_store,
            );

            let (
                watched_dirs,
                excluded_dirs,
                excluded_exts,
                crawl_mode,
                include_ignored,
                skip_hidden,
            ) = {
                let cfg = config_arc.read();
                (
                    cfg.watched_dirs.clone(),
                    cfg.excluded_dirs.clone(),
                    cfg.excluded_extensions.clone(),
                    cfg.crawl_mode,
                    cfg.include_ignored,
                    cfg.skip_hidden,
                )
            };

            // Balanced/eco run the crawl at idle priority; failures are advisory.
            crate::util::priority::apply(crawl_mode);

            crawl_start_unix_ms.store(now_unix_ms(), Ordering::Relaxed);
            watcher_ok.store(false, Ordering::Relaxed);

            // After the first scan, walking an unchanged tree shows no banner.
            let mut quiet = config_arc.read().first_scan_done;
            if quiet {
                status.write().paused = false;
            } else {
                let mut st = status.write();
                st.is_indexing = true;
                st.paused = false;
                st.phase = "walking".to_string();
                st.current_status = "Scanning files...".to_string();
                Self::emit_status(&status_sink, &st);
            }

            // Stop a previous embedding worker here (not on the IPC thread); restarted after the crawl.
            if let Some(mut queue) = embedding_queue.lock().take() {
                queue.stop();
            }

            crate::say!(
                "[Kensaku Indexer] Starting directory scan across {:?}",
                watched_dirs
            );

            let mut files_count: usize = 0;
            let mut content_count = 0;
            // Re-read reasons, counted for the crawl report.
            let mut new_files = 0usize;
            let mut restamped_files = 0usize;
            let mut pending_files = 0usize;
            let mut pending_commits = 0;
            let mut crawled_paths: HashSet<PathBuf> = HashSet::new();
            // OCR-toggle images processed by this crawl; their pending flag is cleared afterwards.
            let mut pending_images: HashSet<PathBuf> = HashSet::new();

            if let Some(db) = metadata.read().db() {
                let _ = db.begin_transaction();
            }

            // Phase 1: collect all entries.
            let all_entries = walk_directories_until(
                &watched_dirs,
                &excluded_dirs,
                include_ignored,
                skip_hidden,
                &cancel_requested,
            );
            let total_entries = all_entries.len();
            if !quiet {
                let mut st = status.write();
                st.total_files = total_entries;
                st.phase = "walking".to_string();
                st.current_status = format!("Indexing metadata for {} entries...", total_entries);
                Self::emit_status(&status_sink, &st);
            }

            // Phase 2a: filter, incremental check, metadata; collect files to extract.
            let mut to_extract: Vec<ExtractJob> = Vec::new();

            for entry in all_entries {
                if cancel_requested.load(Ordering::Relaxed) {
                    break;
                }
                files_count += 1;

                // Excluded extensions are not marked crawled, so previously indexed ones get swept.
                if !entry.is_dir
                    && let Some(ext) = entry.extension
                    && excluded_exts.iter().any(|e| e.eq_ignore_ascii_case(ext))
                {
                    continue;
                }

                crawled_paths.insert(entry.path.clone());

                // Read-lock check first so an unchanged tree never takes the write lock.
                let previous = { metadata.read().get(&entry.path) };
                let unchanged = previous
                    .as_ref()
                    .is_some_and(|existing| is_unchanged(existing, &entry));
                if unchanged {
                    continue;
                }
                let was_pending = previous
                    .as_ref()
                    .is_some_and(|existing| existing.content_pending);
                match previous.as_ref() {
                    None => new_files += 1,
                    Some(existing) => {
                        if was_pending {
                            pending_files += 1;
                        } else if existing.modified != entry.modified
                            || existing.size != entry.size
                        {
                            restamped_files += 1;
                        }
                    }
                }
                let phash_needs_refresh = previous.as_ref().is_none_or(|existing| {
                    existing.modified != entry.modified || existing.size != entry.size
                });
                let node_id = metadata.write().add(entry.clone());
                if was_pending {
                    pending_images.insert(entry.path.clone());
                }

                // Name-only: keep metadata, skip extraction, clear any pending flag so it is not retried forever.
                if !entry.is_dir && crate::indexer::extract::is_name_only_path(&entry.path) {
                    if was_pending {
                        metadata.write().clear_content_pending(&entry.path);
                        pending_images.remove(&entry.path);
                    }
                    if files_count.is_multiple_of(500) {
                        let mut st = status.write();
                        st.files_indexed = files_count;
                        st.content_indexed = content_count;
                        st.current_status = format!("Indexed {} items", files_count);
                        Self::emit_status(&status_sink, &st);
                    }
                    continue;
                }

                // Cloud placeholders stay metadata-only so indexing does not hydrate them.
                if !entry.is_dir
                    && (is_indexable_extension(entry.extension)
                        || is_indexable_file_name(&entry.name)
                        || crate::indexer::extract::external::handles(&entry.path))
                    && !is_cloud_placeholder_path(&entry.path)
                {
                    // Pending until extracted, so a crawl killed mid-way re-reads these next launch.
                    metadata.write().set_content_pending(&entry.path);
                    to_extract.push(ExtractJob {
                        node_id,
                        path: entry.path.clone(),
                        name: entry.name.clone(),
                        modified: entry.modified,
                        size: entry.size,
                        is_image: entry.extension.is_some_and(
                            crate::indexer::extract::image::is_supported_image_extension,
                        ),
                        phash_needs_refresh,
                    });
                }

                if !quiet && files_count.is_multiple_of(500) {
                    let mut st = status.write();
                    st.files_indexed = files_count;
                    st.content_indexed = content_count;
                    st.current_status = format!("Indexed {} items", files_count);
                    Self::emit_status(&status_sink, &st);
                }
            }

            // Text before images (OCR is the slow tail), newest first within each class.
            to_extract.sort_unstable_by_key(extract_sort_key);

            // Phase 2b: streaming extraction -> lexical index -> semantic index, each file read once.
            let extract_total = to_extract.len();
            if extract_total > 0 {
                quiet = false;
            }
            if !quiet {
                // Logged once: "0/N, 0% CPU" is ambiguous between never dispatched and blocked.
                crate::say!(
                    "[Kensaku Indexer] extraction: {} file(s) queued, {} worker(s), {} CPU(s) available",
                    extract_total,
                    crate::indexer::extract::extraction_concurrency(),
                    std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(0)
                );
                let mut st = status.write();
                st.is_indexing = true;
                st.phase = "extracting".to_string();
                st.files_indexed = files_count;
                st.content_indexed = content_count;
                st.total_files = total_entries;
                st.extract_total = extract_total;
                st.extract_done = 0;
                st.current_status =
                    format!("Extracting text from {} potential files...", extract_total);
                Self::emit_status(&status_sink, &st);
            }

            let semantic_enabled = config_arc.read().semantic_enabled;
            let model = semantic_enabled.then(|| ModelRegistry::global().clone());
            let embed_model_id = model.as_ref().map(|m| m.model_id);

            // Drop semantic units for files about to be re-extracted, in one batched pass.
            if semantic_enabled && !to_extract.is_empty() {
                let ids: HashSet<u64> = to_extract
                    .iter()
                    .map(|job| job.node_id)
                    .filter(|id| *id > 0)
                    .collect();
                if !ids.is_empty() {
                    let id_vec: Vec<u64> = ids.iter().copied().collect();
                    if let Some(db) = metadata.read().db() {
                        let _ = db.delete_units_for_nodes(&id_vec);
                        let _ = db.delete_visual_units_for_nodes(&id_vec);
                    }
                    unit_store.write().remove_units_for_nodes(&ids);
                    for id in &ids {
                        visual_store.write().remove_node(*id);
                    }
                }
            }

            let extract_done = AtomicUsize::new(0);
            let mut extracted_paths: HashSet<PathBuf> = HashSet::new();
            // Files whose extraction did not finish; they stay pending for the next crawl.
            let mut retry_paths: HashSet<PathBuf> = HashSet::new();
            let mut units_added = 0usize;

            // Bounded handoff: at most 32 extracted documents in flight.
            let (tx, rx) = std::sync::mpsc::sync_channel::<ExtractedWork>(32);
            let image_db = metadata.read().db();
            let db_handle = metadata.read().db();
            let thumbs_dir = KensakuConfig::config_dir().join("thumbs");
            let producer_status = Arc::clone(&status);
            let producer_app = Arc::clone(&status_sink);
            let producer_config = Arc::clone(&config_arc);
            let producer_cancel = Arc::clone(&cancel_requested);
            let battery_state = Arc::new(Mutex::new((Instant::now(), false)));
            let pace_gate = Arc::new(Mutex::new(Instant::now()));
            let producer_query_flag = Arc::clone(&query_in_flight);

            // Not `rayon::scope`: with a one-thread pool the producer would queue behind the
            // blocked consumer and never run. A scoped thread works regardless of pool size.
            std::thread::scope(|scope| {
                use rayon::prelude::*;

                scope.spawn(move || {
                    to_extract.par_iter().for_each_with(tx, |tx, job| {
                        if producer_cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        Self::wait_while_paused(&producer_config, &producer_status, &producer_app);
                        // Search first: a waiting user must not queue behind a scan.
                        Self::yield_to_queries(&producer_query_flag);
                        Self::pace_on_battery(&producer_config, &battery_state, &pace_gate);

                        // Dimensions only; perceptual hashing (full decode) is left to the backfill.
                        if job.is_image
                            && let Some((width, height)) =
                                crate::indexer::extract::image::dimensions(&job.path)
                            && let Some(db) = image_db.as_ref()
                        {
                            let _ = db.set_image_dimensions(&job.path, width, height);
                        }

                        // Use the configured text cap, not a fixed limit.
                        let (content, retry) = read_text_content_checked(
                            &job.path,
                            crate::indexer::extract::content_max_text_bytes() as u64,
                        );
                        let image_sig = job.is_image.then(|| {
                            content
                                .as_deref()
                                .map(crate::indexer::burst::line_signature)
                                .unwrap_or_default()
                        });
                        let image_phash = if job.is_image {
                            crate::indexer::extract::image::phash64_reusing_fresh_thumbnail(
                                &job.path,
                                &thumbs_dir,
                            )
                        } else {
                            None
                        };

                        // Embed here so the semantic tier never touches the file again.
                        let (units, content_hash) = match (model.as_ref(), content.as_deref()) {
                            (Some(model), Some(text)) if !text.trim().is_empty() => {
                                let chunks = chunk_text(text, job.size as usize);
                                let vectors: Vec<(usize, usize, Vec<i8>)> = chunks
                                    .into_par_iter()
                                    .map(|(start, end, chunk)| {
                                        (start, end, model.embed_to_i8(&chunk))
                                    })
                                    .collect();
                                (vectors, Some(format!("{:x}", fnv_hash(text))))
                            }
                            _ => (Vec::new(), None),
                        };

                        let done = extract_done.fetch_add(1, Ordering::Relaxed) + 1;
                        // Report the first completion too, or small batches look stalled at 0/N.
                        if done == 1 || done.is_multiple_of(5) || done == extract_total {
                            let mut st = producer_status.write();
                            st.extract_done = done;
                            st.current_status =
                                format!("Extracting text… {}/{} files", done, extract_total);
                            Self::emit_status(&producer_app, &st);
                        }

                        // The consumer is only gone during shutdown; dropping is safe.
                        let _ = tx.send(ExtractedWork {
                            node_id: job.node_id,
                            path: job.path.clone(),
                            name: job.name.clone(),
                            modified: job.modified,
                            size: job.size,
                            content,
                            units,
                            content_hash,
                            image_sig,
                            image_phash,
                            phash_needs_refresh: job.phash_needs_refresh,
                            retry,
                        });
                    });
                });

                // Single writer for both indexes (Tantivy requires one).
                let index_guard = content_index.read();
                let c_index = index_guard.as_ref();
                for work in rx {
                    if let Some(ref content) = work.content
                        && let Some(c_index) = c_index
                    {
                        let _ = c_index.add_or_update(
                            &work.path,
                            &work.name,
                            content,
                            work.modified,
                            work.size,
                            work.path.extension().and_then(|ext| ext.to_str()),
                        );
                        content_count += 1;
                        pending_commits += 1;

                        // Publish live counts rather than waiting for a commit.
                        if content_count.is_multiple_of(64) {
                            let mut st = status.write();
                            st.files_indexed = files_count;
                            st.content_indexed = content_count;
                            Self::emit_status(&status_sink, &st);
                        }
                    }

                    if let Some(signature) = work.image_sig.as_deref()
                        && let Some(db) = db_handle.as_ref()
                    {
                        if let Ok(updates) = db.set_image_text_sig(&work.path, signature) {
                            if !updates.is_empty() {
                                metadata.write().apply_bursts(&updates);
                            }
                            if let Some(phash) = work.image_phash
                                && phash != u64::MAX
                            {
                                let _ = db.set_image_phash(&work.path, phash);
                                match db.assign_burst_for_path(&work.path) {
                                    Ok(updates) if !updates.is_empty() => {
                                        metadata.write().apply_bursts(&updates);
                                    }
                                    _ => metadata.write().clear_burst_for_path(&work.path),
                                }
                            } else if work.phash_needs_refresh {
                                if let Ok(updates) = db.clear_image_hash(&work.path)
                                    && !updates.is_empty()
                                {
                                    metadata.write().apply_bursts(&updates);
                                }
                                metadata.write().clear_burst_for_path(&work.path);
                            } else {
                                // Pending OCR: pixels unchanged, keep the existing dHash.
                                match db.assign_burst_for_path(&work.path) {
                                    Ok(updates) if !updates.is_empty() => {
                                        metadata.write().apply_bursts(&updates);
                                    }
                                    _ => metadata.write().clear_burst_for_path(&work.path),
                                }
                            }
                        } else {
                            metadata.write().clear_burst_for_path(&work.path);
                        }
                    }

                    if work.retry {
                        retry_paths.insert(work.path.clone());
                    } else if pending_images.contains(&work.path) && work.content.is_some() {
                        extracted_paths.insert(work.path.clone());
                    }

                    if let Some(model_id) = embed_model_id
                        && let Some(db) = db_handle.as_ref()
                    {
                        for (start, end, vector) in &work.units {
                            if let Ok(id) =
                                db.insert_unit(work.node_id, *start, *end, model_id, vector)
                            {
                                let unit = Unit {
                                    id,
                                    node_id: work.node_id,
                                    byte_start: *start,
                                    byte_end: *end,
                                    model_id,
                                };
                                unit_store.write().add_unit(unit, vector);
                                units_added += 1;
                                if units_added.is_multiple_of(256) {
                                    let mut st = status.write();
                                    st.units_indexed = unit_store.read().len();
                                    st.semantic_ready = st.units_indexed > 0;
                                    Self::emit_status(&status_sink, &st);
                                }
                            }
                        }
                        let _ = db.update_node_indexed_gen(
                            work.node_id,
                            1,
                            work.content_hash.as_deref(),
                        );
                    }

                    // A retry re-sets the flag after the loop; pending images are cleared below.
                    if !work.retry && !pending_images.contains(&work.path) {
                        metadata.write().clear_content_pending(&work.path);
                    }

                    if pending_commits >= 512
                        && let Some(c_index) = c_index
                    {
                        // Search index first: a crash leaves files pending, never indexed-but-forgotten.
                        let _ = c_index.commit();
                        if let Some(db) = db_handle.as_ref()
                            && let Err(e) = db.commit_and_reopen()
                        {
                            crate::say_err!("[Kensaku Indexer] mid-crawl commit failed: {e}");
                        }
                        pending_commits = 0;
                        let mut st = status.write();
                        st.content_indexed = content_count;
                        Self::emit_status(&status_sink, &st);
                    }
                }
            });

            if !quiet {
                let mut st = status.write();
                st.extract_done = extract_total;
                st.content_indexed = content_count;
                if semantic_enabled {
                    st.units_indexed = unit_store.read().len();
                    st.semantic_ready = st.units_indexed > 0;
                }
                st.phase = "committing".to_string();
                st.current_status = "Updating search index...".to_string();
                Self::emit_status(&status_sink, &st);
            }

            // Re-scanned pending images are done; drop stale OCR docs when OCR is off.
            // Incomplete reads keep their old text and stay pending.
            let ocr_on = crate::indexer::extract::ocr_enabled();
            for path in &pending_images {
                if retry_paths.contains(path) {
                    continue;
                }
                if !extracted_paths.contains(path)
                    && !ocr_on
                    && let Some(c_index) = content_index.read().as_ref()
                {
                    let _ = c_index.remove(path);
                }
                metadata.write().clear_content_pending(path);
            }
            // Bounded retry; after the cap a file waits until it or the OCR settings change.
            let mut given_up = 0usize;
            for path in &retry_paths {
                let keep_trying = metadata
                    .read()
                    .db()
                    .map(|db| db.record_retry(path, MAX_EXTRACTION_ATTEMPTS).unwrap_or(true))
                    .unwrap_or(true);
                if keep_trying {
                    metadata.write().set_content_pending(path);
                } else {
                    given_up += 1;
                }
            }
            if !retry_paths.is_empty() {
                crate::say!(
                    "[Kensaku Indexer] {} file(s) left pending for retry (timeout or busy){}",
                    retry_paths.len() - given_up,
                    if given_up > 0 {
                        format!(", {given_up} given up on after {MAX_EXTRACTION_ATTEMPTS} attempts")
                    } else {
                        String::new()
                    }
                );
            }

            if let Some(db) = metadata.read().db() {
                let _ = db.commit_transaction();
                // PASSIVE checkpoint never blocks, keeping the WAL from growing across crawls.
                db.checkpoint_passive();
            }

            // Keep data for unreadable roots (unplugged drive, unmounted share).
            let unavailable_roots: Vec<PathBuf> = watched_dirs
                .iter()
                .filter(|root| std::fs::read_dir(root).is_err())
                .cloned()
                .collect();
            let belongs_to_unavailable_root = |path: &std::path::Path| {
                unavailable_roots.iter().any(|root| path.starts_with(root))
            };

            if !unavailable_roots.is_empty() {
                let hint = format!(
                    "{} roots unavailable, index preserved",
                    unavailable_roots.len()
                );
                crate::say!("[Kensaku Indexer] {}: {:?}", hint, unavailable_roots);
                let mut st = status.write();
                st.current_status = hint;
                Self::emit_status(&status_sink, &st);
            }

            // An unreadable parent directory is not a deletion; cached per directory.
            let mut parent_unreadable: HashMap<PathBuf, bool> = HashMap::new();
            let mut is_protected_by_unreadable_parent = |path: &std::path::Path| -> bool {
                let Some(parent) = path.parent() else {
                    return false;
                };
                if let Some(known) = parent_unreadable.get(parent) {
                    return *known;
                }
                // Only NotFound means the parent is really gone.
                let unreadable = match std::fs::read_dir(parent) {
                    Ok(_) => false,
                    Err(error) => error.kind() != std::io::ErrorKind::NotFound,
                };
                parent_unreadable.insert(parent.to_path_buf(), unreadable);
                unreadable
            };

            // Drop entries no longer on disk, unless cancelled (not every file was seen).
            let crawl_cancelled = cancel_requested.load(Ordering::SeqCst);
            let stale_paths: Vec<PathBuf> = if crawl_cancelled {
                Vec::new()
            } else {
                let store = metadata.read();
                (0..store.len())
                    .filter_map(|i| {
                        let e = store.materialize(i as u32);
                        if crawled_paths.contains(&e.path)
                            || belongs_to_unavailable_root(&e.path)
                            || is_protected_by_unreadable_parent(&e.path)
                        {
                            return None;
                        }
                        Some(e.path)
                    })
                    .collect()
            };

            if !stale_paths.is_empty() {
                // Drop content docs for exactly those paths (no full index scan).
                if let Some(c_index) = content_index.read().as_ref() {
                    let _ = c_index.remove_many(&stale_paths);
                }

                let mut store = metadata.write();
                for path in &stale_paths {
                    store.remove(path);
                }
                crate::say!(
                    "[Kensaku Indexer] Removed {} stale metadata entries",
                    stale_paths.len()
                );
            }

            if let Some(c_index) = content_index.read().as_ref() {
                let _ = c_index.commit();
            }

            last_reconcile_unix_secs.store(now_unix_secs(), Ordering::Relaxed);

            // Record what this crawl did and why; reasons are checked widest first.
            {
                let rebuild_reason = content_index
                    .read()
                    .as_ref()
                    .and_then(|index| index.rebuild_reason.clone());
                // Name every contributing cause, not just the first.
                let mut causes: Vec<String> = Vec::new();
                if let Some(reason) = rebuild_reason {
                    causes.push(format!("content index rebuilt: {reason}"));
                }
                if ocr_queued > 0 {
                    causes.push(format!(
                        "OCR settings changed, {ocr_queued} image/PDF queued"
                    ));
                }
                if name_only_queued > 0 {
                    causes.push(format!(
                        "name-only directories changed, {name_only_queued} file(s) queued"
                    ));
                }
                if pending_files > 0 || given_up > 0 || !retry_paths.is_empty() {
                    causes.push(format!(
                        "{pending_files} left pending by an earlier scan, {} retried, {given_up} given up on",
                        retry_paths.len().saturating_sub(given_up)
                    ));
                }
                // An edit and an unstable measurement look identical, so report counts.
                if new_files > 0 || restamped_files > 0 {
                    causes.push(format!(
                        "{new_files} new, {restamped_files} with a different size or modification time"
                    ));
                }
                let reason = if causes.is_empty() {
                    "nothing changed".to_string()
                } else {
                    causes.join("; ")
                };

                crate::say!(
                    "[Kensaku Indexer] scan: {} entries, {} re-read: {}",
                    files_count,
                    content_count,
                    reason
                );

                write_crawl_report(&CrawlReport {
                    at: now_unix_secs(),
                    walked: files_count,
                    reindexed: content_count,
                    new_files,
                    retried: retry_paths.len().saturating_sub(given_up),
                    abandoned: given_up,
                    reason,
                    // Pre-sync value, so a change is visible.
                    ocr_signature_stored: ocr_signature_before.clone(),
                    ocr_signature_now: crate::ocr::signature(),
                });
            }

            // The first full crawl marks setup done.
            {
                let mut cfg = config_arc.write();
                if marks_first_scan_done(
                    cfg.is_setup_complete,
                    cfg.first_scan_done,
                    cancel_requested.load(Ordering::SeqCst),
                ) {
                    cfg.first_scan_done = true;
                    let _ = cfg.save();
                }
            }

            {
                let mut st = status.write();
                st.is_indexing = false;
                st.paused = false;
                st.phase = "idle".to_string();
                st.files_indexed = files_count;
                st.content_indexed = content_count;
                st.current_status = format!("Ready ({} files indexed)", files_count);
                Self::emit_status(&status_sink, &st);
            }

            crate::say!(
                "[Kensaku Indexer] Finished initial index: {} files ({} with full text)",
                files_count, content_count
            );

            let handle = start_file_watcher(
                watched_dirs,
                metadata.clone(),
                content_index.clone(),
                config_arc.clone(),
                Arc::clone(&watcher_ok),
            );
            *watcher_handle.lock() = Some(handle);

            if config_arc.read().semantic_enabled {
                let mut eq_guard = embedding_queue.lock();
                if eq_guard.is_none() {
                    let model = ModelRegistry::global().clone();
                    let mut q = EmbeddingQueue::new(
                        model,
                        Arc::clone(&unit_store),
                        Some(Arc::clone(&status)),
                        Some(Arc::clone(&status_sink)),
                    );
                    q.query_in_flight = Arc::clone(&query_in_flight);
                    if let Some(db) = metadata.read().db() {
                        q.start(db);
                    }
                    *eq_guard = Some(q);
                } else if let Some(ref mut q) = *eq_guard
                    && let Some(db) = metadata.read().db()
                {
                    q.start(db);
                }
            }

            is_running.store(false, Ordering::SeqCst);
            gate_guard.armed = false;

            // Safe here: the crawl transaction is committed and the worker pauses for crawls.
            start_phash_backfill(
                Arc::clone(&metadata),
                Arc::clone(&content_index),
                Arc::clone(&visual_store),
                Arc::clone(&is_running),
                Arc::clone(&query_in_flight),
            );
        });
    }

    /// Rebuilds the index in place (no restart) and starts a fresh crawl.
    pub fn rebuild_all(self: Arc<Self>) -> Result<(), String> {
        // The rebuild gate is the crawl gate: reject rather than race a running crawl.
        if self
            .is_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(
                "Indexing is already running — wait for the scan to finish, then rebuild."
                    .to_string(),
            );
        }

        let mut st = self.status.write();
        st.is_indexing = true;
        st.paused = false;
        st.phase = "rebuilding".to_string();
        st.files_indexed = 0;
        st.total_files = 0;
        st.extract_done = 0;
        st.extract_total = 0;
        st.current_status = "Rebuilding index…".to_string();
        Self::emit_status(&self.status_sink, &st);
        drop(st);

        thread::spawn(move || self.run_rebuild());
        Ok(())
    }

    /// Teardown half of `rebuild_all`, on its own thread because stopping the embedding worker joins it.
    fn run_rebuild(self: Arc<Self>) {
        // Hands the gate back on unwind; intentional exits settle and release it themselves.
        struct GateGuard {
            gate: Arc<AtomicBool>,
            status: Arc<RwLock<IndexingStatus>>,
            status_sink: StatusSink,
            settled: bool,
        }
        impl Drop for GateGuard {
            fn drop(&mut self) {
                if !self.settled {
                    let mut st = self.status.write();
                    st.is_indexing = false;
                    st.paused = false;
                    st.phase = "idle".to_string();
                    st.current_status = "Rebuild failed".to_string();
                    IndexerManager::emit_status(&self.status_sink, &st);
                }
                self.gate.store(false, Ordering::SeqCst);
            }
        }
        let mut guard = GateGuard {
            gate: Arc::clone(&self.is_running),
            status: Arc::clone(&self.status),
            status_sink: Arc::clone(&self.status_sink),
            settled: false,
        };

        // Freeze every writer touching the stores about to be wiped.
        if let Some(mut handle) = self.watcher_handle.lock().take() {
            handle.stop();
        }
        self.watcher_ok.store(false, Ordering::SeqCst);
        if let Some(mut queue) = self.embedding_queue.lock().take() {
            queue.stop();
        }

        {
            let mut st = self.status.write();
            st.current_status = "Rebuilding index — clearing old data…".to_string();
            Self::emit_status(&self.status_sink, &st);
        }

        // Drops the Tantivy writer/reader so nothing locks the index directory (Windows).
        *self.content_index.write() = None;

        // Wipe the DB via the live connection (plain SQL, immune to open handles), then swap in a fresh store.
        if let Some(db) = self.metadata.read().db() {
            let _ = db.reset_visual_index();
            let _ = db.clear();
        }
        *self.metadata.write() = MetadataStore::new();
        self.unit_store.write().clear();
        self.visual_store.write().clear();

        let index_dir = KensakuConfig::index_dir();
        // If a dropped mmap still holds a file (Windows), `truncate` purges the stale index instead.
        let reclaimed = std::fs::remove_dir_all(&index_dir).is_ok();
        let mut fresh_index = match ContentIndex::new(&index_dir) {
            Ok(index) => Some(index),
            Err(e) => {
                crate::say_err!(
                    "[Kensaku] Rebuild failed: could not recreate the content index: {}",
                    e
                );
                let mut st = self.status.write();
                st.is_indexing = false;
                st.phase = "error".to_string();
                st.current_status = format!("Rebuild failed: {}", e);
                Self::emit_status(&self.status_sink, &st);
                guard.settled = true;
                drop(guard);
                return;
            }
        };
        if !reclaimed
            && let Err(e) = fresh_index
                .as_mut()
                .expect("fresh index exists on the Ok path")
                .truncate()
        {
            // Purge failed: stale documents linger until a successful rebuild.
            crate::say_err!(
                "[Kensaku] Rebuild: could not purge the old content index: {}",
                e
            );
        }
        *self.content_index.write() = fresh_index;

        // Otherwise cached extractions would be served back into the fresh index.
        crate::indexer::extract::clear_cache();

        // Thumbnails regenerate during the crawl; best-effort.
        let _ = std::fs::remove_dir_all(KensakuConfig::config_dir().join("thumbs"));

        // Settle the guard and release the gate so `start_indexing` can re-take it.
        guard.settled = true;
        drop(guard);
        self.start_indexing();
    }

    pub fn get_status(&self) -> IndexingStatus {
        self.status.read().clone()
    }

    /// Runs a query against the live, in-process stores.
    pub fn search_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        with_snippets: bool,
    ) -> Result<SearchResponse, String> {
        let config = self.config.read();
        let aliases = config.alias_pairs();
        let metadata = self.metadata.read();
        let content = self.content_index.read();
        let units = self.unit_store.read();
        let visual = self.visual_store.read();
        Ok(
            SearchDispatcher::search_response_fast_sorted_with_snippets_and_visual(
                query,
                &metadata,
                &content,
                Some(&units),
                Some(&visual),
                config.semantic_enabled,
                limit,
                0,
                SortOrder::Best,
                &aliases,
                offset,
                with_snippets,
            ),
        )
    }

    /// Keyset-paginated exhaustive search: the page of matches strictly after `start_after`.
    pub fn search_page_after(
        &self,
        query: &str,
        limit: usize,
        start_after: &str,
        with_snippets: bool,
    ) -> Result<SearchResponse, String> {
        let config = self.config.read();
        let aliases = config.alias_pairs();
        let metadata = self.metadata.read();
        let content = self.content_index.read();
        let units = self.unit_store.read();
        Ok(SearchDispatcher::search_response_exhaustive_after(
            query,
            &metadata,
            &content,
            Some(&units),
            config.semantic_enabled,
            limit,
            0,
            &aliases,
            start_after,
            with_snippets,
        ))
    }

    pub fn find_similar_images(
        &self,
        path: &std::path::Path,
        limit: usize,
    ) -> Result<Vec<crate::embed::visual::SimilarImageResult>, String> {
        let metadata = self.metadata.read();
        let visual_store = self.visual_store.read();

        let (source_node_id, query_vec) = if let Some(entry) = metadata.get(path) {
            let vec_opt = visual_store.get_vector(entry.id).map(|v| v.to_vec());
            (Some(entry.id), vec_opt)
        } else {
            (None, None)
        };

        let query_vec = match query_vec {
            Some(v) => v,
            None => {
                let encoder = crate::embed::visual::active_encoder().ok_or_else(|| {
                    "Visual embeddings are not available in this build; install the visual model pack"
                        .to_string()
                })?;
                let thumbs_dir = KensakuConfig::config_dir().join("thumbs");
                let thumb = crate::indexer::extract::image::thumbnail_path(path, &thumbs_dir);
                if thumb.is_file()
                    && let Some(v) = encoder.embed_image_from_thumbnail(&thumb)
                {
                    v
                } else if path.is_file() {
                    // Budgeted decode, charged until embedding has consumed it.
                    let (img, _reserved) =
                        crate::indexer::extract::image::decode_for_recognition(path)
                            .ok_or_else(|| {
                                format!("Failed to open image {}", path.display())
                            })?;
                    encoder.embed_image(&img)
                } else {
                    return Err(format!(
                        "Image not found or not readable: {}",
                        path.display()
                    ));
                }
            }
        };

        let floor = 0.30f32;
        let hits = visual_store.find_similar(&query_vec, source_node_id, floor, limit);

        let mut results = Vec::new();
        for (node_id, similarity) in hits {
            if let Some(entry) = metadata.get_by_node_id(node_id) {
                let is_image = entry
                    .path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(crate::indexer::extract::image::is_supported_image_extension);
                if is_image {
                    results.push(crate::embed::visual::SimilarImageResult {
                        path: entry.path.to_string_lossy().to_string(),
                        filename: entry.name,
                        similarity,
                        file_size: entry.size,
                        modified: entry.modified,
                    });
                }
            }
        }

        Ok(results)
    }

    /// Compact live state for the daemon, CLI and MCP, without loading heavy stores.
    pub fn state_json(&self) -> Result<serde_json::Value, String> {
        let (files, dirs) = {
            let store = self.metadata.read();
            let mut files = 0u64;
            let mut dirs = 0u64;
            for index in 0..store.len() {
                if store.node(index as u32).is_dir() {
                    dirs += 1;
                } else {
                    files += 1;
                }
            }
            (files, dirs)
        };
        let content_docs = self
            .content_index
            .read()
            .as_ref()
            .map(|index| index.num_docs())
            .unwrap_or(0);
        let semantic_units = self.unit_store.read().len() as u64;
        let config = self.config.read();
        let roots: Vec<String> = config
            .watched_dirs
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let units_model_id = match self.metadata.read().db() {
            Some(db) => db.units_model_id()?,
            None => None,
        };
        let model_dir = KensakuConfig::config_dir().join("models");
        let real_model = model_dir.join("model.safetensors").is_file()
            && model_dir.join("tokenizer.json").is_file();
        let (active_model_id, model_name) = if real_model {
            (
                crate::embed::model::REAL_MODEL_ID,
                "potion-base-8M".to_string(),
            )
        } else {
            (
                crate::embed::model::DEFAULT_MODEL_ID,
                "static-embed-256".to_string(),
            )
        };
        let index_dir = KensakuConfig::index_dir();
        let index_bytes = dir_size_bytes(&index_dir).saturating_add(prefixed_file_bytes(
            &KensakuConfig::config_dir(),
            "metadata.db",
        ));
        let last_reconcile = last_modified_secs(&[
            KensakuConfig::config_dir().join("metadata.db"),
            index_dir.join("meta.json"),
        ]);
        let status = self.get_status();

        Ok(serde_json::json!({
            "files": files,
            "dirs": dirs,
            "content_docs": content_docs,
            "content_index_ready": self.content_index.read().is_some(),
            "semantic_units": semantic_units,
            "index_bytes": index_bytes,
            "model_id": units_model_id,
            "active_model_id": active_model_id,
            "model_name": model_name,
            "model_real": real_model,
            "roots": roots,
            "last_reconcile": last_reconcile,
            "is_indexing": status.is_indexing,
            "paused": status.paused,
            "phase": status.phase,
            "files_indexed": status.files_indexed,
            "content_indexed": status.content_indexed,
            "extract_done": status.extract_done,
            "extract_total": status.extract_total,
        }))
    }

    pub fn watcher_ok(&self) -> bool {
        self.watcher_ok.load(Ordering::Relaxed)
    }

    /// True while a crawl is in progress (the watcher is stopped during crawls).
    pub fn is_crawling(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    /// Reconciles the stored index with each file's effective names-only/content mode.
    /// Returns how many files were flagged for extraction.
    fn sync_name_only_state(
        config_arc: &Arc<RwLock<KensakuConfig>>,
        metadata: &Arc<RwLock<MetadataStore>>,
        content_index: &Arc<RwLock<Option<ContentIndex>>>,
        unit_store: &Arc<RwLock<UnitStore>>,
        visual_store: &Arc<RwLock<crate::embed::visual::VisualStore>>,
    ) -> usize {
        let current = {
            let cfg = config_arc.read();
            IndexModeState::from_config(&cfg)
        };
        let previous = read_index_mode_state();
        if previous.as_ref() == Some(&current) {
            return 0;
        }
        let previous = previous.unwrap_or_default();

        let (to_drop, to_drop_ids, to_read) = {
            let store = metadata.read();
            let mut to_drop: Vec<PathBuf> = Vec::new();
            let mut to_drop_ids: HashSet<u64> = HashSet::new();
            let mut to_read: Vec<PathBuf> = Vec::new();
            for i in 0..store.len() {
                let entry = store.materialize(i as u32);
                if entry.is_dir {
                    continue;
                }
                match (previous.mode_for(&entry.path), current.mode_for(&entry.path)) {
                    (IndexMode::NameAndContent, IndexMode::NamesOnly) => {
                        if entry.id > 0 {
                            to_drop_ids.insert(entry.id);
                        }
                        to_drop.push(entry.path);
                    }
                    (IndexMode::NamesOnly, IndexMode::NameAndContent) => {
                        to_read.push(entry.path);
                    }
                    _ => {}
                }
            }
            (to_drop, to_drop_ids, to_read)
        };

        if !to_drop.is_empty() {
            if let Some(c_index) = content_index.read().as_ref() {
                let _ = c_index.remove_many(&to_drop);
                let _ = c_index.commit();
            }
            if !to_drop_ids.is_empty() {
                let ids: Vec<u64> = to_drop_ids.iter().copied().collect();
                if let Some(db) = metadata.read().db() {
                    let _ = db.delete_units_for_nodes(&ids);
                    let _ = db.delete_visual_units_for_nodes(&ids);
                }
                unit_store.write().remove_units_for_nodes(&to_drop_ids);
                for id in &to_drop_ids {
                    visual_store.write().remove_node(*id);
                }
            }
            crate::say_err!(
                "[Kensaku] Names-only: dropped content of {} file(s)",
                to_drop.len()
            );
        }

        let marked = metadata.write().mark_paths_pending(&to_read);
        if marked > 0 {
            crate::say_err!(
                "[Kensaku] Paths enabled for content: {marked} file(s) queued for extraction"
            );
        }

        write_index_mode_state(&current);
        marked
    }

    /// Applies OCR config, re-queuing images/PDFs when the OCR signature changes; returns how many.
    fn sync_ocr_state(
        config_arc: &Arc<RwLock<KensakuConfig>>,
        metadata: &Arc<RwLock<MetadataStore>>,
    ) -> usize {
        let mut config = config_arc.read().clone();
        crate::indexer::extract::image::reset_tesseract_probe();
        crate::indexer::extract::set_ocr_enabled(config.ocr_enabled);
        crate::ocr::set_engine(&config.ocr_engine);
        if !config.ocr_enabled {
            return 0;
        }

        if crate::ocr::accurate_compiled() {
            if crate::ocr::engine_name() == crate::ocr::TESSERACT
                && !crate::indexer::extract::image::tesseract_usable()
            {
                config.ocr_engine = crate::ocr::ACCURATE.to_string();
                crate::ocr::set_engine(crate::ocr::ACCURATE);
                match config.save() {
                    Ok(()) => {
                        crate::say_err!("[Kensaku OCR] tesseract not found; switched to PP-OCR");
                        *config_arc.write() = config.clone();
                    }
                    Err(e) => crate::say_err!("[Kensaku OCR] could not save the engine switch: {e}"),
                }
            }
            if crate::ocr::engine_name() == crate::ocr::ACCURATE && !crate::ocr::pack_present() {
                let _ = crate::models::download_ocr_pack();
            }
            if crate::ocr::engine_name() == crate::ocr::INDRA && !crate::ocr_indra::pack_present() {
                let _ = crate::models::download_indra_pack();
            }
        }

        // Never downgrade a stored signature to "no engine": the probe can fail transiently.
        let signature = crate::ocr::signature();
        let stored = read_ocr_signature();
        if stored.as_deref() == Some(signature.as_str()) {
            return 0;
        }
        crate::say_err!(
            "[Kensaku OCR] state changed: was {:?}, now {:?}",
            stored.as_deref().unwrap_or("<none recorded>"),
            signature
        );
        if crate::ocr::usable_engine() == crate::ocr::NONE {
            crate::say_err!(
                "[Kensaku OCR] no OCR engine can run right now; keeping the recorded state so a \
                 temporary failure does not re-read every image"
            );
            return 0;
        }
        write_ocr_signature(&signature);
        crate::indexer::extract::clear_cache();
        let marked = metadata.write().mark_ocr_targets_pending();
        crate::say_err!(
            "[Kensaku OCR] using {} (revision {}); {} image/PDF file(s) queued for OCR",
            crate::ocr::usable_engine(),
            crate::ocr::OCR_REVISION,
            marked
        );
        marked
    }

    /// Clears semantic units from a different embedding model and starts a crawl to rebuild them.
    pub fn on_semantic_model_changed(self: &Arc<Self>) {
        let expected_model_id = crate::embed::ModelRegistry::global().model_id;
        if let Some(db) = self.metadata.read().db() {
            match db.units_model_id() {
                Ok(Some(existing)) if existing != expected_model_id => {
                    match db.reset_semantic_index() {
                        Ok(()) => {
                            self.unit_store.write().clear();
                            crate::say_err!(
                                "[Kensaku] Semantic model changed (units id {existing} -> {expected_model_id}); cleared semantic index for a rebuild"
                            );
                        }
                        Err(e) => crate::say_err!("[Kensaku] Failed to reset semantic index: {}", e),
                    }
                }
                Ok(_) => {}
                Err(e) => crate::say_err!("[Kensaku] Failed to read semantic model id: {}", e),
            }
        }
        self.start_indexing_when_idle();
    }

    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    /// Stops the first-run crawl and returns setup to folder selection.
    pub fn cancel_setup_scan(&self) -> Result<(), String> {
        self.request_cancel();
        let mut config = self.config.write();
        config.is_setup_complete = false;
        config.first_scan_done = false;
        config.save()
    }

    /// Starts a crawl now, or once the running one finishes; never before setup is done.
    pub fn start_indexing_when_idle(self: &Arc<Self>) {
        if !self.config.read().is_setup_complete {
            return;
        }
        if self.crawl_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let manager = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("kensaku-crawl-when-idle".into())
            .spawn(move || {
                while manager.is_running.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_secs(2));
                }
                manager.crawl_pending.store(false, Ordering::SeqCst);
                manager.start_indexing();
            });
        if spawned.is_err() {
            self.crawl_pending.store(false, Ordering::SeqCst);
        }
    }

    pub fn apply_config(&self, config: KensakuConfig) -> Result<(), String> {
        let previous = self.config.read().clone();
        let ocr_changed =
            previous.ocr_enabled != config.ocr_enabled || previous.ocr_engine != config.ocr_engine;
        let extractors_changed = previous.extractors != config.extractors;
        // Watched roots also change effective modes, so both lists trigger a reconcile.
        let mode_changed = previous.watched_dirs != config.watched_dirs
            || previous.name_only_dirs != config.name_only_dirs;
        // Raised caps only help truncated files, which are unchanged on disk and need a re-read.
        let caps_raised = config.content_max_file_size_mb > previous.content_max_file_size_mb
            || config.content_max_text_bytes > previous.content_max_text_bytes
            || config.content_max_semantic_units > previous.content_max_semantic_units;

        config.save()?;
        *self.config.write() = config.clone();
        crate::util::priority::set_mode(config.crawl_mode);
        crate::indexer::extract::image::set_ocr_max_image_bytes(config.ocr_max_image_bytes);
        crate::indexer::extract::set_content_caps(config.content_max_file_size_mb, config.content_max_text_bytes, config.content_csv_head_rows);
        crate::embed::set_max_semantic_units(config.content_max_semantic_units);

        let mut reindex = false;
        if mode_changed {
            crate::indexer::extract::set_path_modes(&config.watched_dirs, &config.name_only_dirs);
            crate::indexer::extract::clear_cache();
            reindex = true;
        }
        if caps_raised {
            crate::indexer::extract::clear_cache();
            let marked = self.metadata.write().mark_all_content_pending();
            crate::say_err!("[Kensaku] Content caps raised: {marked} file(s) queued for re-read");
            reindex = true;
        }
        if ocr_changed {
            crate::indexer::extract::set_ocr_enabled(config.ocr_enabled);
            crate::ocr::set_engine(&config.ocr_engine);
            crate::indexer::extract::clear_cache();
            let marked = self.metadata.write().mark_ocr_targets_pending();
            crate::say_err!(
                "[Kensaku] OCR {}: {} image(s) queued for re-scan",
                if config.ocr_enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                marked
            );
            reindex = true;
        }

        if extractors_changed {
            crate::indexer::extract::set_external_extractors(config.extractors.clone());
            let active: Vec<_> = previous
                .extractors
                .iter()
                .chain(config.extractors.iter())
                .filter(|extractor| extractor.enabled)
                .collect();
            if !active.is_empty() {
                let extensions: Vec<String> = active
                    .iter()
                    .flat_map(|extractor| extractor.extensions.iter().cloned())
                    .collect();
                let broad = active.iter().any(|extractor| {
                    extractor
                        .mime
                        .iter()
                        .any(|selector| !selector.trim().is_empty())
                        || extractor
                            .extensions
                            .iter()
                            .any(|selector| selector.trim().trim_start_matches('.') == "*")
                });
                let marked = if broad || extensions.is_empty() {
                    self.metadata.write().mark_all_content_pending()
                } else {
                    self.metadata.write().mark_extensions_pending(&extensions)
                };
                crate::say_err!(
                    "[Kensaku] External extractors changed; {} file(s) queued for re-scan",
                    marked
                );
                reindex = true;
            }
        }

        // An OCR engine became usable without a config change; `sync_ocr_state` re-queues.
        if !reindex
            && config.ocr_enabled
            && read_ocr_signature().as_deref() != Some(crate::ocr::signature().as_str())
        {
            reindex = true;
        }

        if reindex {
            self.start_indexing();
        }
        Ok(())
    }
}

/// What the last crawl did, written so the reason for a re-read is visible afterwards.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrawlReport {
    pub at: u64,
    /// Files and folders the walk visited.
    pub walked: usize,
    /// Files whose content was extracted (actually re-read).
    pub reindexed: usize,
    /// Files absent from the content index before this crawl.
    #[serde(default)]
    pub new_files: usize,
    /// Files left pending for a retry after this crawl.
    pub retried: usize,
    /// Files dropped from the retry loop (retry budget exhausted).
    pub abandoned: usize,
    /// Why anything was reindexed ("nothing changed" if nothing was).
    pub reason: String,
    /// OCR state as recorded vs. computed now; equal means no OCR re-read was due.
    #[serde(default)]
    pub ocr_signature_stored: Option<String>,
    #[serde(default)]
    pub ocr_signature_now: String,
}

fn crawl_report_path() -> std::path::PathBuf {
    KensakuConfig::config_dir().join("crawl-last.json")
}

fn write_crawl_report(report: &CrawlReport) {
    match serde_json::to_string_pretty(report) {
        Ok(json) => {
            if let Err(e) = std::fs::write(crawl_report_path(), json) {
                crate::say_err!("[Kensaku] could not record the crawl report: {e}");
            }
        }
        Err(e) => crate::say_err!("[Kensaku] could not serialise the crawl report: {e}"),
    }
}

pub fn last_crawl_report() -> Option<CrawlReport> {
    let raw = std::fs::read_to_string(crawl_report_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

fn ocr_signature_path() -> PathBuf {
    KensakuConfig::config_dir().join("ocr-state")
}

fn name_only_state_path() -> PathBuf {
    KensakuConfig::config_dir().join("name-only-state")
}

/// The directory rules the index was last reconciled with.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct IndexModeState {
    watched: Vec<String>,
    names_only: Vec<String>,
}

impl IndexModeState {
    fn from_config(config: &KensakuConfig) -> Self {
        let mut watched: Vec<String> = config
            .watched_dirs
            .iter()
            .map(|p| crate::indexer::extract::rule_key(p))
            .collect();
        let mut names_only: Vec<String> = config
            .name_only_dirs
            .iter()
            .map(|p| crate::indexer::extract::rule_key(p))
            .collect();
        watched.sort();
        watched.dedup();
        names_only.sort();
        names_only.dedup();
        Self { watched, names_only }
    }

    fn mode_for(&self, path: &Path) -> IndexMode {
        crate::indexer::extract::mode_from_rules(&self.watched, &self.names_only, path)
    }
}

/// Reads the last reconciled rules; also accepts the legacy bare `Vec<PathBuf>` shape.
fn read_index_mode_state() -> Option<IndexModeState> {
    let raw = std::fs::read_to_string(name_only_state_path()).ok()?;
    if let Ok(state) = serde_json::from_str::<IndexModeState>(&raw) {
        return Some(state);
    }
    let legacy: Vec<PathBuf> = serde_json::from_str(&raw).ok()?;
    let mut names_only: Vec<String> = legacy
        .iter()
        .map(|p| crate::indexer::extract::rule_key(p))
        .collect();
    names_only.sort();
    names_only.dedup();
    Some(IndexModeState {
        watched: Vec::new(),
        names_only,
    })
}

fn write_index_mode_state(state: &IndexModeState) {
    match serde_json::to_string(state) {
        Ok(raw) => {
            if let Err(e) = std::fs::write(name_only_state_path(), raw) {
                crate::say_err!("[Kensaku] could not record index-mode state: {e}");
            }
        }
        Err(e) => crate::say_err!("[Kensaku] could not encode index-mode state: {e}"),
    }
}

/// Where the OCR signature of the current index is recorded.
fn read_ocr_signature() -> Option<String> {
    std::fs::read_to_string(ocr_signature_path())
        .ok()
        .map(|s| s.trim().to_string())
}

fn write_ocr_signature(signature: &str) {
    if let Err(e) = std::fs::write(ocr_signature_path(), signature) {
        crate::say_err!("[Kensaku OCR] could not record OCR state: {e}");
    }
}

fn dir_size_bytes(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            total = total.saturating_add(dir_size_bytes(&entry.path()));
        } else if file_type.is_file() {
            total = total.saturating_add(entry.metadata().map(|meta| meta.len()).unwrap_or(0));
        }
    }
    total
}

fn prefixed_file_bytes(dir: &std::path::Path, prefix: &str) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
        if entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            total = total.saturating_add(entry.metadata().map(|meta| meta.len()).unwrap_or(0));
        }
    }
    total
}

fn last_modified_secs(paths: &[PathBuf]) -> Option<u64> {
    paths
        .iter()
        .filter_map(|path| {
            std::fs::metadata(path)
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(modified: u64, size: u64, content_pending: bool) -> FileEntry {
        FileEntry {
            id: 0,
            path: PathBuf::from("/tmp/kensaku/example.bin"),
            name: "example.bin".to_string(),
            extension: Some("bin"),
            size,
            modified,
            is_dir: false,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            content_pending,
        }
    }

    #[test]
    fn cancelled_crawl_does_not_mark_first_scan_done() {
        assert!(marks_first_scan_done(true, false, false));
        assert!(!marks_first_scan_done(true, false, true));
        assert!(!marks_first_scan_done(true, true, false));
        assert!(!marks_first_scan_done(false, false, false));
    }

    #[test]
    fn crawl_orders_text_first_then_newest_then_smallest() {
        let job = |path: &str, modified: u64, size: u64, is_image: bool| ExtractJob {
            node_id: 0,
            path: PathBuf::from(path),
            name: path.to_string(),
            modified,
            size,
            is_image,
            phash_needs_refresh: false,
        };
        let mut jobs = [
            job("/old-image", 10, 10, true),
            job("/old-text", 10, 10, false),
            job("/new-text", 20, 50, false),
            job("/new-small-text", 20, 5, false),
        ];
        jobs.sort_unstable_by_key(extract_sort_key);
        let order: Vec<&str> = jobs.iter().map(|job| job.path.to_str().unwrap()).collect();
        assert_eq!(
            order,
            vec!["/new-small-text", "/new-text", "/old-text", "/old-image"]
        );
    }

    #[test]
    fn change_detection_honors_pending_content() {
        let existing = entry(100, 10, false);
        assert!(is_unchanged(&existing, &entry(100, 10, false)));
        assert!(!is_unchanged(&existing, &entry(101, 10, false)));
        assert!(!is_unchanged(&existing, &entry(100, 11, false)));

        let pending = entry(100, 10, true);
        assert!(
            !is_unchanged(&pending, &entry(100, 10, false)),
            "a pending entry must be re-processed even when mtime/size match"
        );
    }

    #[test]
    fn an_unstatable_file_is_not_treated_as_changed() {
        // (0, 0) means metadata() failed, not an empty 1970 file.
        let existing = entry(100, 10, false);
        assert!(
            is_unchanged(&existing, &entry(0, 0, false)),
            "an unreadable stat must not force a re-read"
        );

        // An emptied file keeps its mtime, so it is still detected.
        assert!(!is_unchanged(&existing, &entry(100, 0, false)));
    }

    #[test]
    fn find_similar_images_returns_matches_and_excludes_self() {
        use crate::agent::HeadlessIndex;
        use crate::embed::visual::{MockVisualEncoder, VisualEncoder};
        use crate::indexer::metadata_db::MetadataDb;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("kensaku-sim-mgr-{}", nanos));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let db = MetadataDb::open_in_dir(&temp_dir).unwrap();
        let path1 = temp_dir.join("image_1.png");
        let path2 = temp_dir.join("image_2.png");
        let path3 = temp_dir.join("unrelated.jpg");

        let entry1 = FileEntry {
            id: 1,
            path: path1.clone(),
            name: "image_1.png".to_string(),
            extension: Some("png"),
            size: 100,
            modified: 10,
            is_dir: false,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            content_pending: false,
        };
        let entry2 = FileEntry {
            id: 2,
            path: path2.clone(),
            name: "image_2.png".to_string(),
            extension: Some("png"),
            size: 100,
            modified: 10,
            is_dir: false,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            content_pending: false,
        };
        let entry3 = FileEntry {
            id: 3,
            path: path3.clone(),
            name: "unrelated.jpg".to_string(),
            extension: Some("jpg"),
            size: 100,
            modified: 10,
            is_dir: false,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            content_pending: false,
        };

        db.upsert(&entry1).unwrap();
        db.upsert(&entry2).unwrap();
        db.upsert(&entry3).unwrap();

        let id1 = db.get_node_id_by_path(&path1).unwrap().unwrap();
        let id2 = db.get_node_id_by_path(&path2).unwrap().unwrap();
        let id3 = db.get_node_id_by_path(&path3).unwrap().unwrap();

        let encoder = MockVisualEncoder;
        let vec_base = encoder.embed_text("same concept");
        let inverted_vec: Vec<i8> = vec_base.iter().map(|&v| -v).collect();

        db.save_visual_unit(id1, encoder.model_id(), 0x1111, &vec_base)
            .unwrap();
        db.save_visual_unit(id2, encoder.model_id(), 0x2222, &vec_base)
            .unwrap();
        db.save_visual_unit(id3, encoder.model_id(), 0x3333, &inverted_vec)
            .unwrap();

        let index = HeadlessIndex::open_at(&temp_dir).unwrap();

        let results = index
            .find_similar_images_local(path1.to_str().unwrap(), 10)
            .unwrap();

        assert!(results.iter().all(|r| r.path != path1.to_string_lossy()));

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, path2.to_string_lossy());
        assert!(results[0].similarity >= 0.99);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    fn crawling_status() -> IndexingStatus {
        IndexingStatus {
            is_indexing: true,
            files_indexed: 12,
            content_indexed: 0,
            current_status: "Extracting text… 0/3 files".to_string(),
            is_embedding: false,
            units_indexed: 0,
            semantic_ready: false,
            phase: "extracting".to_string(),
            total_files: 3,
            extract_done: 0,
            extract_total: 3,
            paused: false,
        }
    }

    #[test]
    fn panicking_crawl_releases_the_gate_and_clears_the_phase() {
        // A panicking crawl must release the gate and reset the phase.
        let gate = Arc::new(AtomicBool::new(true));
        let status = Arc::new(RwLock::new(crawling_status()));
        let handle = Arc::new(RwLock::new(None));

        let (gate_in, status_in, handle_in) =
            (Arc::clone(&gate), Arc::clone(&status), Arc::clone(&handle));
        let crawl = thread::spawn(move || {
            let _guard = CrawlGateGuard::new(gate_in, status_in, handle_in);
            panic!("simulated extractor panic");
        });
        assert!(crawl.join().is_err(), "the simulated crawl must panic");

        assert!(!gate.load(Ordering::SeqCst), "gate must be released");
        let st = status.read();
        assert!(!st.is_indexing);
        assert_eq!(st.phase, "idle");
        assert_eq!(st.extract_total, 0);
        assert_eq!(st.extract_done, 0);
        assert!(st.current_status.contains("stopped unexpectedly"));
    }

    #[test]
    fn completed_crawl_keeps_the_status_it_set() {
        let gate = Arc::new(AtomicBool::new(true));
        let status = Arc::new(RwLock::new(crawling_status()));
        let handle = Arc::new(RwLock::new(None));
        let status_in = Arc::clone(&status);

        let crawl = thread::spawn(move || {
            let mut guard = CrawlGateGuard::new(gate, status_in, handle);
            {
                let mut st = guard.status.write();
                st.is_indexing = false;
                st.phase = "idle".to_string();
                st.current_status = "Ready (3 files indexed)".to_string();
            }
            gate_for_normal_path(&mut guard);
        });
        crawl.join().expect("a clean crawl must not panic");

        let st = status.read();
        assert_eq!(st.current_status, "Ready (3 files indexed)");
        assert_eq!(st.phase, "idle");
    }

    fn gate_for_normal_path(guard: &mut CrawlGateGuard) {
        guard.gate.store(false, Ordering::SeqCst);
        guard.armed = false;
    }
}
