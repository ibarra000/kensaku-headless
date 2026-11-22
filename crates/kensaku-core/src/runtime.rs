//! Headless runtime for the index owner: apply reset/rebuild markers, reclaim stale artifacts,
//! install extraction globals, take the writer lock, start the crawl and pack-download hooks.

use crate::config::KensakuConfig;
use crate::indexer::IndexerManager;
use crate::{crashlog, maintenance, models};
use std::fs::File;
use std::sync::Arc;

pub const WRITER_LOCK_FILE: &str = "kensaku.lock";

/// A running index owner; dropping it releases the writer lock.
pub struct Runtime {
    pub indexer: Arc<IndexerManager>,
    _writer_lock: File,
}

#[derive(Debug)]
pub enum StartError {
    /// Another process (normally the daemon) already owns the index.
    AlreadyRunning,
    Io(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "another Kensaku process owns the index"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StartError {}

impl Runtime {
    /// Takes the writer lock, prepares the data dir, opens stores and starts indexing.
    /// Only one per data directory; a second returns [`StartError::AlreadyRunning`].
    pub fn start() -> Result<Self, StartError> {
        let lock = acquire_writer_lock()?;
        prepare_data_dir();

        let config = KensakuConfig::load();
        crashlog::info(&format!(
            "config loaded: setup_complete={} ocr_enabled={} engine={}",
            config.is_setup_complete, config.ocr_enabled, config.ocr_engine
        ));
        apply_extraction_globals(&config);
        spawn_ocr_self_heal(&config);

        let indexer = Arc::new(IndexerManager::new(config.clone()));
        if config.is_setup_complete {
            indexer.start_indexing();
        }
        install_pack_hooks(&indexer);

        Ok(Self {
            indexer,
            _writer_lock: lock,
        })
    }
}

pub fn acquire_writer_lock() -> Result<File, StartError> {
    let dir = KensakuConfig::config_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| StartError::Io(format!("{}: {e}", dir.display())))?;
    let path = dir.join(WRITER_LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| StartError::Io(format!("{}: {e}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(StartError::AlreadyRunning),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(StartError::Io(format!("{}: {e}", path.display())))
        }
    }
}

/// Applies reset/rebuild markers and reclaims stale artifacts.
/// Must run before anything is opened: deleting open files leaves dangling handles.
pub fn prepare_data_dir() {
    crate::ops::stats::init_process_start();
    maybe_apply_pending_rebuild();
    maybe_apply_pending_reset();

    // Best-effort: never stop startup.
    let cleaned = maintenance::cleanup_stale_artifacts();
    if cleaned.bytes_reclaimed > 0 || !cleaned.errors.is_empty() {
        crate::say!(
            "[Kensaku] cleanup: removed {} legacy index backup(s), reclaimed {} bytes",
            cleaned.legacy_backups_removed,
            cleaned.bytes_reclaimed,
        );
        for error in &cleaned.errors {
            crate::say_err!("[Kensaku] cleanup: {error}");
        }
    }
}

/// Installs process-global OCR settings (on/off, languages, engine) not covered by `IndexerManager::new`.
pub fn apply_extraction_globals(config: &KensakuConfig) {
    crate::indexer::extract::set_ocr_enabled(config.ocr_enabled);
    crate::indexer::extract::image::set_ocr_max_image_bytes(config.ocr_max_image_bytes);
    crate::indexer::extract::set_ocr_languages(config.ocr_languages.clone());
    crate::indexer::extract::set_content_caps(
        config.content_max_file_size_mb,
        config.content_max_text_bytes,
        config.content_csv_head_rows,
    );
    crate::embed::set_max_semantic_units(config.content_max_semantic_units);
    crate::ocr::set_engine(&config.ocr_engine);
    crate::indexer::extract::set_external_extractors(config.extractors.clone());
    crate::indexer::extract::set_path_modes(&config.watched_dirs, &config.name_only_dirs);
}

/// Repairs missing OCR data (Tesseract langs, selected pack) in the background when OCR is enabled.
pub fn spawn_ocr_self_heal(config: &KensakuConfig) {
    if !config.ocr_enabled {
        return;
    }
    std::thread::spawn(|| {
        use crate::indexer::extract::image;
        if image::tesseract_available() && !image::tesseract_usable() {
            let _ = image::download_eng_tessdata();
        }
    });
    if crate::ocr::accurate_compiled() && crate::ocr::runtime_loadable() {
        // One download at a time: the selected engine's pack first.
        if crate::ocr::engine_name() == crate::ocr::INDRA && !crate::ocr_indra::pack_present() {
            let _ = models::download_indra_pack();
        } else if !crate::ocr::pack_present() {
            let _ = models::download_ocr_pack();
        }
    }
}

/// Re-crawls when the OCR pack lands; swaps in the semantic model when it finishes downloading.
fn install_pack_hooks(indexer: &Arc<IndexerManager>) {
    {
        let manager = Arc::clone(indexer);
        models::on_ocr_pack_ready(move || manager.start_indexing_when_idle());
    }
    {
        let manager = Arc::clone(indexer);
        models::on_semantic_models_ready(move || manager.on_semantic_model_changed());
    }
}

/// Schedules a full wipe applied by the next index owner before it opens anything; packs kept unless `remove_packs`.
pub fn request_reset(remove_packs: bool) -> Result<(), String> {
    let dir = KensakuConfig::config_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create config directory: {e}"))?;
    let scope = if remove_packs { "full" } else { "keep-packs" };
    std::fs::write(reset_marker_path(), scope.as_bytes())
        .map_err(|e| format!("Failed to write reset marker: {e}"))
}

fn reset_marker_path_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".reset-requested")
}

fn reset_marker_path() -> std::path::PathBuf {
    reset_marker_path_in(&KensakuConfig::config_dir())
}

