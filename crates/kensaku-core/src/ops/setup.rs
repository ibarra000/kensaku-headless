//! First-run setup and folder management without a wizard.

use crate::config::KensakuConfig;
use crate::indexer::IndexerManager;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// OCR engines accepted by [`complete_setup`].
const OCR_ENGINES: [&str; 3] = [crate::ocr::TESSERACT, crate::ocr::ACCURATE, crate::ocr::INDRA];

/// Folders shown as suggestions on first run.
const SUGGESTED_HOME_FOLDERS: [&str; 4] = ["Documents", "Downloads", "Desktop", "Pictures"];

/// Everything first-run setup needs. Paths are validated with
/// [`validate_directory`] before they are stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetupRequest {
    /// Folders to index fully (names + content).
    pub watched_dirs: Vec<String>,
    /// Folders indexed by name only. `None` keeps the current list.
    #[serde(default)]
    pub name_only_dirs: Option<Vec<String>>,
    /// Exclusion patterns/paths.
    #[serde(default)]
    pub excluded_dirs: Vec<String>,
    /// `tesseract`, `accurate` or `indra`. `None` keeps the current engine.
    #[serde(default)]
    pub ocr_engine: Option<String>,
}

/// Existing common folders under the home directory. Never includes home itself.
pub fn suggested_directories() -> Vec<String> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    SUGGESTED_HOME_FOLDERS
        .iter()
        .map(|name| home.join(name))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .collect()
}

/// Validates a user-typed folder path and returns its canonical form.
///
/// Accepts `~` expansion, quoted paths and stray whitespace, and refuses
/// anything that is not an existing directory.
pub fn validate_directory(path: &str) -> Result<String, String> {
    let trimmed = path.trim().trim_matches('"').trim();
    let resolved = if trimmed == "~" {
        dirs::home_dir()
    } else if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        dirs::home_dir().map(|home| home.join(rest))
    } else {
        Some(PathBuf::from(trimmed))
    };
    let Some(candidate) = resolved else {
        return Err("Could not resolve the home directory".to_string());
    };
    if !candidate.exists() {
        return Err(format!("Folder not found: {}", candidate.display()));
    }
    if !candidate.is_dir() {
        return Err(format!("Not a folder: {}", candidate.display()));
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|e| format!("Could not resolve {}: {}", candidate.display(), e))?
        .to_string_lossy()
        .to_string();
    // Windows canonicalize() returns an extended-length path (\\?\C:\...);
    // everything else expects the plain form.
    Ok(canonical
        .strip_prefix("\\\\?\\")
        .map(str::to_string)
        .unwrap_or(canonical))
}

fn validate_all(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    paths
        .iter()
        .map(|p| validate_directory(p).map(PathBuf::from))
        .collect()
}

/// Builds the post-setup config from `current` and `req` without touching disk.
pub fn setup_config(current: &KensakuConfig, req: &SetupRequest) -> Result<KensakuConfig, String> {
    if req.watched_dirs.is_empty() {
        return Err("Setup needs at least one folder to index".to_string());
    }
    let mut config = current.clone();
    config.watched_dirs = validate_all(&req.watched_dirs)?;
    if let Some(dirs) = &req.name_only_dirs {
        config.name_only_dirs = validate_all(dirs)?;
    }
    config.excluded_dirs = req.excluded_dirs.clone();
    if let Some(engine) = &req.ocr_engine {
        if !OCR_ENGINES.contains(&engine.as_str()) {
            return Err(format!(
                "Unknown OCR engine '{engine}'; expected one of {}",
                OCR_ENGINES.join(", ")
            ));
        }
        config.ocr_engine = engine.clone();
    }
    config.is_setup_complete = true;
    config.first_scan_done = false;
    Ok(config)
}

/// Saves the setup choices and starts the first crawl on the writer.
pub fn complete_setup(
    indexer: &Arc<IndexerManager>,
    req: &SetupRequest,
) -> Result<KensakuConfig, String> {
    let config = setup_config(&indexer.config.read(), req)?;
    config.save()?;
    crate::runtime::apply_extraction_globals(&config);
    *indexer.config.write() = config.clone();
    indexer.start_indexing_when_idle();
    Ok(config)
}

/// Marks setup incomplete so the next `setup` call starts fresh.
pub fn run_setup_again(indexer: &IndexerManager) -> Result<(), String> {
    indexer.request_cancel();
    let mut config = indexer.config.write();
    config.is_setup_complete = false;
    config.first_scan_done = false;
    config.save()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kensaku-setup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn validate_directory_rejects_missing_and_files() {
        let dir = temp_dir();
        let file = dir.join("f.txt");
        std::fs::write(&file, "x").unwrap();

        assert!(validate_directory(&dir.join("nope").to_string_lossy()).is_err());
        assert!(validate_directory(&file.to_string_lossy()).is_err());
        let ok = validate_directory(&format!("  \"{}\" ", dir.display())).unwrap();
        assert!(!ok.starts_with("\\\\?\\"));
        assert!(PathBuf::from(ok).is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_config_marks_setup_complete_and_validates_roots() {
        let dir = temp_dir();
        let req = SetupRequest {
            watched_dirs: vec![dir.to_string_lossy().to_string()],
            excluded_dirs: vec!["node_modules".into()],
            ocr_engine: Some(crate::ocr::TESSERACT.into()),
            ..Default::default()
        };
        let config = setup_config(&KensakuConfig::default(), &req).unwrap();

        assert!(config.is_setup_complete);
        assert!(!config.first_scan_done);
        assert_eq!(config.watched_dirs.len(), 1);
        assert_eq!(config.excluded_dirs, vec!["node_modules".to_string()]);
        assert_eq!(config.ocr_engine, crate::ocr::TESSERACT);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_config_rejects_empty_roots_and_unknown_engines() {
        let empty = SetupRequest::default();
        assert!(setup_config(&KensakuConfig::default(), &empty).is_err());

        let dir = temp_dir();
        let bad_engine = SetupRequest {
            watched_dirs: vec![dir.to_string_lossy().to_string()],
            ocr_engine: Some("magic".into()),
            ..Default::default()
        };
        assert!(setup_config(&KensakuConfig::default(), &bad_engine).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
