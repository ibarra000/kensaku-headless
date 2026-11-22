//! Executes parsed commands against the daemon (or the read-only index).

use crate::args::*;
use crate::output;
use kensaku_core::agent::{HeadlessIndex, compose_query, connect};
use kensaku_core::api::{ApiError, Client, ErrorCode, Kensaku};
use kensaku_core::config::KensakuConfig;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(750);
/// How long `--wait` gives a requested crawl to actually begin.
const START_GRACE: Duration = Duration::from_secs(8);

pub type CmdResult = Result<(), ApiError>;

pub fn execute(cli: &Cli) -> CmdResult {
    match &cli.command {
        Command::Search(args) => search(cli, args),
        Command::State => read_only_or_daemon(cli, "index_state", |index| index.state()),
        Command::Health => read_only_or_daemon(cli, "health", |index| Ok(index.health_local())),
        Command::Snippet {
            path,
            query,
            max_tokens,
        } => call_print(
            cli,
            "read_snippet",
            json!({ "path": path, "query": query, "max_tokens": max_tokens }),
            |v| println!("{}", v["text"].as_str().unwrap_or("")),
        ),
        Command::Preview { path } => call_pretty(cli, "preview", json!({ "path": path })),
        Command::Details { path } => call_pretty(cli, "details", json!({ "path": path })),
        Command::Thumbnail { path } => call_print(cli, "thumbnail", json!({ "path": path }), |v| {
            println!("{}", v["thumbnail"].as_str().unwrap_or(""))
        }),

        Command::Status { follow } => {
            if *follow {
                wait_for_idle(&client(cli)?, cli.json, Duration::ZERO)
            } else {
                call_print(cli, "get_status", json!({}), output::status_line)
            }
        }
        Command::Stats => call_pretty(cli, "get_stats", json!({})),

        Command::Setup(args) => setup(cli, args),
        Command::Roots(cmd) => roots(cli, cmd),
        Command::Exclude(cmd) => exclude(cli, cmd),
        Command::Config(cmd) => config(cli, cmd),
        Command::Index(cmd) => index(cli, cmd),
        Command::Reset { yes, remove_packs } => {
            require_yes(*yes, "reset")?;
            let value = call(cli, "reset", json!({ "remove_packs": remove_packs }))?;
            kensaku_core::daemon::wait_stopped()?;
            if cli.json {
                output::json(&value);
            } else {
                println!("Reset scheduled; the daemon stopped and will wipe state on its next start.");
            }
            Ok(())
        }
        Command::RemoveData { yes } => remove_data(*yes),

        Command::Packs(cmd) => packs(cli, cmd),
        Command::Doctor { fix } => match fix {
            Some(action) => call_print(cli, "doctor_fix", json!({ "action": action }), |v| {
                println!("{}", v["message"].as_str().unwrap_or("done"))
            }),
            None => call_print(cli, "doctor", json!({}), output::doctor),
        },

        Command::Home => call_pretty(cli, "get_home_data", json!({})),
        Command::Pin { path } => call_print(cli, "toggle_pin", json!({ "path": path }), |v| {
            println!("{}", if v["pinned"].as_bool() == Some(true) { "pinned" } else { "unpinned" })
        }),
        Command::RecordOpen { path } => call_quiet(cli, "record_open", json!({ "path": path })),
        Command::Dupes {
            max_distance,
            limit,
        } => call_pretty(
            cli,
            "find_duplicates",
            json!({ "max_distance": max_distance, "limit": limit }),
        ),
        Command::Similar { path, limit } => call_pretty(
            cli,
            "find_similar_images",
            json!({ "path": path, "limit": limit }),
        ),
        Command::Scopes { prefix, limit } => call_print(
            cli,
            "get_scope_suggestions",
            json!({ "prefix": prefix, "limit": limit }),
            output::lines,
        ),

        Command::Daemon(cmd) => daemon(cli, cmd),
        Command::Mcp => kensaku_core::agent::mcp::serve().map_err(ApiError::internal),
        Command::Serve { port, allow_writes } => serve(*port, *allow_writes),
        Command::Connect { client, write } => connect_client(client, *write),

        Command::Call { method, params } => {
            let params = match params {
                Some(raw) => serde_json::from_str(raw)
                    .map_err(|e| ApiError::invalid(format!("params must be JSON: {e}")))?,
                None => json!({}),
            };
            let value = client(cli)?.call(method, params)?;
            output::json(&value);
            Ok(())
        }
        Command::Methods => {
            if cli.json {
                output::json(&serde_json::to_value(kensaku_core::api::METHODS).unwrap_or_default());
            } else {
                output::methods();
            }
            Ok(())
        }
        Command::Completions { .. } => unreachable!("handled in main"),
    }
}

