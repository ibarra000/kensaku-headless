//! Kensaku: local file and content search engine.
//!
//! Headless library. [`api`] is the public contract: [`api::Engine`] runs the
//! index in-process, [`api::Client`] talks to the `kensaku` daemon, and both
//! execute the same method table ([`api::METHODS`]).

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
