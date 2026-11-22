use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingProviderKind {
    /// Potion-base-8M static table (or hashing fallback).
    Static,
    /// Downloaded ONNX model pack.
    Smart,
    /// Lexical only.
    None,
}

impl std::fmt::Display for EmbeddingProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static => write!(f, "static"),
            Self::Smart => write!(f, "smart"),
            Self::None => write!(f, "none"),
        }
    }
}
