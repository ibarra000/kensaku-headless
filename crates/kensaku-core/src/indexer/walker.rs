use crate::indexer::ignore::IgnoreSet;
use crate::search::fuzzy::compute_char_mask;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    /// Interned lowercase extension (avoids one allocation per entry).
    pub extension: Option<&'static str>,
    pub size: u64,
    pub modified: u64,
    pub is_dir: bool,
    /// `st_dev`; `0` means unknown.
    pub dev: u64,
    /// `st_ino`; `0` means unknown.
    pub ino: u64,
    /// Character presence bitmask for prefiltering.
    pub char_bloom: u64,
    /// Re-read content even if mtime/size are unchanged (e.g. OCR toggled).
    pub content_pending: bool,
}

/// Interns an extension for the process lifetime (a small, bounded leak).
pub fn intern_extension(extension: Option<&str>) -> Option<&'static str> {
    let extension = extension?.trim().trim_start_matches('.');
    if extension.is_empty() {
        return None;
    }
    static EXTENSIONS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut table = EXTENSIONS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(&interned) = table.get(extension) {
        return Some(interned);
    }
    let lower = extension.to_ascii_lowercase();
    if let Some(&interned) = table.get(lower.as_str()) {
        return Some(interned);
    }
    let interned: &'static str = Box::leak(lower.into_boxed_str());
    table.insert(interned);
    Some(interned)
}

/// `(device_id, inode)` identity across renames; `0` (unknown) off Unix.
pub fn file_identity(meta: &std::fs::Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        (0, 0)
    }
}

/// True for a cloud placeholder that reading would hydrate (Windows offline/recall attrs,
/// macOS `SF_DATALESS`); always false on Linux.
#[cfg(windows)]
pub fn is_cloud_placeholder(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    // https://learn.microsoft.com/windows/win32/fileio/file-attribute-constants
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    let attributes = meta.file_attributes();
    attributes & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;

    // <sys/stat.h>: "file is dataless object" (contents live in the cloud).
    const SF_DATALESS: u32 = 0x4000_0000;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn is_cloud_placeholder(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Path variant; a `stat` never downloads.
pub fn is_cloud_placeholder_path(path: &std::path::Path) -> bool {
    #[cfg(any(windows, target_os = "macos"))]
    {
        std::fs::metadata(path)
            .map(|meta| is_cloud_placeholder(&meta))
            .unwrap_or(false)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = path;
        false
    }
}

/// Kensaku's own runtime dir, never crawled (not dot-prefixed on macOS/Windows).
pub fn runtime_dir_prunes() -> Vec<PathBuf> {
    vec![crate::config::KensakuConfig::config_dir()]
}

/// OS app-data trees under home (`AppData`, `~/Library`): caches and logs, never search targets.
pub fn system_data_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    if cfg!(windows) {
        // Phone Link's CrossDevice listing hits the phone over Wi-Fi and pops "offline" dialogs.
        vec![home.join("AppData"), home.join("CrossDevice")]
    } else if cfg!(target_os = "macos") {
        vec![home.join("Library")]
    } else {
        Vec::new()
    }
}

/// Prunes for a walk/watch over `roots`: runtime data always, OS data trees unless a root lies inside.
pub fn prunes_for_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    prunes_for_roots_with(roots, system_data_dirs())
}

fn prunes_for_roots_with(roots: &[PathBuf], system: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut prunes = runtime_dir_prunes();
    for dir in system {
        if !roots.iter().any(|root| root.starts_with(&dir)) {
            prunes.push(dir);
        }
    }
    prunes
}