pub fn reset_pending() -> bool {
    reset_marker_path().exists()
}

/// Reset retry budget; only used when deleting something failed.
const RESET_ATTEMPTS: u32 = 20;
const RESET_RETRY: std::time::Duration = std::time::Duration::from_millis(150);

/// Runs `reset` until success or the budget runs out; returns the last error.
fn reset_with_retries<F>(
    attempts: u32,
    delay: std::time::Duration,
    mut reset: F,
) -> Result<(), String>
where
    F: FnMut() -> Result<(), String>,
{
    let mut last = String::new();
    for attempt in 1..=attempts {
        match reset() {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = error;
                if attempt < attempts {
                    std::thread::sleep(delay);
                }
            }
        }
    }
    Err(last)
}

/// Applies a reset marker in `dir`; true when one was found and applied.
fn apply_pending_reset_in(dir: &std::path::Path) -> bool {
    let marker = reset_marker_path_in(dir);
    if !marker.exists() {
        return false;
    }

    // Downloaded packs survive a reset unless explicitly requested (`ResetScope`).
    let scope = maintenance::ResetScope {
        remove_packs: reset_marker_removes_packs(dir),
    };

    // The replacement process starts before the old one exits; Windows can't delete still-mapped files, so retry.
    let result = reset_with_retries(RESET_ATTEMPTS, RESET_RETRY, || {
        maintenance::apply_reset(dir, scope)
    });

    match result {
        Ok(()) => {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!(
                    "[Kensaku] Reset requested but could not recreate {}: {}",
                    dir.display(),
                    e
                );
                return true;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
            let _ = std::fs::remove_file(&marker);
            eprintln!(
                "[Kensaku] Reset requested: cleared user data in {}{}",
                dir.display(),
                if scope.remove_packs {
                    " (including downloaded packs)"
                } else {
                    " (downloaded packs kept)"
                }
            );
        }
        Err(e) => eprintln!(
            "[Kensaku] Reset requested but could not clear everything in {}: {}",
            dir.display(),
            e
        ),
    }
    true
}

/// True when the reset marker (`full`) asks for downloaded packs to go too.
fn reset_marker_removes_packs(dir: &std::path::Path) -> bool {
    std::fs::read_to_string(reset_marker_path_in(dir))
        .map(|contents| contents.trim().eq_ignore_ascii_case("full"))
        .unwrap_or(false)
}

fn rebuild_marker_path_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".rebuild-requested")
}

fn rebuild_marker_path() -> std::path::PathBuf {
    rebuild_marker_path_in(&KensakuConfig::config_dir())
}

pub fn rebuild_pending() -> bool {
    rebuild_marker_path().exists()
}

fn apply_pending_rebuild_in(dir: &std::path::Path) -> bool {
    let marker = rebuild_marker_path_in(dir);
    if !marker.exists() {
        return false;
    }
    // Keep settings (config.json); wipe only the index and metadata.
    let _ = std::fs::remove_dir_all(dir.join("index"));
    let _ = std::fs::remove_dir_all(dir.join("thumbs"));
    for name in ["metadata.db", "metadata.db-wal", "metadata.db-shm"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let _ = std::fs::remove_file(&marker);
    eprintln!("[Kensaku] Rebuild requested: index cleared in {}", dir.display());
    true
}

fn maybe_apply_pending_rebuild() {
    apply_pending_rebuild_in(&KensakuConfig::config_dir());
}

fn maybe_apply_pending_reset() {
    apply_pending_reset_in(&KensakuConfig::config_dir());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn pending_reset_clears_and_recreates_the_directory() {
        let dir = std::env::temp_dir().join(format!(
            "kensaku-reset-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("metadata.db"), b"stale").unwrap();
        std::fs::write(reset_marker_path_in(&dir), b"1").unwrap();
        assert!(reset_pending_at(&dir));

        assert!(apply_pending_reset_in(&dir));
        assert!(dir.is_dir());
        assert!(!reset_marker_path_in(&dir).exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        assert!(!apply_pending_reset_in(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_retries_while_the_old_process_still_holds_the_index() {
        let mut calls = 0;
        let result = reset_with_retries(5, std::time::Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                Err("index is mapped by another process".to_string())
            } else {
                Ok(())
            }
        });

        assert!(result.is_ok());
        assert_eq!(calls, 3, "it must stop retrying once the reset works");
    }

    #[test]
    fn reset_reports_the_last_error_when_every_attempt_fails() {
        let mut calls = 0;
        let result = reset_with_retries(3, std::time::Duration::ZERO, || {
            calls += 1;
            Err(format!("busy {calls}"))
        });

        assert_eq!(result, Err("busy 3".to_string()));
        assert_eq!(calls, 3);
    }

    #[test]
    fn reset_keeps_downloaded_packs_unless_asked_for_a_full_reset() {
        let dir = std::env::temp_dir().join(format!(
            "kensaku-reset-packs-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("models/embed.onnx"), b"pack").unwrap();
        std::fs::create_dir_all(dir.join("index")).unwrap();
        std::fs::write(dir.join("metadata.db"), b"stale").unwrap();

        std::fs::write(reset_marker_path_in(&dir), b"keep-packs").unwrap();
        assert!(apply_pending_reset_in(&dir));
        assert!(dir.join("models/embed.onnx").exists());
        assert!(!dir.join("metadata.db").exists());
        assert!(!dir.join("index").exists());

        std::fs::write(reset_marker_path_in(&dir), b"full").unwrap();
        assert!(apply_pending_reset_in(&dir));
        assert!(!dir.join("models").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn reset_pending_at(dir: &std::path::Path) -> bool {
        reset_marker_path_in(dir).exists()
    }
}
