//! The one Kensaku API.
//!
//! Every surface speaks the same method table: the daemon serves it over
//! JSON-RPC, the HTTP server exposes it under `/v1/`, the CLI calls it through
//! a [`Client`], and Rust embedders call it in-process through an [`Engine`].
//! Method names, parameters and results are identical everywhere; only the
//! transport differs.

pub mod client;
pub mod dispatch;
pub mod engine;
pub mod error;
pub mod methods;

pub use client::Client;
pub use dispatch::dispatch;
pub use engine::Engine;
pub use error::{ApiError, ErrorCode};
pub use methods::{Access, METHODS, MethodSpec};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Anything that can execute API methods: an in-process [`Engine`] or a
/// daemon [`Client`]. Typed helpers are provided on top of [`Kensaku::call`].
pub trait Kensaku {
    /// Executes `method` with JSON `params` and returns the JSON result.
    fn call(&self, method: &str, params: Value) -> Result<Value, ApiError>;

    /// [`Kensaku::call`], deserializing the result into `T`.
    fn call_typed<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, ApiError>
    where
        Self: Sized,
    {
        let value = self.call(method, params)?;
        serde_json::from_value(value)
            .map_err(|e| ApiError::internal(format!("unexpected {method} result: {e}")))
    }

    /// Ranked search; `query` accepts the full query language.
    fn search(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<crate::search::SearchResponse, ApiError>
    where
        Self: Sized,
    {
        self.call_typed(
            "search",
            json!({ "query": query, "limit": limit, "offset": offset, "snippets": true }),
        )
    }

    /// Current indexing progress.
    fn status(&self) -> Result<crate::indexer::IndexingStatus, ApiError>
    where
        Self: Sized,
    {
        self.call_typed("get_status", json!({}))
    }

    /// Current configuration.
    fn config(&self) -> Result<crate::config::KensakuConfig, ApiError>
    where
        Self: Sized,
    {
        self.call_typed("get_config", json!({}))
    }

    /// First-run setup: stores the roots and starts the first crawl.
    fn setup(&self, req: &crate::ops::setup::SetupRequest) -> Result<Value, ApiError>
    where
        Self: Sized,
    {
        let params = serde_json::to_value(req).map_err(|e| ApiError::internal(e.to_string()))?;
        self.call("setup", params)
    }
}
