//! The one Kensaku API: one method table served over daemon JSON-RPC, HTTP `/v1/`, the CLI
//! [`Client`] and in-process [`Engine`]; only the transport differs.

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

/// Executes API methods via an in-process [`Engine`] or daemon [`Client`].
pub trait Kensaku {
    fn call(&self, method: &str, params: Value) -> Result<Value, ApiError>;

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

    fn status(&self) -> Result<crate::indexer::IndexingStatus, ApiError>
    where
        Self: Sized,
    {
        self.call_typed("get_status", json!({}))
    }

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
