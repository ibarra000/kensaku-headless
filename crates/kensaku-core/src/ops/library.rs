//! Home view, pins, scope suggestions, duplicates and similar images.

use crate::indexer::IndexerManager;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    /// `"content"` for exact extracted-content matches, `"image"` for
    /// perceptual-hash matches.
    pub kind: String,
    /// Hex content hash or perceptual hash identifying the group.
    pub hash: String,
    pub paths: Vec<String>,
}

/// Finds exact content duplicates and near-duplicate images.
pub fn duplicates_for(
    indexer: &crate::indexer::IndexerManager,
    max_distance: Option<u32>,
    limit: Option<usize>,
) -> Result<Vec<DuplicateGroup>, String> {
    let db = indexer.metadata.read().db();
    let Some(db) = db else {
        return Ok(Vec::new());
    };

    let limit = limit.unwrap_or(200).clamp(1, 1_000);
    let max_distance = max_distance.unwrap_or(10).min(64);
    let mut groups = Vec::new();
    let mut seen: std::collections::HashSet<Vec<String>> = std::collections::HashSet::new();
    let group_key = |paths: &[String]| {
        let mut key = paths.to_vec();
        key.sort();
        key
    };

    for (hash, paths) in db.exact_duplicate_groups(limit)? {
        seen.insert(group_key(&paths));
        groups.push(DuplicateGroup {
            kind: "content".to_string(),
            hash,
            paths,
        });
    }

    // Screenshot bursts are the primary version grouping. A near-duplicate
    // group covering exactly the same paths would be redundant.
    for (look, paths) in db.burst_duplicate_groups(limit)? {
        if !seen.insert(group_key(&paths)) {
            continue;
        }
        groups.push(DuplicateGroup {
            kind: "burst".to_string(),
            hash: format!("{:016x}", look),
            paths,
        });
    }

    for (phash, paths) in db.near_duplicate_groups(max_distance, limit)? {
        if !seen.insert(group_key(&paths)) {
            continue;
        }
        groups.push(DuplicateGroup {
            kind: "image".to_string(),
            hash: format!("{:016x}", phash),
            paths,
        });
    }

    Ok(groups)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeItem {
    pub path: String,
    pub filename: String,
    pub size: u64,
    pub modified: u64,
    pub is_dir: bool,
    /// Last time the file was opened through Kensaku (0 = never).
    pub last_open: u64,
    /// Total opens through Kensaku.
    pub opens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeData {
    pub recent: Vec<HomeItem>,
    pub pins: Vec<HomeItem>,
    pub saved_searches: Vec<crate::config::SavedSearch>,
}

pub fn home_data_for(indexer: &crate::indexer::IndexerManager) -> Result<HomeData, String> {
    let metadata = indexer.metadata.read();
    let config = indexer.config.read().clone();
    let mut recent: Vec<HomeItem> = (0..metadata.len())
        .filter_map(|i| {
            let e = metadata.materialize(i as u32);
            if e.is_dir || e.modified == 0 {
                return None;
            }
            let (opens, last_open) = metadata.get_usage(&e.path);
            Some(HomeItem {
                path: e.path.to_string_lossy().to_string(),
                filename: e.name.clone(),
                size: e.size,
                modified: e.modified,
                is_dir: e.is_dir,
                last_open,
                opens,
            })
        })
        .collect();
    // "Recent" means last touched: opening a file through Kensaku counts
    // (that timestamp lives in `last_open`), otherwise its on-disk modified
    // time. Sort by the later of the two, opens as a secondary signal, and
    // the path as the deterministic tie-break — files generated in one batch
    // share timestamps, and without it the tail of the list would reshuffle
    // on every refetch.
    let touched = |item: &HomeItem| item.last_open.max(item.modified);
    recent.sort_by(|a, b| {
        touched(b)
            .cmp(&touched(a))
            .then_with(|| b.opens.cmp(&a.opens))
            .then_with(|| a.path.cmp(&b.path))
    });
    recent.truncate(12);

    let pins = config
        .pins
        .iter()
        .map(|p| {
            let path = std::path::PathBuf::from(p);
            match metadata.get(&path) {
                Some(e) => {
                    let (opens, last_open) = metadata.get_usage(&e.path);
                    HomeItem {
                        path: p.clone(),
                        filename: e.name.clone(),
                        size: e.size,
                        modified: e.modified,
                        is_dir: e.is_dir,
                        last_open,
                        opens,
                    }
                }
                None => HomeItem {
                    path: p.clone(),
                    filename: path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.clone()),
                    size: 0,
                    modified: 0,
                    is_dir: path.is_dir(),
                    last_open: 0,
                    opens: 0,
                },
            }
        })
        .collect();

    Ok(HomeData {
        recent,
        pins,
        saved_searches: config.saved_searches,
    })
}

pub fn toggle_pin_for(
    indexer: &crate::indexer::IndexerManager,
    path: String,
) -> Result<bool, String> {
    let mut config = indexer.config.read().clone();
    let pinned = if let Some(pos) = config.pins.iter().position(|p| p == &path) {
        config.pins.remove(pos);
        false
    } else {
        config.pins.push(path);
        true
    };
    config.save()?;
    *indexer.config.write() = config;
    Ok(pinned)
}

/// Directory suggestions for the `@` scope menu, derived from indexed dirs.
pub fn scope_suggestions_for(
    indexer: &crate::indexer::IndexerManager,
    prefix: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<String>, String> {
    let prefix = prefix.unwrap_or_default().trim().to_lowercase();
    let limit = limit.unwrap_or(12).clamp(1, 50);
    let metadata = indexer.metadata.read();
    let mut matches: Vec<(u8, String)> = Vec::new();
    for i in 0..metadata.len() {
        let entry = metadata.materialize(i as u32);
        if !entry.is_dir {
            continue;
        }
        let name = entry.name.to_lowercase();
        let path = entry.path.to_string_lossy().to_string();
        let score = if prefix.is_empty() || name.starts_with(&prefix) {
            0
        } else if name.contains(&prefix) {
            1
        } else if path.to_lowercase().contains(&prefix) {
            2
        } else {
            continue;
        };
        matches.push((score, path));
    }
    matches.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.len().cmp(&b.1.len()))
            .then_with(|| a.1.cmp(&b.1))
    });
    Ok(matches
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect())
}

/// Images visually similar to `path` (empty unless a visual encoder exists).
pub fn similar_images(
    indexer: &IndexerManager,
    path: &str,
    limit: Option<usize>,
) -> Result<Vec<crate::embed::visual::SimilarImageResult>, String> {
    indexer.find_similar_images(Path::new(path), limit.unwrap_or(20))
}
