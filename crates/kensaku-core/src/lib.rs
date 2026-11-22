//! Kensaku: local file and content search engine (headless library).
//! [`api`] is the public contract: in-process [`api::Engine`] or daemon [`api::Client`], same [`api::METHODS`].

pub mod agent;
pub mod api;
pub mod config;
pub mod crashlog;
pub mod daemon;
pub mod doctor;
pub mod embed;
pub mod indexer;
pub mod maintenance;
pub mod models;
pub mod ocr;
pub mod ocr_indra;
pub mod ops;
pub mod preview;
pub mod query;
pub mod runtime;
pub mod search;
pub mod util;