fn client(cli: &Cli) -> Result<Client, ApiError> {
    if cli.no_daemon {
        return Err(ApiError::new(
            ErrorCode::DaemonUnavailable,
            "this command needs the daemon; drop --no-daemon",
        ));
    }
    Client::connect()
}

fn call(cli: &Cli, method: &str, params: Value) -> Result<Value, ApiError> {
    client(cli)?.call(method, params)
}

/// Prints JSON with `--json`, else `human(result)`.
fn call_print(cli: &Cli, method: &str, params: Value, human: impl FnOnce(&Value)) -> CmdResult {
    let value = call(cli, method, params)?;
    if cli.json {
        output::json(&value);
    } else {
        human(&value);
    }
    Ok(())
}

fn call_pretty(cli: &Cli, method: &str, params: Value) -> CmdResult {
    call_print(cli, method, params, output::json)
}

fn call_quiet(cli: &Cli, method: &str, params: Value) -> CmdResult {
    call_print(cli, method, params, |_| {})
}

/// Read-only commands work without a daemon via `--no-daemon`.
fn read_only_or_daemon(
    cli: &Cli,
    method: &str,
    local: impl FnOnce(&HeadlessIndex) -> Result<Value, String>,
) -> CmdResult {
    if !cli.no_daemon {
        return call_pretty(cli, method, json!({}));
    }
    let index = HeadlessIndex::open().map_err(ApiError::from)?;
    output::json(&local(&index).map_err(ApiError::from)?);
    Ok(())
}

fn search(cli: &Cli, args: &SearchArgs) -> CmdResult {
    let query = compose_query(
        &args.query.join(" "),
        args.path.as_deref(),
        args.ext.as_deref(),
        args.since.as_deref(),
        args.exhaustive || args.after.is_some(),
    );
    let response = if cli.no_daemon {
        let index = HeadlessIndex::open().map_err(ApiError::from)?;
        let response = index
            .search_page(&query, args.limit, args.offset)
            .map_err(ApiError::from)?;
        serde_json::to_value(response).map_err(|e| ApiError::internal(e.to_string()))?
    } else {
        let mut params = json!({
            "query": query,
            "limit": args.limit,
            "offset": args.offset,
            "snippets": !args.no_snippets,
        });
        if let Some(after) = &args.after {
            params["start_after"] = json!(after);
        }
        call(cli, "search", params)?
    };
    if cli.json {
        output::json(&response);
    } else {
        output::search_results(&response);
    }
    Ok(())
}

fn setup(cli: &Cli, args: &SetupArgs) -> CmdResult {
    match args.action {
        Some(SetupAction::Suggest) => return call_print(cli, "suggest_roots", json!({}), output::lines),
        Some(SetupAction::Again) => {
            return call_print(cli, "setup_again", json!({}), |_| {
                println!("Setup marked incomplete; run `kensaku setup --root <dir>` again.")
            });
        }
        None => {}
    }
    if args.roots.is_empty() {
        return Err(ApiError::invalid(
            "setup needs at least one --root <dir> (see `kensaku setup suggest`)",
        ));
    }
    let name_only = (!args.name_only.is_empty()).then(|| args.name_only.clone());
    let params = json!({
        "watched_dirs": args.roots,
        "name_only_dirs": name_only,
        "excluded_dirs": args.exclude,
        "ocr_engine": args.ocr_engine,
    });
    let client = client(cli)?;
    let value = client.call("setup", params)?;
    if cli.json && !args.wait {
        output::json(&value);
        return Ok(());
    }
    if !cli.json {
        println!("Setup saved; indexing {}.", output::join_paths(&value["watched_dirs"]));
    }
    if args.wait {
        wait_for_idle(&client, cli.json, START_GRACE)?;
    }
    Ok(())
}

