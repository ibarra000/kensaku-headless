//! Command-line grammar.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "kensaku",
    version,
    about = "Headless local file and content search",
    long_about = "Headless local file and content search.\n\n\
        A background daemon owns the index; every command talks to it and starts \
        it on demand. Use --json for machine-readable output.",
    after_help = "EXIT CODES:\n  0 ok  1 failure  2 usage  3 no index / setup incomplete\n  \
        4 daemon unavailable  5 forbidden  6 busy"
)]
pub struct Cli {
    /// Data directory (default: platform config dir; env KENSAKU_DATA_DIR).
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// Print machine-readable JSON (the raw API result).
    #[arg(long, global = true)]
    pub json: bool,

    /// Never start or use the daemon; read the index directly (read-only commands only).
    #[arg(long, global = true)]
    pub no_daemon: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search names, contents and meaning. Accepts the full query language.
    Search(SearchArgs),
    /// Print a bounded text window from a file around a match.
    Snippet {
        path: String,
        #[arg(long, short)]
        query: Option<String>,
        /// Token budget (32-1024).
        #[arg(long, default_value_t = 256)]
        max_tokens: u64,
    },
    /// Show a file preview (text head, image size, or folder count).
    Preview { path: String },
    /// Show image dimensions for a file.
    Details { path: String },
    /// Generate (or reuse) a 240px JPEG thumbnail and print its path.
    Thumbnail { path: String },

    /// Indexing progress.
    Status {
        /// Keep printing until indexing is idle.
        #[arg(long)]
        follow: bool,
    },
    /// Query latency, index sizes and runtime statistics.
    Stats,
    /// Daemon, database, index and watcher health.
    Health,
    /// Index counts, roots and model ids.
    State,

    /// First-run setup: choose folders and start indexing.
    Setup(SetupArgs),
    /// Manage watched folders.
    #[command(subcommand)]
    Roots(RootsCommand),
    /// Manage exclusions (folder names, paths or patterns).
    #[command(subcommand)]
    Exclude(ExcludeCommand),
    /// Read or change configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Control indexing.
    #[command(subcommand)]
    Index(IndexCommand),
    /// Wipe all Kensaku state (applied by the daemon on its next start).
    Reset {
        /// Required: confirms the wipe.
        #[arg(long)]
        yes: bool,
        /// Also delete downloaded OCR/model packs.
        #[arg(long)]
        remove_packs: bool,
    },
    /// Stop the daemon and delete the entire data directory now.
    RemoveData {
        #[arg(long)]
        yes: bool,
    },

    /// Model and OCR packs.
    #[command(subcommand)]
    Packs(PacksCommand),
    /// Run health checks, optionally applying a repair.
    Doctor {
        /// Repair action named by a check (e.g. reindex, cleanup-storage).
        #[arg(long, value_name = "ACTION")]
        fix: Option<String>,
    },

    /// Recent files, pins and saved searches.
    Home,
    /// Pin or unpin a path.
    Pin { path: String },
    /// Record that a file was opened (feeds frecency ranking).
    RecordOpen { path: String },
    /// Exact duplicates and near-duplicate images.
    Dupes {
        #[arg(long)]
        max_distance: Option<u32>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Visually similar images (needs a visual encoder).
    Similar {
        path: String,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Indexed folders matching a prefix.
    Scopes {
        prefix: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },

    /// Manage the background daemon.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Serve MCP over stdio.
    Mcp,
    /// Serve the loopback HTTP API (/v1), MCP Streamable HTTP and SSE.
    Serve {
        #[arg(long, default_value_t = kensaku_core::agent::http::DEFAULT_PORT)]
        port: u16,
        /// Allow write methods on /v1 (config, setup, indexing control).
        #[arg(long)]
        allow_writes: bool,
    },
    /// Print or install the MCP config for an agent client.
    Connect {
        /// claude-desktop, claude-code or cursor.
        client: String,
        /// Merge the entry into the client's config file (with a .bak backup).
        #[arg(long)]
        write: bool,
    },

    /// Call any API method directly: `kensaku call search '{"query":"x"}'`.
    Call {
        method: String,
        /// JSON object of parameters.
        params: Option<String>,
    },
    /// List every API method.
    Methods,
    /// Print shell completions.
    Completions { shell: clap_complete::Shell },
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Query text; multiple words are joined.
    #[arg(required = true, num_args = 1..)]
    pub query: Vec<String>,
    #[arg(long, short = 'n', default_value_t = 10)]
    pub limit: usize,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    /// Continue an exhaustive listing after this path (keyset cursor).
    #[arg(long, value_name = "PATH")]
    pub after: Option<String>,
    /// Enumerate every match by path instead of ranking.
    #[arg(long)]
    pub exhaustive: bool,
    /// Restrict to a folder (becomes in:"<path>").
    #[arg(long)]
    pub path: Option<String>,
    /// Restrict to an extension (becomes ext:<ext>).
    #[arg(long)]
    pub ext: Option<String>,
    /// Modified within a window, e.g. 1h, 7d, 30d.
    #[arg(long)]
    pub since: Option<String>,
    /// Skip snippet extraction (faster).
    #[arg(long)]
    pub no_snippets: bool,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    #[command(subcommand)]
    pub action: Option<SetupAction>,
    /// Folder to index fully (repeatable).
    #[arg(long = "root", value_name = "DIR")]
    pub roots: Vec<String>,
    /// Folder indexed by name only (repeatable).
    #[arg(long = "name-only", value_name = "DIR")]
    pub name_only: Vec<String>,
    /// Exclusion (repeatable).
    #[arg(long = "exclude", value_name = "PATTERN")]
    pub exclude: Vec<String>,
    /// OCR engine: tesseract, accurate or indra.
    #[arg(long)]
    pub ocr_engine: Option<String>,
    /// Wait for the first scan to finish.
    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Subcommand)]
pub enum SetupAction {
    /// List common folders worth indexing.
    Suggest,
    /// Mark setup incomplete and stop the running scan.
    Again,
}

#[derive(Debug, Subcommand)]
pub enum RootsCommand {
    List,
    Add {
        path: String,
        /// Index names only, not contents.
        #[arg(long)]
        name_only: bool,
    },
    #[command(alias = "remove")]
    Rm { path: String },
}

#[derive(Debug, Subcommand)]
pub enum ExcludeCommand {
    List,
    Add { pattern: String },
    #[command(alias = "remove")]
    Rm { pattern: String },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the whole config, or one key.
    Get { key: Option<String> },
    /// Set one key. VALUE is parsed as JSON, falling back to a string.
    Set { key: String, value: String },
    /// Print the config file path.
    Path,
}

#[derive(Debug, Subcommand)]
pub enum IndexCommand {
    /// Pause content extraction (renames/deletes still apply).
    Pause,
    Resume,
    /// Re-crawl watched folders.
    Rescan {
        #[arg(long)]
        wait: bool,
    },
    /// Wipe and rebuild the index.
    Rebuild {
        #[arg(long)]
        wait: bool,
    },
    /// Stop the first-run scan.
    Cancel,
}

#[derive(Debug, Subcommand)]
pub enum PacksCommand {
    List,
    /// models, ocr, indra, smart, visual or tessdata.
    Download { pack: String },
    Delete { pack: String },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Run in the foreground (what `start` launches).
    Run,
    /// Start in the background if not running.
    Start,
    Stop,
    Status,
    /// Start at login (Run key / systemd user unit / LaunchAgent).
    Install,
    Uninstall,
    /// Print the daemon log path.
    Log,
}
