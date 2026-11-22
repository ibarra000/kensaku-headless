//! Per-file views: preview, details, thumbnails, and bounded text reads.

use crate::config::KensakuConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Upper bound on characters returned by [`read_text`].
const MAX_READ_TEXT_BYTES: u64 = 256_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePreview {
    pub path: String,
    pub filename: String,
    pub file_size: u64,
    pub modified: u64,
    pub is_dir: bool,
    pub preview_type: String,
    pub content: Option<String>,
    pub total_lines: Option<usize>,
    pub child_count: Option<usize>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

pub fn build_file_preview(path: &str) -> Result<FilePreview, String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err("File does not exist".to_string());
    }

    let meta = std::fs::metadata(p).map_err(|e| e.to_string())?;
    let filename = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let file_size = meta.len();
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let is_dir = meta.is_dir();

    if is_dir {
        let child_count = std::fs::read_dir(p).map(|rd| rd.count()).unwrap_or(0);
        return Ok(FilePreview {
            path: path.to_string(),
            filename,
            file_size,
            modified,
            is_dir: true,
            preview_type: "dir".to_string(),
            content: None,
            total_lines: None,
            child_count: Some(child_count),
            width: None,
            height: None,
        });
    }

    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Raster images are previewed by the webview directly. SVG/ICO are handled
    // the same way (the browser decodes them), but they have no raster
    // dimensions here. This check runs before any extraction so an image can
    // never reach tesseract on preview.
    let is_image = crate::indexer::extract::image::is_supported_image_extension(&ext)
        || matches!(ext.as_str(), "svg" | "ico");
    if is_image {
        let dimensions = crate::indexer::extract::image::dimensions(p);
        return Ok(FilePreview {
            path: path.to_string(),
            filename,
            file_size,
            modified,
            is_dir: false,
            preview_type: "image".to_string(),
            content: None,
            total_lines: None,
            child_count: None,
            width: dimensions.map(|(width, _)| width),
            height: dimensions.map(|(_, height)| height),
        });
    }

    if let Some(doc) = crate::indexer::extract::extract_document(p, 128_000) {
        let preview_type = match crate::indexer::extract::sniff_format(p) {
            crate::indexer::extract::DocumentFormat::Pdf => "pdf",
            crate::indexer::extract::DocumentFormat::Docx => "doc",
            crate::indexer::extract::DocumentFormat::Pptx => "presentation",
            crate::indexer::extract::DocumentFormat::Xlsx => "sheet",
            crate::indexer::extract::DocumentFormat::Epub
            | crate::indexer::extract::DocumentFormat::Odt
            | crate::indexer::extract::DocumentFormat::Rtf => "doc",
            _ => "text",
        };

        let total_lines = doc.text.lines().count();
        return Ok(FilePreview {
            path: path.to_string(),
            filename,
            file_size,
            modified,
            is_dir: false,
            preview_type: preview_type.to_string(),
            content: Some(doc.text),
            total_lines: Some(total_lines),
            child_count: doc.page_count,
            width: None,
            height: None,
        });
    }

    Ok(FilePreview {
        path: path.to_string(),
        filename,
        file_size,
        modified,
        is_dir: false,
        preview_type: "binary".to_string(),
        content: None,
        total_lines: None,
        child_count: None,
        width: None,
        height: None,
    })
}

/// Cache directory for generated image thumbnails.
pub fn thumbnail_cache_dir() -> PathBuf {
    KensakuConfig::config_dir().join("thumbs")
}

/// Creates the thumbnail cache directory with owner-only permissions.
fn ensure_thumbnail_cache_dir() -> Result<PathBuf, String> {
    let dir = thumbnail_cache_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create thumbnail cache directory: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Failed to secure thumbnail cache directory: {}", e))?;
    }
    Ok(dir)
}

pub fn generate_thumbnail(path: &str) -> Result<String, String> {
    let source = Path::new(path);
    let dir = ensure_thumbnail_cache_dir()?;
    let preview = crate::preview::preview_jpeg(source, &dir)?;
    preview
        .to_str()
        .map(|p| p.to_string())
        .ok_or_else(|| "Preview path is not valid UTF-8".to_string())
}

/// Extra facts for the hover card, fetched on demand. Dimensions are read
/// only for image files, so this never runs during search.
#[derive(Debug, Clone, Serialize)]
pub struct FileDetails {
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Image dimensions for `path` (only probed for image extensions).
pub fn file_details(path: &str) -> FileDetails {
    let p = Path::new(path);
    let is_image = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            crate::indexer::extract::image::is_supported_image_extension(&e.to_ascii_lowercase())
        })
        .unwrap_or(false);
    let dims = if is_image {
        crate::indexer::extract::image::dimensions(p)
    } else {
        None
    };
    FileDetails {
        width: dims.map(|(w, _)| w),
        height: dims.map(|(_, h)| h),
    }
}

/// Reads up to 256 KB of extracted text from `path`.
pub fn read_text(path: &str) -> Result<String, String> {
    crate::indexer::content::read_text_content(Path::new(path), MAX_READ_TEXT_BYTES)
        .ok_or_else(|| "Could not read text content from file".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(extension: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "kensaku-commands-test-{}-{}.{}",
            std::process::id(),
            nanos,
            extension
        ))
    }

    #[test]
    fn image_preview_returns_dimensions_and_no_content() {
        let path = temp_path("png");
        ::image::RgbImage::from_pixel(12, 7, ::image::Rgb([10, 20, 30]))
            .save_with_format(&path, ::image::ImageFormat::Png)
            .expect("write test png");

        let preview = build_file_preview(path.to_str().unwrap()).expect("preview");

        assert_eq!(preview.preview_type, "image");
        assert!(preview.content.is_none(), "images must not be OCR'd");
        assert_eq!((preview.width, preview.height), (Some(12), Some(7)));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn thumbnail_cache_lives_at_config_dir_thumbs() {
        let dir = thumbnail_cache_dir();
        assert_eq!(
            dir.file_name().and_then(|name| name.to_str()),
            Some("thumbs")
        );
        assert_eq!(dir.parent(), Some(KensakuConfig::config_dir().as_path()));
        assert_eq!(
            KensakuConfig::config_dir()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("kensaku")
        );
    }
}
