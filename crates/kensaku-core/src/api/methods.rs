//! Method registry (name, access class, description): source of truth for `kensaku methods`,
//! the HTTP write gate and docs/API.md. A test enforces that dispatch handles every entry.

use serde::Serialize;

/// What a method may touch; transports gate on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    /// Reads a caller-supplied file path; HTTP applies `agent_scoped_roots`.
    FileRead,
    /// Mutates config/index or downloads; disabled on HTTP unless writes are allowed.
    Write,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MethodSpec {
    pub name: &'static str,
    pub access: Access,
    pub summary: &'static str,
}

const fn m(name: &'static str, access: Access, summary: &'static str) -> MethodSpec {
    MethodSpec {
        name,
        access,
        summary,
    }
}

use Access::{FileRead, Read, Write};

/// Every API method, in documentation order.
pub const METHODS: &[MethodSpec] = &[
    m("ping", Read, "Liveness check."),
    m("methods", Read, "List every API method."),
    m("health", Read, "Daemon, database, index and watcher health."),
    m("get_status", Read, "Indexing progress (phase, counts, paused)."),
    m("get_stats", Read, "Query latency, index sizes, ingestion and runtime stats."),
    m("index_state", Read, "File, document and semantic-unit counts; roots; model ids."),
    m("search", Read, "Ranked or exhaustive search. Params: query, limit, offset | start_after, snippets."),
    m("get_snippets", FileRead, "Snippets for up to 50 paths. Params: paths, query."),
    m("read_snippet", FileRead, "Bounded text window around a match. Params: path, query?, max_tokens?."),
    m("contains_path", Read, "Whether a path is indexed. Params: path."),
    m("get_scope_suggestions", Read, "Indexed directories matching a prefix. Params: prefix?, limit?."),
    m("preview", FileRead, "File preview (text head, image size, folder count). Params: path."),
    m("details", FileRead, "Image dimensions. Params: path."),
    m("thumbnail", FileRead, "Cached 240px JPEG thumbnail path. Params: path."),
    m("get_home_data", Read, "Recent files, pins and saved searches."),
    m("toggle_pin", Write, "Pin or unpin a path. Params: path."),
    m("record_open", Write, "Record an open for frecency ranking. Params: path."),
    m("find_duplicates", Read, "Exact duplicates and near-duplicate images. Params: max_distance?, limit?."),
    m("find_similar_images", FileRead, "Visually similar images. Params: path, limit?."),
    m("get_config", Read, "Full configuration."),
    m("save_config", Write, "Replace the configuration. Params: config."),
    m("patch_config", Write, "Merge a partial configuration object. Params: patch."),
    m("setup", Write, "First-run setup; starts the first crawl. Params: watched_dirs, name_only_dirs?, excluded_dirs?, ocr_engine?."),
    m("setup_again", Write, "Mark setup incomplete and stop the current scan."),
    m("suggest_roots", Read, "Common folders under the home directory."),
    m("validate_dir", FileRead, "Canonicalize and validate a folder path. Params: path."),
    m("roots_add", Write, "Add a watched folder. Params: path, name_only?."),
    m("roots_remove", Write, "Remove a watched or name-only folder. Params: path."),
    m("exclude_add", Write, "Add an exclusion. Params: pattern."),
    m("exclude_remove", Write, "Remove an exclusion. Params: pattern."),
    m("pause", Write, "Pause content extraction (the watcher keeps running)."),
    m("resume", Write, "Resume content extraction."),
    m("rescan", Write, "Reload config from disk and re-crawl watched folders."),
    m("reindex", Write, "Agent-facing re-crawl; requires mcp_allow_reindex."),
    m("rebuild", Write, "Wipe and rebuild the index from scratch."),
    m("cancel_setup_scan", Write, "Stop the first-run crawl and mark setup incomplete."),
    m("reset", Write, "Wipe all state on the next daemon start, then stop the daemon. Params: remove_packs?."),
    m("packs_status", Read, "Download state of every model and OCR pack."),
    m("pack_download", Write, "Start a pack download. Params: pack (models|ocr|indra|smart|visual|tessdata)."),
    m("pack_delete", Write, "Delete downloaded semantic model files. Params: pack (models)."),
    m("doctor", Read, "Run all health checks."),
    m("doctor_fix", Write, "Run a repair a doctor check offers. Params: action."),
    m("shutdown", Write, "Stop the daemon."),
];

pub fn find(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|spec| spec.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_names_are_unique() {
        let mut names: Vec<_> = METHODS.iter().map(|m| m.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }

    #[test]
    fn mutating_methods_are_marked_write() {
        for name in ["save_config", "setup", "rebuild", "reset", "pack_download", "shutdown"] {
            assert_eq!(find(name).map(|m| m.access), Some(Access::Write), "{name}");
        }
    }
}
