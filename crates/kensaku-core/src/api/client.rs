//! API client for a daemon in another process.

use super::{ApiError, Kensaku};
use serde_json::Value;

/// Talks to the daemon for the current data directory over local IPC.
#[derive(Debug, Clone, Copy)]
pub struct Client {
    _private: (),
}

impl Client {
    /// Connects to a running daemon, starting one first when none answers.
    pub fn connect() -> Result<Self, ApiError> {
        crate::daemon::ensure_running()?;
        Ok(Self { _private: () })
    }

    /// Connects only if a daemon is already running.
    pub fn connect_existing() -> Result<Self, ApiError> {
        if crate::daemon::is_running() {
            Ok(Self { _private: () })
        } else {
            Err(ApiError::new(
                super::ErrorCode::DaemonUnavailable,
                "no Kensaku daemon is running (start one with `kensaku daemon start`)",
            ))
        }
    }
}

impl Kensaku for Client {
    fn call(&self, method: &str, params: Value) -> Result<Value, ApiError> {
        crate::daemon::call(method, params)
    }
}
