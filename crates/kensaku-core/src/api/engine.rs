//! In-process API for Rust applications that embed Kensaku.

use super::{ApiError, Kensaku, dispatch};
use crate::indexer::{IndexerManager, StatusListener};
use crate::runtime::{Runtime, StartError};
use serde_json::Value;
use std::sync::Arc;

/// Owns the index inside the calling process.
///
/// Takes the same writer lock as the daemon, so an `Engine` and a daemon for
/// the same data directory cannot run together. The data directory is
/// `KENSAKU_DATA_DIR` or the platform default.
///
/// ```no_run
/// use kensaku_core::api::{Engine, Kensaku};
/// use kensaku_core::ops::setup::SetupRequest;
///
/// let engine = Engine::open()?;
/// engine.setup(&SetupRequest { watched_dirs: vec!["/home/me/notes".into()], ..Default::default() })?;
/// let hits = engine.search("meeting notes", 10, 0)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Engine {
    runtime: Runtime,
}

impl Engine {
    /// Opens the stores and starts indexing (when setup is complete).
    pub fn open() -> Result<Self, StartError> {
        Ok(Self {
            runtime: Runtime::start()?,
        })
    }

    /// The live indexer, for callers that need lower-level access.
    pub fn indexer(&self) -> &Arc<IndexerManager> {
        &self.runtime.indexer
    }

    /// Receives every indexing status update (push alternative to polling `get_status`).
    pub fn set_status_listener(&self, listener: StatusListener) {
        self.runtime.indexer.set_status_listener(listener);
    }
}

impl Kensaku for Engine {
    fn call(&self, method: &str, params: Value) -> Result<Value, ApiError> {
        dispatch(&self.runtime.indexer, method, &params)
    }
}
