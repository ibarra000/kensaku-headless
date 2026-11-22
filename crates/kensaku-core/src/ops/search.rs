//! Snippet retrieval; search itself is `IndexerManager::search_page` (see `api::dispatch`).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Snippets fetched in a second round-trip after results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnippetDto {
    pub path: String,
    pub snippet: Option<String>,
}

const MAX_SNIPPET_CHARS: usize = 499;

const MAX_SNIPPET_PATHS: usize = 50;

pub fn compute_snippets(paths: Vec<String>, query: &str) -> Vec<SnippetDto> {
    use rayon::prelude::*;

    let paths: Vec<String> = paths.into_iter().take(MAX_SNIPPET_PATHS).collect();
    paths
        .par_iter()
        .map(|path| {
            let snippet = crate::indexer::content::snippet_for_path(Path::new(path), query)
                .map(|snippet| crate::agent::truncate_chars(&snippet, MAX_SNIPPET_CHARS));
            SnippetDto {
                path: path.clone(),
                snippet,
            }
        })
        .collect()
}
