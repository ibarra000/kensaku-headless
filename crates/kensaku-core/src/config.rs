use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alias {
    /// Word to expand.
    pub term: String,
    /// Other spellings that match the same thing.
    pub expansions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSearch {
    pub name: String,
    pub query: String,
    #[serde(default)]
    pub sort: Option<String>,
}

/// Opt-in external text extractor, run directly (no shell) with a cleared env and timeout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalExtractor {
    /// Shown in extraction metadata and warnings.
    pub name: String,
    /// Lowercase extensions without the dot, e.g. `["stl", "obj"]`.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// MIME selectors, e.g. `["model/*"]` (alias `mime_types`); either list may match.
    #[serde(default, alias = "mime_types")]
    pub mime: Vec<String>,
    /// Absolute executable path; PATH lookup is never used.
    pub command: String,
    /// `{path}` is replaced with the file path, else the path is appended.
    #[serde(default)]
    pub args: Vec<String>,
    /// Wall-clock limit; clamped to 100..=7000 ms.
    #[serde(default = "default_extractor_timeout_ms")]
    pub timeout_ms: u64,
    /// Max captured stdout; clamped to 1 KiB..=4 MiB.
    #[serde(default = "default_extractor_output_bytes")]
    pub max_output_bytes: u64,
    /// External commands never run unless this is true.
    #[serde(default)]
    pub enabled: bool,
}

pub fn default_extractor_timeout_ms() -> u64 {
    5_000
}

pub fn default_extractor_output_bytes() -> u64 {
    1_000_000
}

