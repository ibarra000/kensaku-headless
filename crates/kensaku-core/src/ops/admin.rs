//! Maintenance actions: doctor repairs, rebuild, reset.

use crate::indexer::IndexerManager;
use std::sync::Arc;

/// Repair actions [`doctor_fix`] accepts (names match `DoctorCheck::action`).
pub const DOCTOR_ACTIONS: [&str; 6] = [
    "download-models",
    "download-ocr-pack",
    "download-indra-pack",
    "cleanup-storage",
    "restart-watcher",
    "reindex",
];

/// Runs the repair a doctor check offers. Unknown actions are rejected so a
/// stale client cannot look like a successful repair.
pub fn doctor_fix(indexer: &Arc<IndexerManager>, action: &str) -> Result<String, String> {
    match action {
        "download-models" => {
            crate::models::download_all()?;
            Ok("Downloading model files. Semantic search upgrades when it finishes.".to_string())
        }
        "download-ocr-pack" => {
            crate::models::download_ocr_pack()?;
            Ok("Downloading the OCR pack.".to_string())
        }
        "download-indra-pack" => {
            crate::models::download_indra_pack()?;
            Ok("Downloading the Indra OCR pack.".to_string())
        }
        "cleanup-storage" => {
            let report = crate::maintenance::cleanup_stale_artifacts();
            let failures = if report.errors.is_empty() {
                String::new()
            } else {
                format!(" ({} item(s) could not be removed)", report.errors.len())
            };
            Ok(format!(
                "Reclaimed {}{failures}.",
                crate::doctor::format_bytes(report.bytes_reclaimed)
            ))
        }
        // A fresh crawl restarts the watcher at its end and re-reads anything
        // that changed while it was down.
        "restart-watcher" => {
            Arc::clone(indexer).rebuild_all()?;
            Ok("Restarting indexing; the watcher comes back when the scan finishes.".to_string())
        }
        "reindex" => {
            Arc::clone(indexer).rebuild_all()?;
            Ok("Rebuilding the index.".to_string())
        }
        other => Err(format!(
            "Unknown repair '{other}'; expected one of {}",
            DOCTOR_ACTIONS.join(", ")
        )),
    }
}

/// Sets or clears the persistent pause flag. Extraction workers block while
/// paused; the watcher keeps applying renames and deletes.
pub fn set_paused(indexer: &IndexerManager, paused: bool) -> Result<(), String> {
    let mut config = indexer.config.read().clone();
    config.pause_indexing = paused;
    indexer.apply_config(config)
}
