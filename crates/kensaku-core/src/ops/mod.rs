//! Headless operations.
//!
//! Plain-Rust entry points shared by every surface (CLI, daemon RPC, HTTP,
//! MCP, and Rust embedders). Nothing in here knows about a UI or a transport.

pub mod admin;
pub mod files;
pub mod library;
pub mod search;
pub mod setup;
pub mod stats;
