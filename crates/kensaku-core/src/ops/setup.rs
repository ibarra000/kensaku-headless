//! First-run setup and folder management.
use crate::config::KensakuConfig;
use crate::indexer::IndexerManager;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

const OCR_ENGINES: [&str; 3] = [crate::ocr::TESSERACT, crate::ocr::ACCURATE, crate::ocr::INDRA];

const SUGGESTED_HOME_FOLDERS: [&str; 4] = ["Documents", "Downloads", "Desktop", "Pictures"];

/// First-run setup input; paths go through [`validate_directory`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetupRequest {
    /// Folders indexed fully (names + content).
    pub watched_dirs: Vec<String>,
    /// Name-only folders; `None` keeps the current list.
    #[serde(default)]
    pub name_only_dirs: Option<Vec<String>>,
    #[serde(default)]
    pub excluded_dirs: Vec<String>,
    /// `tesseract`, `accurate` or `indra`; `None` keeps the current engine.
    #[serde(default)]
    pub ocr_engine: Option<String>,
}

/// Existing common folders under home (never home itself).
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

/// Canonicalizes a user-typed directory (`~`, quotes, whitespace); rejects non-directories.
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
    Ok(simplify_verbatim(&canonical))
}

/// Turns Windows' verbatim `canonicalize()` output into an ordinary path:
/// `\\?\UNC\srv\share\x` -> `\\srv\share\x` (mapped drives resolve to UNC) and
/// `\\?\C:\x` -> `C:\x`. Other forms (e.g. `\\?\Volume{..}`) are left verbatim.
pub fn simplify_verbatim(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest) if is_drive_path(rest) => rest.to_string(),
        _ => path.to_string(),
    }
}

/// `C:\...` or bare `C:`.
fn is_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'\\')
}

fn validate_all(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    paths
        .iter()
        .map(|p| validate_directory(p).map(PathBuf::from))
        .collect()
}

/// Builds the post-setup config without touching disk.
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

/// Saves setup choices and starts the first crawl.
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

/// Marks setup incomplete so the next `setup` starts fresh.
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
    fn simplify_verbatim_rewrites_unc_to_double_backslash() {
        assert_eq!(
            simplify_verbatim(r"\\?\UNC\server\share\Docs"),
            r"\\server\share\Docs"
        );
    }

    #[test]
    fn simplify_verbatim_strips_drive_letter_prefix() {
        assert_eq!(simplify_verbatim(r"\\?\C:\Users\me"), r"C:\Users\me");
    }

    #[test]
    fn simplify_verbatim_leaves_other_paths_unchanged() {
        for path in [r"C:\Users\me", r"\\server\share", "/home/me", r"\\?\Volume{abc}\x"] {
            assert_eq!(simplify_verbatim(path), path);
        }
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