/// True for a cloud folder whose listing is not on disk; it stays findable but is not descended.
#[cfg(windows)]
pub fn is_unpopulated_cloud_dir(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    meta.is_dir()
        && meta.file_attributes()
            & (FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0
}

#[cfg(target_os = "macos")]
pub fn is_unpopulated_cloud_dir(meta: &std::fs::Metadata) -> bool {
    meta.is_dir() && is_cloud_placeholder(meta)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn is_unpopulated_cloud_dir(_meta: &std::fs::Metadata) -> bool {
    false
}

pub fn starts_with_any(path: &std::path::Path, prune_roots: &[PathBuf]) -> bool {
    prune_roots.iter().any(|root| path.starts_with(root))
}

/// Default-deny sensitive names, shared by the crawler and the watcher.
pub const SENSITIVE_NAMES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".gpg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".password-store",
    "1password",
    "bitwarden",
    "keepass",
    "keepassxc",
];

/// Whether `name` is sensitive; `.env` matches as a prefix.
pub fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env" || lower.starts_with(".env.") || SENSITIVE_NAMES.contains(&lower.as_str())
}

pub fn is_sensitive_path(path: &std::path::Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(is_sensitive_name)
    })
}

/// Walks `dirs`, honoring ignore files unless `include_ignored`; sensitive names are always excluded.
pub fn walk_directories(
    dirs: &[PathBuf],
    excluded_dirs: &[String],
    include_ignored: bool,
    skip_hidden: bool,
) -> Vec<FileEntry> {
    let never = AtomicBool::new(false);
    walk_directories_until(dirs, excluded_dirs, include_ignored, skip_hidden, &never)
}

