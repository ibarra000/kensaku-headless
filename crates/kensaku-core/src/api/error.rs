//! Stable, transport-independent error codes.

use serde::{Deserialize, Serialize};

/// Machine-readable error category; `as_str` is public contract (JSON-RPC `error.data.code`, HTTP bodies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidParams,
    MethodNotFound,
    /// No index exists yet (run `kensaku setup`).
    NoIndex,
    /// Setup has not been completed.
    NotSetup,
    /// The daemon is not running and could not be started.
    DaemonUnavailable,
    /// Disallowed by scope, write gating or token.
    Forbidden,
    /// Conflicts with work in progress.
    Busy,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidParams => "invalid_params",
            Self::MethodNotFound => "method_not_found",
            Self::NoIndex => "no_index",
            Self::NotSetup => "not_setup",
            Self::DaemonUnavailable => "daemon_unavailable",
            Self::Forbidden => "forbidden",
            Self::Busy => "busy",
            Self::Internal => "internal",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "invalid_params" => Self::InvalidParams,
            "method_not_found" => Self::MethodNotFound,
            "no_index" => Self::NoIndex,
            "not_setup" => Self::NotSetup,
            "daemon_unavailable" => Self::DaemonUnavailable,
            "forbidden" => Self::Forbidden,
            "busy" => Self::Busy,
            _ => Self::Internal,
        }
    }

    pub fn rpc_code(self) -> i64 {
        match self {
            Self::InvalidParams => -32602,
            Self::MethodNotFound => -32601,
            Self::NoIndex => -32010,
            Self::NotSetup => -32011,
            Self::DaemonUnavailable => -32012,
            Self::Forbidden => -32003,
            Self::Busy => -32013,
            Self::Internal => -32000,
        }
    }

    pub fn http_status(self) -> u16 {
        match self {
            Self::InvalidParams => 400,
            Self::Forbidden => 403,
            Self::MethodNotFound | Self::NoIndex => 404,
            Self::Busy | Self::NotSetup => 409,
            Self::DaemonUnavailable => 503,
            Self::Internal => 500,
        }
    }

    /// CLI exit code; 0/1/2/3 are stable.
    pub fn exit_code(self) -> i32 {
        match self {
            Self::InvalidParams | Self::MethodNotFound => 2,
            Self::NoIndex | Self::NotSetup => 3,
            Self::DaemonUnavailable => 4,
            Self::Forbidden => 5,
            Self::Busy => 6,
            Self::Internal => 1,
        }
    }
}

/// Stable [`ErrorCode`] plus a human-readable message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn to_rpc(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code.rpc_code(),
            "message": self.message,
            "data": { "code": self.code.as_str() },
        })
    }

    /// Inverse of [`Self::to_rpc`].
    pub fn from_rpc(error: &serde_json::Value) -> Self {
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("daemon error")
            .to_string();
        let code = error
            .pointer("/data/code")
            .and_then(|c| c.as_str())
            .map(ErrorCode::parse)
            .unwrap_or(match error.get("code").and_then(|c| c.as_i64()) {
                Some(-32602) => ErrorCode::InvalidParams,
                Some(-32601) => ErrorCode::MethodNotFound,
                Some(-32001) | Some(-32002) | Some(-32003) => ErrorCode::Forbidden,
                _ => ErrorCode::Internal,
            });
        Self { code, message }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code.as_str())
    }
}

impl std::error::Error for ApiError {}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        if message == crate::agent::NO_INDEX_MESSAGE {
            return Self::new(ErrorCode::NoIndex, message);
        }
        Self::internal(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_round_trip_preserves_code_and_message() {
        let err = ApiError::new(ErrorCode::Busy, "scan running");
        assert_eq!(ApiError::from_rpc(&err.to_rpc()), err);
    }

    #[test]
    fn legacy_rpc_errors_without_data_map_by_numeric_code() {
        let err = ApiError::from_rpc(&serde_json::json!({"code": -32001, "message": "bad token"}));
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[test]
    fn exit_codes_keep_historical_meanings() {
        assert_eq!(ErrorCode::InvalidParams.exit_code(), 2);
        assert_eq!(ErrorCode::NoIndex.exit_code(), 3);
        assert_eq!(ErrorCode::Internal.exit_code(), 1);
    }

    #[test]
    fn every_code_round_trips_through_its_string_form() {
        for code in [
            ErrorCode::InvalidParams,
            ErrorCode::MethodNotFound,
            ErrorCode::NoIndex,
            ErrorCode::NotSetup,
            ErrorCode::DaemonUnavailable,
            ErrorCode::Forbidden,
            ErrorCode::Busy,
            ErrorCode::Internal,
        ] {
            assert_eq!(ErrorCode::parse(code.as_str()), code);
        }
    }
}