pub fn default_aliases() -> Vec<Alias> {
    [
        ("resume", &["cv", "curriculum vitae"][..]),
        ("invoice", &["receipt", "bill"][..]),
        ("receipt", &["invoice", "bill"][..]),
        ("screenshot", &["screen shot", "capture"][..]),
        ("contract", &["agreement"][..]),
        ("photo", &["image", "picture"][..]),
    ]
    .into_iter()
    .map(|(term, expansions)| Alias {
        term: term.to_string(),
        expansions: expansions.iter().map(|s| s.to_string()).collect(),
    })
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CrawlMode {
    Full,
    #[default]
    Balanced,
    Eco,
}

impl CrawlMode {
    pub fn is_background(self) -> bool {
        !matches!(self, Self::Full)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Balanced => "balanced",
            Self::Eco => "eco",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KensakuConfig {
    pub watched_dirs: Vec<PathBuf>,
    pub excluded_dirs: Vec<String>,
    pub excluded_extensions: Vec<String>,
    pub shortcut: String,
    pub max_results: usize,
    pub is_setup_complete: bool,
    /// True once the first full crawl finished.
    #[serde(default)]
    pub first_scan_done: bool,
    /// Skip dot-prefixed (hidden) files and directories. Default true.
    #[serde(default = "default_true")]
    pub skip_hidden: bool,
    #[serde(default = "default_true")]
    pub semantic_enabled: bool,
    #[serde(default = "default_semantic_model")]
    pub semantic_model: String,
    /// Keep the static embedding table in f32 instead of i8.
    #[serde(default)]
    pub semantic_f32_table: bool,
    /// `balanced` uses idle priority; `eco` also paces on battery; `full` is normal priority.
    #[serde(default)]
    pub crawl_mode: CrawlMode,
    /// Pause crawling; the watcher still applies renames and deletes.
    #[serde(default)]
    pub pause_indexing: bool,
    /// Allow MCP agents to call the `reindex` write tool.
    #[serde(default)]
    pub mcp_allow_reindex: bool,
    #[serde(default = "default_true")]
    pub ocr_enabled: bool,
    /// Legacy; ignored.
    #[serde(default = "default_app_mode")]
    pub app_mode: String,
    /// Legacy; ignored.
    #[serde(default = "default_true")]
    pub close_to_background: bool,
    /// `tesseract`, `accurate` (PP-OCRv6) or `indra`; falls back Indra -> PP-OCR -> Tesseract.
    #[serde(default = "default_ocr_engine")]
    pub ocr_engine: String,
    /// External extractors; none run without explicit configuration.
    #[serde(default)]
    pub extractors: Vec<ExternalExtractor>,
    /// Skip OCR on images larger than this many bytes (0 = no limit).
    #[serde(default)]
    pub ocr_max_image_bytes: u64,
    /// Tesseract language codes, e.g. `["eng", "hin"]`; empty = English if installed.
    #[serde(default)]
    pub ocr_languages: Vec<String>,
    /// Beta visual model pack; not wired into search yet.
    #[serde(default)]
    pub visual_search_beta_enabled: bool,
    /// `quick` (default), `balanced` or `deep`; unknown values mean `quick`.
    #[serde(default = "default_content_depth")]
    pub content_depth: String,
    /// Max file size (MB) for text extraction; larger files are name-only. Default 20.
    #[serde(default = "default_content_max_file_size_mb")]
    pub content_max_file_size_mb: u64,
    /// Max extracted text bytes per document. Default 1 MB.
    #[serde(default = "default_content_max_text_bytes")]
    pub content_max_text_bytes: usize,
    /// Data rows indexed per CSV/TSV/Parquet file after the header. Default 200.
    #[serde(default = "default_content_csv_head_rows")]
    pub content_csv_head_rows: usize,
    /// Max semantic units (chunks) per document. Default 20.
    #[serde(default = "default_content_max_semantic_units")]
    pub content_max_semantic_units: usize,
    /// When non-empty, agent-surface results and reads are restricted to these roots.
    #[serde(default)]
    pub agent_scoped_roots: Vec<String>,
    /// Index files matched by `.gitignore` / `.cursorignore`. Default false.
    #[serde(default)]
    pub include_ignored: bool,
    /// Directories indexed by name and path only (no extraction, OCR or embeddings).
    #[serde(default)]
    pub name_only_dirs: Vec<PathBuf>,
    /// "light" or "dark" (legacy "paper"/"void" migrated in v5).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Language code ("en", "zh", …) or None to auto-detect.
    #[serde(default)]
    pub language: Option<String>,
    /// Personal vocabulary: `term` matches any of `expansions`.
    #[serde(default = "default_aliases")]
    pub aliases: Vec<Alias>,
    /// Saved queries.
    #[serde(default)]
    pub saved_searches: Vec<SavedSearch>,
    /// Pinned file paths.
    #[serde(default)]
    pub pins: Vec<String>,
    /// Generate thumbnails. Default true.
    #[serde(default = "default_true")]
    pub show_thumbnails: bool,
    /// "grid" (default) or "list".
    #[serde(default = "default_results_view")]
    pub results_view: String,
    /// Check for updates automatically. Default true.
    #[serde(default = "default_true")]
    pub auto_update: bool,
    /// Config schema version, used for one-time migrations.
    #[serde(default)]
    pub config_version: u32,
}

fn default_true() -> bool {
    true
}

/// PP-OCR; engine selection falls back to Tesseract where it cannot run.
fn default_ocr_engine() -> String {
    "accurate".to_string()
}

fn default_app_mode() -> String {
    "launcher".to_string()
}

/// Pure core of [`KensakuConfig::config_dir`], testable without mutating process env.
fn resolve_config_dir(
    env: Option<&std::ffi::OsStr>,
    exe_path: Option<&std::path::Path>,
    platform_default: Option<PathBuf>,
) -> PathBuf {
    if let Some(dir) = env
        && !dir.is_empty()
        && !dir
            .to_str()
            .is_some_and(|text| text.trim().is_empty())
    {
        return PathBuf::from(dir);
    }
    if let Some(exe_dir) = exe_path.and_then(std::path::Path::parent)
        && (exe_dir.join("portable").is_file() || exe_dir.join("kensaku.portable").is_file())
    {
        return exe_dir.join("data");
    }
    if let Some(config_dir) = platform_default {
        config_dir.join("kensaku")
    } else {
        PathBuf::from(".kensaku")
    }
}

fn default_semantic_model() -> String {
    "static".to_string()
}

fn default_results_view() -> String {
    "grid".to_string()
}

fn default_content_depth() -> String {
    "balanced".to_string()
}

/// Books/manuals are often large from images but small in text.
fn default_content_max_file_size_mb() -> u64 {
    100
}

/// About 2,000 pages of text.
fn default_content_max_text_bytes() -> usize {
    4_000_000
}

/// Later rows aren't searchable by value; header and column types still are.
fn default_content_csv_head_rows() -> usize {
    200
}

/// 48 overlapping chunks covers roughly the first 9,000 words.
fn default_content_max_semantic_units() -> usize {
    48
}

fn default_theme() -> String {
    "dark".to_string()
}

/// Canonicalizes to "light" or "dark" (accepts "paper"/"void"); unknown -> default.
pub fn normalize_theme(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "light" | "paper" => "light".to_string(),
        "dark" | "void" => "dark".to_string(),
        _ => default_theme(),
    }
}

/// Supported language code, or None for auto-detect.
pub fn normalize_language(raw: &str) -> Option<String> {
    let v = raw.trim().to_ascii_lowercase();
    match v.as_str() {
        "en" | "zh" | "ja" | "es" | "fr" | "de" | "ko" | "pt" | "ru" | "it" => Some(v),
        _ => None,
    }
}

/// Version written by this build; see `parse_and_migrate`.
pub const CURRENT_CONFIG_VERSION: u32 = 8;

/// `UNC\srv\share\x` -> `\\srv\share\x`; other paths unchanged.
fn repair_stripped_unc(dirs: &[PathBuf]) -> Vec<PathBuf> {
    dirs.iter()
        .map(|dir| match dir.to_str().and_then(|s| s.strip_prefix(r"UNC\")) {
            Some(rest) => PathBuf::from(format!(r"\\{rest}")),
            None => dir.clone(),
        })
        .collect()
}

/// Extensions skipped entirely (not even indexed by name).
const DEFAULT_EXCLUDED_EXTENSIONS: &[&str] = &[
    // Binaries and build artifacts
    "exe", "dll", "so", "dylib", "bin", "o", "a", "lib", "class", "pyc", "pyo",
    // Secrets (`key` is also Apple Keynote; private keys win)
    "pem", "key", "pfx", "p12", "crt", "cer",
    // Databases
    "db", "sqlite", "sqlite3",
    // Lock files
    "lock", "lockb",
    // Minified / generated
    "min.js", "min.css", "map",
];

/// Excluded up to v5, findable by name since v6.
const NAME_SEARCHABLE_SINCE_V6: &[&str] = &[
    "psd", "ai", "sketch", "fig", "mp3", "mp4", "wav", "avi", "mov", "mkv", "flac", "ogg",
    "webm", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "tgz", "ttf", "otf", "woff",
    "woff2", "eot",
];

impl Default for KensakuConfig {
    fn default() -> Self {
        let mut watched = Vec::new();
        if let Some(home) = dirs::home_dir() {
            watched.push(home);
        }

        let default_excluded_dirs: Vec<String> = vec![
            // Version control
            ".git",
            ".svn",
            ".hg",
            ".bzr",
            // Package managers / dependencies
            "node_modules",
            "vendor",
            "Pods",
            ".bundle",
            // Build outputs
            "target",
            "build",
            "dist",
            "out",
            ".output",
            ".next",
            ".nuxt",
            ".svelte-kit",
            ".turbo",
            ".vercel",
            ".netlify",
            // Caches
            ".cache",
            ".npm",
            ".yarn",
            ".pnpm",
            ".cargo",
            ".rustup",
            ".gradle",
            ".m2",
            "__pycache__",
            ".pytest_cache",
            ".mypy_cache",
            ".tox",
            ".nox",
            // Virtual environments
            "venv",
            ".venv",
            "env",
            ".env",
            // IDE / editor
            ".vscode",
            ".idea",
            ".vs",
            ".eclipse",
            // OS junk
            ".DS_Store",
            "Thumbs.db",
            "desktop.ini",
            // Misc
            ".Trash",
            "lost+found",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        // Documents/images are extracted/OCR'd; media, archives and fonts stay findable by name.
        let default_excluded_extensions: Vec<String> = DEFAULT_EXCLUDED_EXTENSIONS
            .iter()
            .map(|ext| ext.to_string())
            .collect();

        Self {
            watched_dirs: watched,
            excluded_dirs: default_excluded_dirs,
            excluded_extensions: default_excluded_extensions,
            shortcut: "Alt+Space".to_string(),
            max_results: 50,
            is_setup_complete: false,
            first_scan_done: false,
            skip_hidden: true,
            semantic_enabled: true,
            semantic_model: "static".to_string(),
            semantic_f32_table: false,
            crawl_mode: CrawlMode::default(),
            pause_indexing: false,
            mcp_allow_reindex: false,
            visual_search_beta_enabled: false,
            content_depth: default_content_depth(),
            content_max_file_size_mb: default_content_max_file_size_mb(),
            content_max_text_bytes: default_content_max_text_bytes(),
            content_csv_head_rows: default_content_csv_head_rows(),
            content_max_semantic_units: default_content_max_semantic_units(),
            agent_scoped_roots: Vec::new(),
            include_ignored: false,
            name_only_dirs: Vec::new(),
            extractors: Vec::new(),
            ocr_enabled: true,
            ocr_max_image_bytes: 0,
            ocr_languages: Vec::new(),
            ocr_engine: default_ocr_engine(),
            app_mode: default_app_mode(),
            close_to_background: true,
            theme: default_theme(),
            language: None,
            aliases: default_aliases(),
            saved_searches: Vec::new(),
            pins: Vec::new(),
            show_thumbnails: true,
            results_view: default_results_view(),
            auto_update: true,
            config_version: CURRENT_CONFIG_VERSION,
        }
    }
}

impl KensakuConfig {
    /// Data/config dir: `KENSAKU_DATA_DIR`, then portable mode (`portable` or
    /// `kensaku.portable` marker next to the exe -> `<exe_dir>/data`), then the platform default.
    pub fn config_dir() -> PathBuf {
        let env_dir = std::env::var_os("KENSAKU_DATA_DIR");
        let exe_path = std::env::current_exe().ok();
        resolve_config_dir(env_dir.as_deref(), exe_path.as_deref(), dirs::config_dir())
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.json")
    }

    pub fn index_dir() -> PathBuf {
        Self::config_dir().join("index")
    }

    /// Parses and migrates; returns whether anything changed so callers can persist.
    fn parse_and_migrate(content: &str) -> Result<(Self, bool), serde_json::Error> {
        let mut cfg: Self = serde_json::from_str(content)?;
        let mut migrated = false;

        if cfg.config_version < 1 {
            // v1: document/image formats became indexable.
            const LEGACY_EXCLUDED: &[&str] = &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "png", "jpg", "jpeg", "gif",
                "bmp", "ico", "webp", "tiff", "tif",
            ];
            cfg.excluded_extensions.retain(|ext| {
                !LEGACY_EXCLUDED
                    .iter()
                    .any(|legacy| legacy.eq_ignore_ascii_case(ext))
            });
            cfg.config_version = 1;
            migrated = true;
        }

        if cfg.config_version < 2 {
            // v2: also strip legacy dot-prefixed exclusions (".jpg").
            const NOW_INDEXABLE: &[&str] = &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "epub", "odt", "ods", "odp",
                "rtf", "png", "jpg", "jpeg", "gif", "bmp", "webp", "tiff", "tif",
            ];
            cfg.excluded_extensions.retain(|ext| {
                let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
                !NOW_INDEXABLE.contains(&normalized.as_str())
            });
            cfg.config_version = 2;
            migrated = true;
        }

        if cfg.config_version < 3 {
            // v3: installs with setup complete count as having finished their first scan.
            if cfg.is_setup_complete {
                cfg.first_scan_done = true;
            }
            cfg.config_version = 3;
            migrated = true;
        }

        if cfg.config_version < 4 {
            // v4: default-deny secret/credential extensions.
            const SENSITIVE_EXTS: &[&str] = &["pem", "key", "pfx", "p12", "crt", "cer"];
            for ext in SENSITIVE_EXTS {
                if !cfg
                    .excluded_extensions
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(ext))
                {
                    cfg.excluded_extensions.push((*ext).to_string());
                }
            }
            cfg.config_version = 4;
            migrated = true;
        }

        if cfg.config_version < 5 {
            // v5: themes renamed "paper"/"void" -> "light"/"dark".
            cfg.theme = normalize_theme(&cfg.theme);
            cfg.config_version = 5;
            migrated = true;
        }

        if cfg.config_version < 6 {
            // v6 (1.0.10): move values still on an old default; user changes are left alone.
            if cfg.ocr_engine.eq_ignore_ascii_case("tesseract") {
                // Anchored to PP-OCR, not to future default changes.
                cfg.ocr_engine = "accurate".to_string();
            }
            // Media, archives, fonts and design files become findable by name.
            cfg.excluded_extensions.retain(|ext| {
                let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
                !NAME_SEARCHABLE_SINCE_V6.contains(&normalized.as_str())
            });
            // `.Trash-*` never matched: exclusions are exact names.
            cfg.excluded_dirs.retain(|dir| dir != ".Trash-*");
            // Raise content caps still at the old defaults.
            if cfg.content_max_file_size_mb == 20 {
                cfg.content_max_file_size_mb = default_content_max_file_size_mb();
            }
            if cfg.content_max_text_bytes == 1_000_000 {
                cfg.content_max_text_bytes = default_content_max_text_bytes();
            }
            if cfg.content_max_semantic_units == 20 {
                cfg.content_max_semantic_units = default_content_max_semantic_units();
            }
            if cfg.content_depth == "quick" {
                cfg.content_depth = default_content_depth();
            }
            cfg.config_version = 6;
            migrated = true;
        }

        if cfg.config_version < 7 {
            // v7: normalize language; unknown codes -> auto-detect.
            cfg.language = cfg.language.as_deref().and_then(normalize_language);
            cfg.config_version = 7;
            migrated = true;
        }

        if cfg.config_version < 8 {
            // v8 (2.0.1): network roots were saved as `UNC\srv\share` (a relative path).
            cfg.watched_dirs = repair_stripped_unc(&cfg.watched_dirs);
            cfg.name_only_dirs = repair_stripped_unc(&cfg.name_only_dirs);
            cfg.config_version = 8;
            migrated = true;
        }

        Ok((cfg, migrated))
    }

    /// Disk-free [`Self::load`] for tests.
    pub fn load_from_str(content: &str) -> Self {
        match Self::parse_and_migrate(content) {
            Ok((cfg, _migrated)) => cfg,
            Err(_) => Self::default(),
        }
    }

    /// Strict load: missing file -> default, invalid file -> error (not defaults).
    pub fn try_load() -> Result<Self, String> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        Self::parse_and_migrate(&content)
            .map(|(cfg, _)| cfg)
            .map_err(|e| format!("invalid config {}: {e}", path.display()))
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if !path.exists() {
            return Self::default();
        }

        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                eprintln!(
                    "[Kensaku] Could not read config {}: {}; using defaults",
                    path.display(),
                    e
                );
                return Self::default();
            }
        };

        match Self::parse_and_migrate(&content) {
            Ok((cfg, migrated)) => {
                if migrated {
                    let _ = cfg.save();
                }
                cfg
            }
            Err(e) => {
                // Back up the unreadable file; the next `save()` overwrites it.
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let backup = Self::config_dir().join(format!("config.json.corrupt-{}", stamp));
                match fs::copy(&path, &backup) {
                    Ok(_) => eprintln!(
                        "[Kensaku] Config is corrupt ({}); backup saved to {}; using defaults",
                        e,
                        backup.display()
                    ),
                    Err(copy_err) => eprintln!(
                        "[Kensaku] Config is corrupt ({}); failed to back up {}: {}; using defaults",
                        e,
                        path.display(),
                        copy_err
                    ),
                }
                Self::default()
            }
        }
    }

    /// Atomic save: temp file, fsync, rename, then best-effort directory fsync.
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::config_dir())
    }

    pub fn save_to(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join("config.json");
        let tmp_path = dir.join("config.json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;

        {
            let mut file = fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
            file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }

        fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;

        #[cfg(unix)]
        {
            // Directory fsync makes the rename durable; some filesystems reject it.
            if let Ok(dir_file) = fs::File::open(dir) {
                let _ = dir_file.sync_all();
            }
        }

        Ok(())
    }

    pub fn is_setup_done() -> bool {
        let cfg = Self::load();
        cfg.is_setup_complete && cfg.first_scan_done
    }

    /// (term, expansions) for the intent compiler.
    pub fn alias_pairs(&self) -> Vec<(String, Vec<String>)> {
        self.aliases
            .iter()
            .filter(|a| !a.term.trim().is_empty() && !a.expansions.is_empty())
            .map(|a| (a.term.trim().to_lowercase(), a.expansions.clone()))
            .collect()
    }

    pub fn is_dir_excluded(&self, dir_name: &str) -> bool {
        self.excluded_dirs
            .iter()
            .any(|ex| ex.eq_ignore_ascii_case(dir_name))
    }

    pub fn is_extension_excluded(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.trim_start_matches('.').to_ascii_lowercase();
            return self
                .excluded_extensions
                .iter()
                .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&ext));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "kensaku-config-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn auto_update_default_and_roundtrip() {
        let mut cfg = KensakuConfig::default();
        assert!(cfg.auto_update);
        cfg.auto_update = false;
        let json = serde_json::to_string(&cfg).unwrap();
        let loaded = KensakuConfig::load_from_str(&json);
        assert!(!loaded.auto_update);

        let json_without_flag = json.replace("\"auto_update\":false,", "");
        let loaded_defaulted = KensakuConfig::load_from_str(&json_without_flag);
        assert!(loaded_defaulted.auto_update);
    }

    #[test]
    fn closing_keeps_the_app_running_by_default() {
        // Old config files without this field must also get the default.
        let cfg = KensakuConfig::default();
        assert!(
            cfg.close_to_background,
            "closing the window must keep the app running by default"
        );

        let json = serde_json::to_string(&cfg).unwrap();
        let without = json.replace("\"close_to_background\":true,", "");
        assert!(
            !without.contains("close_to_background"),
            "test setup failed to remove the field"
        );
        let loaded = KensakuConfig::load_from_str(&without);
        assert!(
            loaded.close_to_background,
            "a config without the field must default to running in the background"
        );
    }

    #[test]
    fn default_config_is_current_version_and_dark() {
        let cfg = KensakuConfig::default();
        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        assert!(cfg.skip_hidden);
        assert!(cfg.auto_update);
        assert!(cfg.ocr_enabled);
        assert_eq!(cfg.theme, "dark");
    }

    fn write_marker(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"").unwrap();
        path
    }

    #[test]
    fn config_dir_env_var_wins_over_everything() {
        let marker_dir = temp_dir("marker");
        std::fs::create_dir_all(&marker_dir).unwrap();
        write_marker(&marker_dir, "portable");
        let default = PathBuf::from("/platform/default");

        let resolved = resolve_config_dir(
            Some(std::ffi::OsStr::new("/override/data")),
            Some(&marker_dir),
            Some(default),
        );
        assert_eq!(resolved, PathBuf::from("/override/data"));
    }

    #[test]
    fn config_dir_empty_env_var_is_ignored() {
        let resolved = resolve_config_dir(
            Some(std::ffi::OsStr::new("  ")),
            None,
            Some(std::env::temp_dir()),
        );
        assert_eq!(
            resolved,
            std::env::temp_dir().join("kensaku"),
            "an empty/whitespace KENSAKU_DATA_DIR must not become an empty relative path"
        );
    }

    #[test]
    fn portable_marker_file_puts_data_next_to_the_exe() {
        let exe_dir = temp_dir("portable");
        write_marker(&exe_dir, "portable");
        let resolved = resolve_config_dir(None, Some(&exe_dir.join("kensaku.exe")), None);
        assert_eq!(resolved, exe_dir.join("data"));
    }

    #[test]
    fn kensaku_portable_marker_file_also_deployed() {
        let exe_dir = temp_dir("portable2");
        write_marker(&exe_dir, "kensaku.portable");
        let resolved = resolve_config_dir(None, Some(&exe_dir.join("kensaku.exe")), None);
        assert_eq!(resolved, exe_dir.join("data"));
    }

    #[test]
    fn portable_marker_directory_named_portable_is_not_a_marker() {
        // A `portable` directory (not file) must not redirect data storage.
        let exe_dir = temp_dir("marker-dir");
        std::fs::create_dir_all(exe_dir.join("portable")).unwrap();
        let resolved = resolve_config_dir(
            None,
            Some(&exe_dir.join("kensaku.exe")),
            Some(std::env::temp_dir()),
        );
        assert_eq!(resolved, std::env::temp_dir().join("kensaku"));
    }

    #[test]
    fn no_env_no_marker_uses_platform_default() {
        let resolved = resolve_config_dir(
            None,
            Some(&std::env::temp_dir().join("kensaku.exe")),
            Some(std::env::temp_dir()),
        );
        assert!(resolved.ends_with("kensaku"));
    }

    #[test]
    fn missing_platform_default_falls_back_relative() {
        let resolved = resolve_config_dir(None, None, None);
        assert_eq!(resolved, PathBuf::from(".kensaku"));
    }

    #[test]
    fn semantic_f32_table_flag_defaults_off_and_round_trips() {
        let cfg = KensakuConfig::default();
        assert!(!cfg.semantic_f32_table);
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed = KensakuConfig::load_from_str(&json);
        assert!(!parsed.semantic_f32_table);

        let with_flag = json.replace(
            "\"semantic_f32_table\":false",
            "\"semantic_f32_table\":true",
        );
        assert!(KensakuConfig::load_from_str(&with_flag).semantic_f32_table);
    }

    #[test]
    fn v6_moves_old_defaults_and_keeps_user_choices() {
        let old_defaults = r#"{
            "watched_dirs": [], "excluded_dirs": [".git", ".Trash-*"],
            "excluded_extensions": ["mp4", ".zip", "ttf", "exe", "pem", "key"],
            "shortcut": "Alt+Space", "max_results": 50, "is_setup_complete": true,
            "ocr_engine": "tesseract", "content_max_file_size_mb": 20,
            "content_max_text_bytes": 1000000, "content_max_semantic_units": 20,
            "content_depth": "quick", "config_version": 5
        }"#;
        let cfg = KensakuConfig::load_from_str(old_defaults);
        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(cfg.ocr_engine, "accurate");
        assert_eq!(cfg.excluded_extensions, vec!["exe", "pem", "key"]);
        assert_eq!(cfg.excluded_dirs, vec![".git"]);
        assert_eq!(cfg.content_max_file_size_mb, 100);
        assert_eq!(cfg.content_max_text_bytes, 4_000_000);
        assert_eq!(cfg.content_max_semantic_units, 48);
        assert_eq!(cfg.content_depth, "balanced");

        let customized = r#"{
            "watched_dirs": [], "excluded_dirs": [], "excluded_extensions": ["log"],
            "shortcut": "Alt+Space", "max_results": 50, "is_setup_complete": true,
            "ocr_engine": "accurate", "content_max_file_size_mb": 5,
            "content_max_text_bytes": 200000, "content_max_semantic_units": 8,
            "config_version": 5
        }"#;
        let cfg = KensakuConfig::load_from_str(customized);
        assert_eq!(cfg.excluded_extensions, vec!["log"]);
        assert_eq!(cfg.content_max_file_size_mb, 5);
        assert_eq!(cfg.content_max_text_bytes, 200_000);
        assert_eq!(cfg.content_max_semantic_units, 8);
    }

    #[test]
    fn v8_repairs_roots_saved_with_stripped_unc_prefix() {
        let broken = r#"{
            "watched_dirs": ["UNC\\server\\share\\Docs", "C:\\Users\\me"],
            "name_only_dirs": ["UNC\\nas\\media"],
            "excluded_dirs": [], "excluded_extensions": [],
            "shortcut": "Alt+Space", "max_results": 50, "is_setup_complete": true,
            "config_version": 7
        }"#;
        let (cfg, migrated) = KensakuConfig::parse_and_migrate(broken).unwrap();
        assert!(migrated);
        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(
            cfg.watched_dirs,
            vec![
                PathBuf::from(r"\\server\share\Docs"),
                PathBuf::from(r"C:\Users\me")
            ]
        );
        assert_eq!(cfg.name_only_dirs, vec![PathBuf::from(r"\\nas\media")]);
    }

    #[test]
    fn fresh_defaults_are_the_1_0_10_defaults() {
        let cfg = KensakuConfig::default();
        assert_eq!(cfg.ocr_engine, "accurate");
        assert!(!cfg.excluded_extensions.iter().any(|e| e == "mp4" || e == "zip"));
        assert!(cfg.excluded_extensions.iter().any(|e| e == "exe"));
        assert!(!cfg.excluded_dirs.iter().any(|d| d.contains('*')));
        assert_eq!(cfg.content_max_file_size_mb, 100);
        let json = serde_json::to_string(&cfg).unwrap();
        let (_, migrated) = KensakuConfig::parse_and_migrate(&json).unwrap();
        assert!(!migrated);
    }

    #[test]
    fn name_only_dirs_default_empty_and_round_trip() {
        assert!(KensakuConfig::default().name_only_dirs.is_empty());

        let cfg = KensakuConfig {
            name_only_dirs: vec![PathBuf::from("/media")],
            ..KensakuConfig::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let (parsed, migrated) = KensakuConfig::parse_and_migrate(&json).unwrap();
        assert!(!migrated);
        assert_eq!(parsed.name_only_dirs, vec![PathBuf::from("/media")]);

        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value.as_object_mut().unwrap().remove("name_only_dirs");
        let (legacy, _) = KensakuConfig::parse_and_migrate(&value.to_string()).unwrap();
        assert!(legacy.name_only_dirs.is_empty());
    }

    #[test]
    fn crawl_mode_and_pause_parse_from_config() {
        let cfg = KensakuConfig::default();
        assert_eq!(cfg.crawl_mode, CrawlMode::Balanced);
        assert_eq!(cfg.crawl_mode, CrawlMode::default());
        assert!(!cfg.pause_indexing);

        let raw = r#"{
            "watched_dirs": [],
            "excluded_dirs": [],
            "excluded_extensions": [],
            "shortcut": "Alt+Space",
            "max_results": 50,
            "is_setup_complete": false,
            "crawl_mode": "eco",
            "pause_indexing": true
        }"#;
        let parsed = KensakuConfig::load_from_str(raw);
        assert_eq!(parsed.crawl_mode, CrawlMode::Eco);
        assert!(parsed.pause_indexing);
    }

    #[test]
    fn legacy_config_is_migrated() {
        let legacy = r#"{
            "watched_dirs": [],
            "excluded_dirs": [],
            "excluded_extensions": ["pdf", "PNG", "doc", "rs", "png", "zip"],
            "shortcut": "Alt+Space",
            "max_results": 50,
            "is_setup_complete": false
        }"#;

        let cfg = KensakuConfig::load_from_str(legacy);

        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        assert!(
            !cfg.excluded_extensions
                .iter()
                .any(|e| e.eq_ignore_ascii_case("pdf"))
        );
        assert!(
            !cfg.excluded_extensions
                .iter()
                .any(|e| e.eq_ignore_ascii_case("png"))
        );
        assert!(
            !cfg.excluded_extensions
                .iter()
                .any(|e| e.eq_ignore_ascii_case("doc"))
        );
        assert!(cfg.excluded_extensions.iter().any(|e| e == "rs"));
        assert!(!cfg.excluded_extensions.iter().any(|e| e == "zip"));
        assert!(cfg.skip_hidden);
        assert!(cfg.ocr_enabled);
        assert_eq!(cfg.theme, "dark");
    }

    #[test]
    fn already_migrated_config_is_unchanged() {
        let current = r#"{
            "watched_dirs": [],
            "excluded_dirs": [],
            "excluded_extensions": ["rs", "node_modules"],
            "shortcut": "Alt+Space",
            "max_results": 50,
            "is_setup_complete": false,
            "config_version": 2
        }"#;

        let cfg = KensakuConfig::load_from_str(current);
        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(
            cfg.excluded_extensions,
            vec![
                "rs".to_string(),
                "node_modules".to_string(),
                "pem".to_string(),
                "key".to_string(),
                "pfx".to_string(),
                "p12".to_string(),
                "crt".to_string(),
                "cer".to_string(),
            ]
        );
    }

    #[test]
    fn v5_migration_renames_legacy_theme_names() {
        for (stored, expected) in [("paper", "light"), ("void", "dark")] {
            let legacy = format!(
                r#"{{
                "watched_dirs": [],
                "excluded_dirs": [],
                "excluded_extensions": [],
                "shortcut": "Alt+Space",
                "max_results": 50,
                "is_setup_complete": true,
                "theme": "{}",
                "config_version": 4
            }}"#,
                stored
            );

            let cfg = KensakuConfig::load_from_str(&legacy);
            assert_eq!(cfg.theme, expected, "theme {} should become {}", stored, expected);
            assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        }
    }

    #[test]
    fn v5_migration_keeps_current_theme_names_and_repairs_junk() {
        assert_eq!(normalize_theme("light"), "light");
        assert_eq!(normalize_theme("DARK"), "dark");
        assert_eq!(normalize_theme("  Void "), "dark");
        assert_eq!(normalize_theme("solarized"), default_theme());
    }

    #[test]
    fn v7_migration_normalizes_language() {
        assert_eq!(normalize_language("ZH"), Some("zh".to_string()));
        assert_eq!(normalize_language(" en "), Some("en".to_string()));
        assert_eq!(normalize_language("klingon"), None);

        let legacy = r#"{
            "watched_dirs": [],
            "excluded_dirs": [],
            "excluded_extensions": [],
            "shortcut": "Alt+Space",
            "max_results": 50,
            "is_setup_complete": true,
            "language": "ZH",
            "config_version": 6
        }"#;
        let cfg = KensakuConfig::load_from_str(legacy);
        assert_eq!(cfg.language.as_deref(), Some("zh"));
        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);

        let junk = legacy.replace("\"ZH\"", "\"klingon\"");
        assert_eq!(KensakuConfig::load_from_str(&junk).language, None);
    }

    #[test]
    fn v2_migration_strips_dotted_document_and_image_extensions() {
        let legacy = r#"{
            "watched_dirs": [],
            "excluded_dirs": [],
            "excluded_extensions": [".jpg", ".PNG", ".pdf", ".o", "zip"],
            "shortcut": "Alt+Space",
            "max_results": 50,
            "is_setup_complete": false,
            "config_version": 1
        }"#;

        let cfg = KensakuConfig::load_from_str(legacy);
        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(
            cfg.excluded_extensions,
            vec![
                ".o".to_string(),
                "pem".to_string(),
                "key".to_string(),
                "pfx".to_string(),
                "p12".to_string(),
                "crt".to_string(),
                "cer".to_string(),
            ]
        );
    }

    #[test]
    fn external_extractors_default_to_off_with_safe_limits() {
        let default = KensakuConfig::default();
        assert!(default.extractors.is_empty(), "no plugins ship by default");

        let extractor: ExternalExtractor =
            serde_json::from_str(r#"{"name":"sample","extensions":[".STL"],"command":"/bin/cat"}"#)
                .expect("minimal extractor should parse");
        assert!(!extractor.enabled, "plugins require explicit opt-in");
        assert_eq!(extractor.timeout_ms, 5_000);
        assert_eq!(extractor.max_output_bytes, 1_000_000);
        assert!(extractor.mime.is_empty());
        assert!(extractor.args.is_empty());

        let enabled: ExternalExtractor = serde_json::from_str(
            r#"{"name":"sample","extensions":["stl"],"mime":["model/*"],"command":"/bin/cat","enabled":true}"#,
        )
        .expect("explicitly enabled extractor should parse");
        assert!(enabled.enabled);
        assert_eq!(enabled.mime, vec!["model/*".to_string()]);
    }

    #[test]
    fn corrupt_config_falls_back_to_defaults() {
        let cfg = KensakuConfig::load_from_str("{ not json");
        assert_eq!(cfg.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(cfg.theme, "dark");
    }

    #[test]
    fn save_to_writes_atomically() {
        let dir = temp_dir("save");

        let cfg = KensakuConfig::default();
        cfg.save_to(&dir).expect("save should succeed");

        let path = dir.join("config.json");
        assert!(path.is_file(), "config.json should exist");
        assert!(
            !dir.join("config.json.tmp").exists(),
            "temp file should be renamed away"
        );

        let saved: KensakuConfig =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(saved.theme, "dark");

        let updated = KensakuConfig {
            max_results: 7,
            ..cfg
        };
        updated.save_to(&dir).expect("second save should succeed");
        let saved: KensakuConfig =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.max_results, 7);
        assert!(!dir.join("config.json.tmp").exists());

        fs::remove_dir_all(&dir).ok();
    }
}