fn current_config(cli: &Cli) -> Result<KensakuConfig, ApiError> {
    client(cli)?.config()
}

fn roots(cli: &Cli, cmd: &RootsCommand) -> CmdResult {
    match cmd {
        RootsCommand::List => {
            let config = current_config(cli)?;
            if cli.json {
                output::json(&json!({
                    "watched_dirs": config.watched_dirs,
                    "name_only_dirs": config.name_only_dirs,
                }));
            } else {
                config.watched_dirs.iter().for_each(|p| println!("{}", p.display()));
                config
                    .name_only_dirs
                    .iter()
                    .for_each(|p| println!("{}\t(names only)", p.display()));
            }
            Ok(())
        }
        RootsCommand::Add { path, name_only } => call_print(
            cli,
            "roots_add",
            json!({ "path": path, "name_only": name_only }),
            |_| println!("Added; indexing it now."),
        ),
        RootsCommand::Rm { path } => call_print(cli, "roots_remove", json!({ "path": path }), |_| {
            println!("Removed.")
        }),
    }
}

fn exclude(cli: &Cli, cmd: &ExcludeCommand) -> CmdResult {
    match cmd {
        ExcludeCommand::List => {
            let config = current_config(cli)?;
            if cli.json {
                output::json(&json!(config.excluded_dirs));
            } else {
                config.excluded_dirs.iter().for_each(|p| println!("{p}"));
            }
            Ok(())
        }
        ExcludeCommand::Add { pattern } => {
            call_print(cli, "exclude_add", json!({ "pattern": pattern }), |_| println!("Added."))
        }
        ExcludeCommand::Rm { pattern } => call_print(
            cli,
            "exclude_remove",
            json!({ "pattern": pattern }),
            |_| println!("Removed."),
        ),
    }
}

fn config(cli: &Cli, cmd: &ConfigCommand) -> CmdResult {
    match cmd {
        ConfigCommand::Path => {
            println!("{}", KensakuConfig::config_path().display());
            Ok(())
        }
        ConfigCommand::Get { key } => {
            let value = call(cli, "get_config", json!({}))?;
            match key {
                None => output::json(&value),
                Some(key) => {
                    let field = value.get(key).ok_or_else(|| {
                        ApiError::invalid(format!("unknown config key '{key}'"))
                    })?;
                    output::scalar_or_json(field);
                }
            }
            Ok(())
        }
        ConfigCommand::Set { key, value } => {
            let parsed = serde_json::from_str::<Value>(value).unwrap_or_else(|_| json!(value));
            let mut patch = serde_json::Map::new();
            patch.insert(key.clone(), parsed);
            call_print(cli, "patch_config", json!({ "patch": patch }), |_| println!("Saved."))
        }
    }
}

fn index(cli: &Cli, cmd: &IndexCommand) -> CmdResult {
    let (method, wait) = match cmd {
        IndexCommand::Pause => ("pause", false),
        IndexCommand::Resume => ("resume", false),
        IndexCommand::Rescan { wait } => ("rescan", *wait),
        IndexCommand::Rebuild { wait } => ("rebuild", *wait),
        IndexCommand::Cancel => ("cancel_setup_scan", false),
    };
    let client = client(cli)?;
    let value = client.call(method, json!({}))?;
    if wait {
        return wait_for_idle(&client, cli.json, START_GRACE);
    }
    if cli.json {
        output::json(&value);
    } else {
        println!("{method}: ok");
    }
    Ok(())
}

fn packs(cli: &Cli, cmd: &PacksCommand) -> CmdResult {
    match cmd {
        PacksCommand::List => call_print(cli, "packs_status", json!({}), output::packs),
        PacksCommand::Download { pack } => call_print(
            cli,
            "pack_download",
            json!({ "pack": pack }),
            |_| println!("Downloading {pack}; check progress with `kensaku packs list`."),
        ),
        PacksCommand::Delete { pack } => {
            call_print(cli, "pack_delete", json!({ "pack": pack }), |_| println!("Deleted."))
        }
    }
}

fn remove_data(yes: bool) -> CmdResult {
    require_yes(yes, "remove-data")?;
    // The daemon holds the stores open; stop it before deleting them.
    kensaku_core::daemon::stop()?;
    let report = kensaku_core::maintenance::remove_all_data().map_err(ApiError::internal)?;
    println!("Removed {} ({} reclaimed).", report.path, report.human_bytes());
    Ok(())
}

