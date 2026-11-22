//! Data directory maintenance: inventory, stale-artifact cleanup, uninstall,
//! and resets that keep downloaded packs by default.

use crate::config::KensakuConfig;
use std::path::{Path, PathBuf};

/// Downloaded artifacts, not user state; a reset keeps them by default.
const PACK_DIRS: &[&str] = &["models", "ocr", "ocr-indra", "tessdata"];

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct DataInventory {
    pub config_dir: String,
    pub total_bytes: u64,
    pub index_bytes: u64,
    pub metadata_bytes: u64,
    pub thumbnail_bytes: u64,
    pub pack_bytes: u64,
    /// Content-schema backups left by past upgrades.
    pub legacy_backups: usize,
    pub legacy_backup_bytes: u64,
    /// Installers and partial downloads in the temp directory.
    pub update_temp_bytes: u64,
}

fn dir_size(path: &Path) -> u64 {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            total += dir_size(&entry.path());
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

pub fn update_temp_dir() -> PathBuf {
    std::env::temp_dir().join("kensaku-updates")
}

fn legacy_backup_dirs(config_dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(config_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("index.legacy-")
                })
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default();
    // Oldest first: the timestamp is part of the name.
    dirs.sort();
    dirs
}

pub fn inventory() -> DataInventory {
    let config_dir = KensakuConfig::config_dir();
    let legacy = legacy_backup_dirs(&config_dir);
    let legacy_bytes: u64 = legacy.iter().map(|dir| dir_size(dir)).sum();

    let pack_bytes = PACK_DIRS
        .iter()
        .map(|name| dir_size(&config_dir.join(name)))
        .sum();

    DataInventory {
        config_dir: config_dir.display().to_string(),
        total_bytes: dir_size(&config_dir),
        index_bytes: dir_size(&KensakuConfig::index_dir()),
        metadata_bytes: ["metadata.db", "metadata.db-wal", "metadata.db-shm"]
            .iter()
            .map(|name| file_size(&config_dir.join(name)))
            .sum(),
        thumbnail_bytes: dir_size(&config_dir.join("thumbs")),
        pack_bytes,
        legacy_backups: legacy.len(),
        legacy_backup_bytes: legacy_bytes,
        update_temp_bytes: dir_size(&update_temp_dir()),
    }
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CleanupReport {
    pub legacy_backups_removed: usize,
    pub bytes_reclaimed: u64,
    pub update_temp_cleared: bool,
    pub errors: Vec<String>,
}

/// Removes superseded index backups (keeping the newest) and staged installers; safe on every start.
pub fn cleanup_stale_artifacts() -> CleanupReport {
    let mut report = CleanupReport::default();
    let config_dir = KensakuConfig::config_dir();

    let mut backups = legacy_backup_dirs(&config_dir);
    if backups.len() > 1 {
        backups.pop();
        for dir in backups {
            let bytes = dir_size(&dir);
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => {
                    report.legacy_backups_removed += 1;
                    report.bytes_reclaimed += bytes;
                }
                Err(error) => report.errors.push(format!(
                    "could not remove {}: {}",
                    dir.display(),
                    error
                )),
            }
        }
    }

    let temp = update_temp_dir();
    if temp.is_dir() {
        let before = dir_size(&temp);
        match std::fs::remove_dir_all(&temp) {
            Ok(()) => {
                report.update_temp_cleared = true;
                report.bytes_reclaimed += before;
            }
            Err(error) => report.errors.push(format!(
                "could not clear {}: {}",
                temp.display(),
                error
            )),
        }
    }

    report
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RemovalReport {
    pub path: String,
    pub bytes: u64,
}

impl RemovalReport {
    /// e.g. "1.4 GB"
    pub fn human_bytes(&self) -> String {
        crate::doctor::format_bytes(self.bytes)
    }
}

/// Removes the data directory and the updater temp directory.
pub fn remove_all_data() -> Result<RemovalReport, String> {
    let config_dir = KensakuConfig::config_dir();
    let bytes = dir_size(&config_dir) + dir_size(&update_temp_dir());

    if config_dir.exists() {
        std::fs::remove_dir_all(&config_dir).map_err(|error| {
            format!(
                "could not remove {}: {}. Quit Kensaku, then run this again.",
                config_dir.display(),
                error
            )
        })?;
    }
    let _ = std::fs::remove_dir_all(update_temp_dir());

    Ok(RemovalReport {
        path: config_dir.display().to_string(),
        bytes,
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ResetScope {
    /// Also delete downloaded OCR and model packs.
    pub remove_packs: bool,
}

/// Clears user state; runs before any store is opened so no file is held open.
pub fn apply_reset(config_dir: &Path, scope: ResetScope) -> Result<(), String> {
    let entries = std::fs::read_dir(config_dir)
        .map_err(|error| format!("cannot read {}: {}", config_dir.display(), error))?;

    let mut failures: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // The caller consumes the marker.
        if name_str.starts_with(".reset-requested") || name_str.starts_with(".rebuild-requested") {
            continue;
        }
        if !scope.remove_packs && PACK_DIRS.contains(&name_str.as_ref()) {
            continue;
        }

        let path = entry.path();
        let result = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = result {
            failures.push(format!("{}: {}", path.display(), error));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "kensaku-maint-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reset_keeps_downloaded_packs_by_default() {
        let dir = temp_dir("reset-keep");
        std::fs::create_dir_all(dir.join("index")).unwrap();
        std::fs::write(dir.join("index/segment"), b"x").unwrap();
        std::fs::write(dir.join("config.json"), b"{}").unwrap();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("models/embed.onnx"), b"y").unwrap();
        std::fs::create_dir_all(dir.join("ocr-indra")).unwrap();
        std::fs::write(dir.join("ocr-indra/pack.onnx"), b"z").unwrap();

        apply_reset(&dir, ResetScope::default()).unwrap();

        assert!(!dir.join("index").exists(), "index should be cleared");
        assert!(!dir.join("config.json").exists(), "config should be cleared");
        assert!(dir.join("models").exists(), "model packs are not user data");
        assert!(dir.join("ocr-indra").exists(), "OCR packs are not user data");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_can_remove_packs_when_asked() {
        let dir = temp_dir("reset-packs");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("models/embed.onnx"), b"y").unwrap();

        apply_reset(
            &dir,
            ResetScope {
                remove_packs: true,
            },
        )
        .unwrap();

        assert!(!dir.join("models").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_keeps_the_marker_files_it_is_consuming() {
        let dir = temp_dir("reset-markers");
        std::fs::write(dir.join(".reset-requested"), b"1").unwrap();
        std::fs::write(dir.join("metadata.db"), b"db").unwrap();

        apply_reset(&dir, ResetScope::default()).unwrap();

        // The caller removes the marker only after success, so a failed reset stays recoverable.
        assert!(dir.join(".reset-requested").exists());
        assert!(!dir.join("metadata.db").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_keeps_only_the_newest_legacy_backup() {
        let dir = temp_dir("cleanup-legacy");
        for stamp in ["100", "200", "300"] {
            let backup = dir.join(format!("index.legacy-{stamp}"));
            std::fs::create_dir_all(&backup).unwrap();
            std::fs::write(backup.join("segment"), vec![0u8; 32]).unwrap();
        }

        let backups = legacy_backup_dirs(&dir);
        assert_eq!(backups.len(), 3);

        // cleanup_stale_artifacts uses the real config dir; test the rule directly.
        let mut to_remove = legacy_backup_dirs(&dir);
        to_remove.pop(); // keep newest
        assert_eq!(to_remove.len(), 2);
        assert!(to_remove.iter().all(|p| !p.ends_with("index.legacy-300")));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