/// [`walk_directories`] that returns nothing if cancelled (a partial list would read as deletions).
pub fn walk_directories_until(
    dirs: &[PathBuf],
    excluded_dirs: &[String],
    include_ignored: bool,
    skip_hidden: bool,
    cancel: &AtomicBool,
) -> Vec<FileEntry> {
    let mut result = Vec::new();
    let mut ignore_dirs: Vec<PathBuf> = Vec::new();
    let excluded_set: HashSet<String> = excluded_dirs.iter().map(|s| s.to_lowercase()).collect();

    for root in dirs {
        if !root.exists() {
            continue;
        }

        let walker = jwalk::WalkDir::new(root)
            .skip_hidden(skip_hidden)
            .process_read_dir({
                let excluded_set = excluded_set.clone();
                let prune_roots = prunes_for_roots(dirs);
                move |_depth, _path, _read_dir_state, children| {
                    children.retain(|dir_entry_result| {
                        if let Ok(entry) = dir_entry_result {
                            if starts_with_any(&entry.path(), &prune_roots) {
                                return false;
                            }
                            let name = entry.file_name.to_string_lossy().to_lowercase();
                            // jwalk's hidden filtering is unsupported on some platforms.
                            if skip_hidden && name.starts_with('.') {
                                return false;
                            }
                            if excluded_set.contains(&name) {
                                return false;
                            }
                            if is_sensitive_name(&name) {
                                return false;
                            }
                        }
                        true
                    });
                    // Keep cloud-only folders as entries but never list them (network fetch).
                    #[cfg(any(windows, target_os = "macos"))]
                    for entry in children.iter_mut().flatten() {
                        if entry.file_type.is_dir()
                            && let Ok(meta) = std::fs::metadata(entry.path())
                            && is_unpopulated_cloud_dir(&meta)
                        {
                            entry.read_children = None;
                        }
                    }
                }
            });

        for entry_res in walker {
            if cancel.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let entry = match entry_res {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            let is_dir = entry.file_type().is_dir();
            let name = entry.file_name.to_string_lossy().to_string();

            let extension = if is_dir {
                None
            } else {
                intern_extension(path.extension().and_then(|ext| ext.to_str()))
            };

            let (size, modified, dev, ino) = match entry.metadata() {
                Ok(meta) => {
                    let sz = meta.len();
                    let mod_time = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let (dev, ino) = file_identity(&meta);
                    (sz, mod_time, dev, ino)
                }
                Err(_) => (0, 0, 0, 0),
            };

            let char_bloom = compute_char_mask(&name);
            let file_entry = FileEntry {
                id: 0,
                path: path.clone(),
                name,
                extension,
                size,
                modified,
                is_dir,
                dev,
                ino,
                char_bloom,
                content_pending: false,
            };

            if !include_ignored && is_dir {
                ignore_dirs.push(path);
            }

            result.push(file_entry);
        }
    }

    {
        let ignore_set = if include_ignored {
            IgnoreSet::empty()
        } else {
            IgnoreSet::from_directories(&ignore_dirs)
        };
        // Shared with the watcher so edits in ignored trees stay out of the index.
        crate::indexer::ignore::set_active(ignore_set.clone());
        if !include_ignored && !ignore_set.is_empty() {
            result.retain(|entry| !ignore_set.is_ignored(&entry.path, entry.is_dir));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HiddenFixture(PathBuf);

    impl HiddenFixture {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("kensaku-hidden-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, relative: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "searchable fixture").unwrap();
        }

        fn paths(&self, root: &std::path::Path, skip_hidden: bool) -> HashSet<PathBuf> {
            walk_directories(&[root.to_path_buf()], &[], true, skip_hidden)
                .into_iter()
                .map(|entry| entry.path)
                .collect()
        }
    }

    impl Drop for HiddenFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hidden_entries_follow_skip_hidden_setting() {
        let fixture = HiddenFixture::new();
        for path in [
            "visible.txt",
            ".dotfile",
            ".notes/note.txt",
            ".notes/.nested/deep.txt",
        ] {
            fixture.write(path);
        }
        for skip_hidden in [true, false] {
            let paths = fixture.paths(&fixture.0, skip_hidden);
            assert!(paths.contains(&fixture.0.join("visible.txt")));
            for path in [
                ".dotfile",
                ".notes",
                ".notes/note.txt",
                ".notes/.nested/deep.txt",
            ] {
                assert_eq!(
                    paths.contains(&fixture.0.join(path)),
                    !skip_hidden,
                    "{path}, skip_hidden={skip_hidden}"
                );
            }
        }
    }

    #[test]
    fn explicitly_watched_dot_directory_includes_hidden_descendants_when_enabled() {
        let fixture = HiddenFixture::new();
        fixture.write(".notes/plain.txt");
        fixture.write(".notes/.nested/deep.txt");
        let root = fixture.0.join(".notes");
        let paths = fixture.paths(&root, false);
        assert!(paths.contains(&root.join("plain.txt")));
        assert!(paths.contains(&root.join(".nested/deep.txt")));
    }

    #[test]
    fn hidden_indexing_keeps_sensitive_names_and_explicit_exclusions_pruned() {
        let fixture = HiddenFixture::new();
        for path in [
            ".notes/keep.txt",
            ".ssh/id_rsa",
            ".env",
            ".env.local",
            ".excluded/drop.txt",
        ] {
            fixture.write(path);
        }
        for skip_hidden in [true, false] {
            let entries = walk_directories(
                std::slice::from_ref(&fixture.0),
                &[".excluded".into()],
                true,
                skip_hidden,
            );
            assert_eq!(
                entries
                    .iter()
                    .any(|e| e.path == fixture.0.join(".notes/keep.txt")),
                !skip_hidden
            );
            assert!(!entries.iter().any(|entry| {
                entry
                    .path
                    .strip_prefix(&fixture.0)
                    .unwrap()
                    .components()
                    .any(|component| {
                        let name = component.as_os_str().to_string_lossy();
                        is_sensitive_name(&name) || name == ".excluded"
                    })
            }));
        }
    }

    #[test]
    fn cancelled_walk_returns_nothing() {
        let fixture = HiddenFixture::new();
        fixture.write("docs/a.txt");
        fixture.write("docs/b.txt");
        let root = std::slice::from_ref(&fixture.0);

        let cancel = AtomicBool::new(false);
        assert!(!walk_directories_until(root, &[], true, false, &cancel).is_empty());

        cancel.store(true, Ordering::Relaxed);
        assert!(walk_directories_until(root, &[], true, false, &cancel).is_empty());
    }

    #[test]
    fn hidden_indexing_still_honors_ignore_files() {
        let fixture = HiddenFixture::new();
        fixture.write(".notes/keep.txt");
        fixture.write(".ignored/drop.txt");
        std::fs::write(fixture.0.join(".gitignore"), ".ignored/\n").unwrap();
        let entries = walk_directories(std::slice::from_ref(&fixture.0), &[], false, false);
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == fixture.0.join(".notes/keep.txt"))
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.path.starts_with(fixture.0.join(".ignored")))
        );
    }

    #[test]
    fn extension_interning_is_stable_and_case_insensitive() {
        let upper = intern_extension(Some("PNG")).expect("interned");
        let dotted = intern_extension(Some(".png")).expect("interned");
        assert_eq!(upper, dotted);
        assert!(std::ptr::eq(upper, dotted));
        assert_eq!(intern_extension(None), None);
        assert_eq!(intern_extension(Some("")), None);
        assert_eq!(intern_extension(Some(".")), None);
    }

    #[test]
    fn interned_extension_storage_is_inline() {
        assert!(
            std::mem::size_of::<Option<&'static str>>() < std::mem::size_of::<Option<String>>()
        );
    }

    #[test]
    fn system_data_is_pruned_unless_a_root_lives_inside_it() {
        let home = PathBuf::from("/home/me");
        let library = home.join("Library");
        let prunes = prunes_for_roots_with(std::slice::from_ref(&home), vec![library.clone()]);
        assert!(starts_with_any(&library.join("Caches/x"), &prunes));
        assert!(!starts_with_any(&home.join("Documents/a.txt"), &prunes));
        let icloud = library.join("Mobile Documents/com~apple~CloudDocs");
        let prunes = prunes_for_roots_with(&[home.clone(), icloud.clone()], vec![library.clone()]);
        assert!(!starts_with_any(&icloud.join("notes.txt"), &prunes));
    }

    #[test]
    fn runtime_dir_prunes_covers_config_dir_and_subtrees() {
        let prunes = runtime_dir_prunes();
        assert!(!prunes.is_empty());
        let config_dir = crate::config::KensakuConfig::config_dir();
        assert!(starts_with_any(&config_dir, &prunes));
        assert!(starts_with_any(&config_dir.join("index"), &prunes));
        assert!(config_dir.join("index").join("tantivy").is_absolute() == config_dir.is_absolute());
        assert!(starts_with_any(
            &config_dir.join("thumbs").join("a.jpg"),
            &prunes
        ));
        // A sibling sharing a name prefix must not be pruned.
        let sibling = config_dir.parent().map(|p| p.join("kensaku-backup"));
        if let Some(sibling) = sibling {
            assert!(!starts_with_any(&sibling, &prunes));
        }
        assert!(starts_with_any(
            &config_dir.join("models").join("det.onnx"),
            &prunes
        ));
    }

    #[test]
    fn walker_never_enters_its_own_runtime_dir() {
        // Fake macOS-like layout: watched_root/Library next to the config dir.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "kensaku-walker-prune-{}-{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&base).unwrap();

        let config_dir = crate::config::KensakuConfig::config_dir();
        // `dirs::config_dir()` cannot be overridden in parallel tests; prune whatever it is.
        let root = base.join("root");
        let docs = root.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("notes.txt"), b"hello").unwrap();

        let entries = walk_directories(std::slice::from_ref(&root), &[], false, true);
        assert!(
            entries.iter().any(|e| e.path == root.join("Documents")),
            "ordinary directories must survive the prune"
        );
        assert!(
            !entries
                .iter()
                .any(|e| starts_with_any(&e.path, &runtime_dir_prunes())),
            "the crawler must never index its own storage directory"
        );

        assert!(std::fs::remove_dir_all(&base).is_ok());
        let _ = config_dir;
    }
}