fn daemon(cli: &Cli, cmd: &DaemonCommand) -> CmdResult {
    use kensaku_core::daemon::{self as d, server, service};
    match cmd {
        DaemonCommand::Run => match server::run() {
            Ok(()) => Ok(()),
            Err(server::ServeError::AlreadyRunning) => {
                eprintln!("kensaku: a daemon is already running for this data directory");
                Ok(())
            }
            Err(e) => Err(ApiError::internal(e.to_string())),
        },
        DaemonCommand::Start => {
            let spawned = d::ensure_running()?;
            println!("{}", if spawned { "started" } else { "already running" });
            Ok(())
        }
        DaemonCommand::Stop => {
            let stopped = d::stop()?;
            println!("{}", if stopped { "stopped" } else { "not running" });
            Ok(())
        }
        DaemonCommand::Status => {
            let running = d::is_running();
            let value = json!({
                "running": running,
                "data_dir": KensakuConfig::config_dir(),
                "autostart": service::is_installed(),
                "log": d::log_path(),
            });
            if cli.json {
                output::json(&value);
            } else {
                println!("{}", if running { "running" } else { "stopped" });
                println!("data dir:  {}", KensakuConfig::config_dir().display());
                println!("autostart: {}", if service::is_installed() { "on" } else { "off" });
            }
            Ok(())
        }
        DaemonCommand::Install => {
            let location = service::install().map_err(ApiError::internal)?;
            println!("Registered to start at login: {location}");
            Ok(())
        }
        DaemonCommand::Uninstall => {
            service::uninstall().map_err(ApiError::internal)?;
            println!("Removed start-at-login registration.");
            Ok(())
        }
        DaemonCommand::Log => {
            println!("{}", d::log_path().display());
            Ok(())
        }
    }
}

fn serve(port: u16, allow_writes: bool) -> CmdResult {
    use kensaku_core::agent::http;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    http::install_ctrl_c();
    let shutdown = Arc::new(AtomicBool::new(false));
    let options = http::ServeOptions { allow_writes };
    http::serve(&KensakuConfig::config_dir(), port, shutdown, options)
        .map_err(|e| ApiError::internal(e.to_string()))
}

fn connect_client(name: &str, write: bool) -> CmdResult {
    let client = connect::Client::parse(name).ok_or_else(|| {
        let ids: Vec<_> = connect::Client::ALL.iter().map(|c| c.id()).collect();
        ApiError::invalid(format!("connect requires one of: {}", ids.join(", ")))
    })?;
    if write {
        let outcome = connect::install(client).map_err(ApiError::internal)?;
        println!("{}", outcome.message);
        if let Some(backup) = outcome.backup {
            println!("backup: {backup}");
        }
    } else {
        println!("{}", connect::config_snippet(client).map_err(ApiError::internal)?);
        let target = connect::primary_path(client)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| client.name().to_string());
        println!("\n# paste into {target} (or rerun with --write)");
    }
    Ok(())
}

fn require_yes(yes: bool, command: &str) -> CmdResult {
    if yes {
        Ok(())
    } else {
        Err(ApiError::invalid(format!(
            "{command} is destructive; rerun with --yes to confirm"
        )))
    }
}

/// Polls `get_status` until idle; `grace` is how long a requested crawl gets to begin before idle means finished.
fn wait_for_idle(client: &Client, json_out: bool, grace: Duration) -> CmdResult {
    let started = Instant::now();
    let mut seen_running = false;
    let mut last_line = String::new();
    loop {
        let status = client.call("get_status", json!({}))?;
        // Paused extraction never finishes on its own; treat it as settled.
        let paused = status["paused"].as_bool().unwrap_or(false);
        let running = !paused
            && (status["is_indexing"].as_bool().unwrap_or(false)
                || status["phase"].as_str().is_some_and(|p| p != "idle"));
        seen_running |= running;
        let line = output::status_summary(&status);
        if line != last_line {
            if json_out {
                println!("{status}");
            } else {
                eprintln!("{line}");
            }
            last_line = line;
        }
        if !running && (seen_running || started.elapsed() >= grace) {
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
