//! Method table: maps every [`super::METHODS`] entry onto a live [`IndexerManager`].
//! Shared by the daemon (and thus HTTP `/v1`) and the in-process [`super::Engine`].

use super::error::{ApiError, ErrorCode};
use crate::config::KensakuConfig;
use crate::indexer::IndexerManager;
use crate::ops;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Serializes read-modify-write config changes so concurrent updates aren't lost.
static CONFIG_MUTATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub const API_VERSION: u64 = crate::daemon::PROTOCOL_VERSION;

/// Executes `method`. `reset`/`shutdown` only write markers here; stopping the process is the transport's job.
pub fn dispatch(
    indexer: &Arc<IndexerManager>,
    method: &str,
    params: &Value,
) -> Result<Value, ApiError> {
    match method {
        "ping" => Ok(json!({ "ok": true, "protocol_version": API_VERSION })),
        "methods" => to_value(super::METHODS),
        "health" => Ok(health(indexer)),
        "get_status" => to_value(indexer.get_status()),
        "get_stats" => Ok(ops::stats::stats_for(indexer)?),
        "index_state" => Ok(indexer.state_json()?),

        "search" => search(indexer, params),
        "get_snippets" => {
            let paths: Vec<String> = params
                .get("paths")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let query = opt_str(params, "query").unwrap_or("");
            to_value(ops::search::compute_snippets(paths, query))
        }
        "read_snippet" => read_snippet(indexer, params),
        "contains_path" => {
            let path = req_str(params, "path")?;
            Ok(json!({ "indexed": is_indexed(indexer, path) }))
        }
        "get_scope_suggestions" => {
            let prefix = opt_str(params, "prefix").map(str::to_string);
            let limit = opt_u64(params, "limit").map(|v| v as usize);
            to_value(ops::library::scope_suggestions_for(indexer, prefix, limit)?)
        }

        "preview" => to_value(ops::files::build_file_preview(req_str(params, "path")?)?),
        "details" => to_value(ops::files::file_details(req_str(params, "path")?)),
        "thumbnail" => {
            let path = ops::files::generate_thumbnail(req_str(params, "path")?)?;
            Ok(json!({ "thumbnail": path }))
        }

        "get_home_data" => to_value(ops::library::home_data_for(indexer)?),
        "toggle_pin" => {
            let path = req_str(params, "path")?.to_string();
            let pinned = ops::library::toggle_pin_for(indexer, path)?;
            Ok(json!({ "pinned": pinned }))
        }
        "record_open" => {
            let path = req_str(params, "path")?;
            indexer.metadata.read().record_open(Path::new(path));
            Ok(json!({ "ok": true }))
        }
        "find_duplicates" => {
            let max_distance = opt_u64(params, "max_distance").map(|v| v as u32);
            let limit = opt_u64(params, "limit").map(|v| v as usize);
            to_value(ops::library::duplicates_for(indexer, max_distance, limit)?)
        }
        "find_similar_images" => {
            let path = req_str(params, "path")?;
            let limit = opt_u64(params, "limit").map(|v| v as usize);
            to_value(ops::library::similar_images(indexer, path, limit)?)
        }

        "get_config" => to_value(indexer.config.read().clone()),
        "save_config" => {
            let config: KensakuConfig = serde_json::from_value(
                params.get("config").cloned().unwrap_or(Value::Null),
            )
            .map_err(|e| ApiError::invalid(format!("config: {e}")))?;
            let _guard = config_lock();
            apply_config(indexer, validated_roots(config)?)
        }
        "patch_config" => {
            let patch = params
                .get("patch")
                .filter(|p| p.is_object())
                .ok_or_else(|| ApiError::invalid("patch must be a JSON object"))?;
            let _guard = config_lock();
            let config = patch_config(&indexer.config.read(), patch)?;
            apply_config(indexer, validated_roots(config)?)
        }
        "setup" => {
            let req: ops::setup::SetupRequest = serde_json::from_value(params.clone())
                .map_err(|e| ApiError::invalid(format!("setup: {e}")))?;
            let config = ops::setup::complete_setup(indexer, &req).map_err(ApiError::invalid)?;
            Ok(json!({ "started": true, "watched_dirs": config.watched_dirs }))
        }
        "setup_again" => {
            ops::setup::run_setup_again(indexer)?;
            ok()
        }
        "suggest_roots" => to_value(ops::setup::suggested_directories()),
        "validate_dir" => {
            let path = ops::setup::validate_directory(req_str(params, "path")?)
                .map_err(ApiError::invalid)?;
            Ok(json!({ "path": path }))
        }
        "roots_add" => {
            let path = ops::setup::validate_directory(req_str(params, "path")?)
                .map_err(ApiError::invalid)?;
            let name_only = params.get("name_only").and_then(Value::as_bool).unwrap_or(false);
            let _guard = config_lock();
            let config = with_root_added(&indexer.config.read(), &path, name_only);
            apply_config(indexer, config)
        }
        "roots_remove" => {
            let path = req_str(params, "path")?;
            let _guard = config_lock();
            let config = with_root_removed(&indexer.config.read(), path)?;
            if config.watched_dirs.is_empty() && config.name_only_dirs.is_empty() {
                return Err(ApiError::invalid(
                    "cannot remove the last watched folder; add another first or run `setup_again`",
                ));
            }
            apply_config(indexer, config)
        }
        "exclude_add" => {
            let pattern = req_str(params, "pattern")?;
            let _guard = config_lock();
            let mut config = indexer.config.read().clone();
            if !config.excluded_dirs.iter().any(|p| p == pattern) {
                config.excluded_dirs.push(pattern.to_string());
            }
            apply_config(indexer, config)
        }
        "exclude_remove" => {
            let pattern = req_str(params, "pattern")?;
            let _guard = config_lock();
            let mut config = indexer.config.read().clone();
            let before = config.excluded_dirs.len();
            config.excluded_dirs.retain(|p| p != pattern);
            if config.excluded_dirs.len() == before {
                return Err(ApiError::invalid(format!("not an exclusion: {pattern}")));
            }
            apply_config(indexer, config)
        }

        "pause" => {
            let _guard = config_lock();
            ops::admin::set_paused(indexer, true)?;
            ok()
        }
        "resume" => {
            let _guard = config_lock();
            ops::admin::set_paused(indexer, false)?;
            ok()
        }
        "rescan" => {
            require_setup(indexer)?;
            // Reload config from disk (a broken file is an error, never defaults), then wait out a finishing crawl.
            let on_disk = KensakuConfig::try_load().map_err(ApiError::invalid)?;
            let _guard = config_lock();
            indexer.apply_config(on_disk)?;
            indexer.start_indexing_when_idle();
            Ok(json!({ "started": true }))
        }
        "reindex" => {
            if !indexer.config.read().mcp_allow_reindex {
                return Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "reindex is disabled; enable mcp_allow_reindex in config.json",
                ));
            }
            require_setup(indexer)?;
            indexer.start_indexing();
            Ok(json!({ "started": true }))
        }
        "rebuild" => {
            require_setup(indexer)?;
            Arc::clone(indexer).rebuild_all()?;
            Ok(json!({ "started": true }))
        }
        "cancel_setup_scan" => {
            indexer.cancel_setup_scan()?;
            ok()
        }
        "reset" => {
            let remove_packs = params
                .get("remove_packs")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            crate::runtime::request_reset(remove_packs)?;
            Ok(json!({ "scheduled": true, "remove_packs": remove_packs }))
        }

        "packs_status" => Ok(packs_status()),
        "pack_download" => {
            let pack = req_str(params, "pack")?;
            pack_download(pack)?;
            Ok(json!({ "started": true, "pack": pack }))
        }
        "pack_delete" => match req_str(params, "pack")? {
            "models" => {
                crate::models::delete_all()?;
                ok()
            }
            other => Err(ApiError::invalid(format!(
                "pack_delete supports only 'models', got '{other}'"
            ))),
        },

        "doctor" => to_value(crate::doctor::run(indexer)),
        "doctor_fix" => {
            let message = ops::admin::doctor_fix(indexer, req_str(params, "action")?)
                .map_err(|e| {
                    if e.starts_with("Unknown repair") {
                        ApiError::invalid(e)
                    } else {
                        ApiError::internal(e)
                    }
                })?;
            Ok(json!({ "message": message }))
        }
        "shutdown" => ok(),

        other => Err(ApiError::new(
            ErrorCode::MethodNotFound,
            format!("unknown method '{other}'"),
        )),
    }
}

fn ok() -> Result<Value, ApiError> {
    Ok(json!({ "ok": true }))
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|e| ApiError::internal(e.to_string()))
}

fn req_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    opt_str(params, key)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::invalid(format!("missing required parameter '{key}'")))
}

fn opt_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn opt_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

fn require_setup(indexer: &IndexerManager) -> Result<(), ApiError> {
    if indexer.config.read().is_setup_complete {
        Ok(())
    } else {
        Err(ApiError::new(
            ErrorCode::NotSetup,
            "setup is not complete; run `kensaku setup --root <dir>` first",
        ))
    }
}

fn config_lock() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_MUTATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Saves `config`; on scope change queues a crawl that waits out a running one (else new roots are skipped).
fn apply_config(indexer: &Arc<IndexerManager>, config: KensakuConfig) -> Result<Value, ApiError> {
    let scope_changed = {
        let current = indexer.config.read();
        current.watched_dirs != config.watched_dirs
            || current.name_only_dirs != config.name_only_dirs
            || current.excluded_dirs != config.excluded_dirs
    };
    indexer.apply_config(config)?;
    if scope_changed {
        indexer.start_indexing_when_idle();
    }
    to_value(indexer.config.read().clone())
}

/// Canonicalizes and checks every root, as `setup`/`roots_add` do.
fn validated_roots(mut config: KensakuConfig) -> Result<KensakuConfig, ApiError> {
    let check = |dirs: &[std::path::PathBuf]| -> Result<Vec<std::path::PathBuf>, ApiError> {
        dirs.iter()
            .map(|dir| {
                ops::setup::validate_directory(&dir.to_string_lossy())
                    .map(std::path::PathBuf::from)
                    .map_err(ApiError::invalid)
            })
            .collect()
    };
    config.watched_dirs = check(&config.watched_dirs)?;
    config.name_only_dirs = check(&config.name_only_dirs)?;
    Ok(config)
}

/// Overlays top-level keys of `patch` onto `current`.
pub fn patch_config(current: &KensakuConfig, patch: &Value) -> Result<KensakuConfig, ApiError> {
    let mut merged = serde_json::to_value(current).map_err(|e| ApiError::internal(e.to_string()))?;
    let (Some(target), Some(source)) = (merged.as_object_mut(), patch.as_object()) else {
        return Err(ApiError::invalid("patch must be a JSON object"));
    };
    for (key, value) in source {
        if !target.contains_key(key) {
            return Err(ApiError::invalid(format!("unknown config key '{key}'")));
        }
        target.insert(key.clone(), value.clone());
    }
    serde_json::from_value(merged).map_err(|e| ApiError::invalid(format!("patch: {e}")))
}

fn with_root_added(current: &KensakuConfig, path: &str, name_only: bool) -> KensakuConfig {
    let mut config = current.clone();
    let path = std::path::PathBuf::from(path);
    let list = if name_only {
        &mut config.name_only_dirs
    } else {
        &mut config.watched_dirs
    };
    if !list.contains(&path) {
        list.push(path);
    }
    config
}

fn with_root_removed(current: &KensakuConfig, path: &str) -> Result<KensakuConfig, ApiError> {
    let mut config = current.clone();
    let canonical = ops::setup::validate_directory(path).ok();
    let matches = |p: &std::path::PathBuf| {
        p.as_os_str() == path || canonical.as_deref().is_some_and(|c| p.as_os_str() == c)
    };
    let before = config.watched_dirs.len() + config.name_only_dirs.len();
    config.watched_dirs.retain(|p| !matches(p));
    config.name_only_dirs.retain(|p| !matches(p));
    if config.watched_dirs.len() + config.name_only_dirs.len() == before {
        return Err(ApiError::invalid(format!("not a watched folder: {path}")));
    }
    Ok(config)
}

fn search(indexer: &IndexerManager, params: &Value) -> Result<Value, ApiError> {
    require_setup(indexer)?;
    let query = opt_str(params, "query").unwrap_or("");
    let limit = opt_u64(params, "limit").unwrap_or(10).clamp(1, 10_000) as usize;
    let with_snippets = params
        .get("snippets")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let started = Instant::now();
    let response = match opt_str(params, "start_after") {
        Some(start_after) => {
            indexer.search_page_after(query, limit, start_after, with_snippets)?
        }
        None => {
            let offset = opt_u64(params, "offset").unwrap_or(0) as usize;
            indexer.search_page(query, limit, offset, with_snippets)?
        }
    };
    ops::stats::record_query_latency(started.elapsed().as_micros() as u64);
    to_value(response)
}

fn read_snippet(indexer: &IndexerManager, params: &Value) -> Result<Value, ApiError> {
    let path = req_str(params, "path")?;
    let allowed = {
        // Reachable by every local agent client: enforce agent scope and denylist.
        let cfg = indexer.config.read();
        crate::agent::agent_read_allowed(&cfg.agent_scoped_roots, &cfg.excluded_extensions, path)
    };
    if !allowed {
        return Err(ApiError::new(
            ErrorCode::Forbidden,
            "read_snippet: path is outside the configured agent scope",
        ));
    }
    if !is_indexed(indexer, path) {
        return Err(ApiError::invalid("read_snippet: path is not part of the index"));
    }
    let query = opt_str(params, "query");
    let max_tokens = opt_u64(params, "max_tokens").unwrap_or(256).clamp(32, 1024) as usize;
    let text = crate::agent::mcp::extract_text(Path::new(path))
        .ok_or_else(|| ApiError::internal(format!("Could not read text from {path}")))?;
    let (line_start, line_end, window, truncated) =
        crate::agent::mcp::read_snippet_window(&text, query, max_tokens);
    Ok(json!({
        "path": path,
        "line_start": line_start,
        "line_end": line_end,
        "text": window,
        "truncated": truncated,
    }))
}

/// Whether `path` is indexed, by exact spelling or canonical form.
pub fn is_indexed(indexer: &IndexerManager, path: &str) -> bool {
    let requested = Path::new(path);
    let metadata = indexer.metadata.read();
    metadata.get(requested).is_some()
        || std::fs::canonicalize(requested)
            .ok()
            .is_some_and(|canonical| metadata.get(&canonical).is_some())
}

fn health(indexer: &IndexerManager) -> Value {
    let base = json!({
        "ok": true,
        "daemon": true,
        "protocol_version": API_VERSION,
        "pid": std::process::id(),
        "watcher_ok": indexer.watcher_ok(),
        "setup_complete": indexer.config.read().is_setup_complete,
    });
    let mut out = base;
    let extra = match indexer.state_json() {
        Ok(state) => json!({
            "db": true,
            "index": state["content_index_ready"].as_bool().unwrap_or(false),
            "model": state["active_model_id"].as_u64().is_some(),
            "state": state,
        }),
        Err(e) => json!({ "db": false, "index": false, "model": false, "error": e }),
    };
    if let (Some(target), Some(source)) = (out.as_object_mut(), extra.as_object()) {
        target.extend(source.clone());
    }
    out
}

fn packs_status() -> Value {
    json!({
        "models": crate::models::status(),
        "ocr": crate::models::ocr_status(),
        "indra": crate::models::indra_status(),
        "smart": crate::models::smart_status(),
        "visual": crate::models::visual_status(),
        "tessdata": crate::indexer::extract::image::ocr_status(),
    })
}

pub const PACKS: [&str; 6] = ["models", "ocr", "indra", "smart", "visual", "tessdata"];

fn pack_download(pack: &str) -> Result<(), ApiError> {
    match pack {
        "models" => crate::models::download_all()?,
        "ocr" => crate::models::download_ocr_pack()?,
        "indra" => crate::models::download_indra_pack()?,
        "smart" => crate::models::download_smart_pack()?,
        "visual" => crate::models::download_visual_pack()?,
        "tessdata" => {
            std::thread::Builder::new()
                .name("kensaku-tessdata-download".into())
                .spawn(|| {
                    if let Err(e) = crate::indexer::extract::image::download_eng_tessdata() {
                        crate::say_err!("[Kensaku] tessdata download failed: {e}");
                    }
                })
                .map_err(|e| ApiError::internal(e.to_string()))?;
        }
        other => {
            return Err(ApiError::invalid(format!(
                "unknown pack '{other}'; expected one of {}",
                PACKS.join(", ")
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_config_overlays_known_keys_and_rejects_unknown_ones() {
        let base = KensakuConfig::default();
        let patched = patch_config(&base, &json!({ "max_results": 7 })).unwrap();
        assert_eq!(patched.max_results, 7);

        let err = patch_config(&base, &json!({ "no_such_key": 1 })).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(patch_config(&base, &json!([1])).is_err());
    }

    #[test]
    fn roots_add_is_idempotent_and_remove_reports_unknown_paths() {
        let base = KensakuConfig {
            watched_dirs: vec![],
            name_only_dirs: vec![],
            ..KensakuConfig::default()
        };
        let once = with_root_added(&base, "/data/a", false);
        let twice = with_root_added(&once, "/data/a", false);
        assert_eq!(twice.watched_dirs.len(), 1);

        let named = with_root_added(&twice, "/data/b", true);
        assert_eq!(named.name_only_dirs.len(), 1);

        let removed = with_root_removed(&named, "/data/a").unwrap();
        assert!(removed.watched_dirs.is_empty());
        assert!(with_root_removed(&removed, "/data/zzz").is_err());
    }

    #[test]
    fn every_registered_method_is_dispatched() {
        // Every registered method must not yield MethodNotFound; read-only manager, side-effect methods skipped.
        let indexer = Arc::new(IndexerManager::new_readonly(KensakuConfig::default()));
        let side_effects = [
            "save_config",
            "patch_config",
            "setup",
            "setup_again",
            "roots_add",
            "roots_remove",
            "exclude_add",
            "exclude_remove",
            "pause",
            "resume",
            "rescan",
            "rebuild",
            "cancel_setup_scan",
            "reset",
            "pack_download",
            "pack_delete",
            "doctor_fix",
            "toggle_pin",
            "record_open",
            "thumbnail",
        ];
        for spec in super::super::METHODS {
            if side_effects.contains(&spec.name) {
                continue;
            }
            let result = dispatch(&indexer, spec.name, &json!({}));
            if let Err(e) = &result {
                assert_ne!(e.code, ErrorCode::MethodNotFound, "{} not dispatched", spec.name);
            }
        }
        let err = dispatch(&indexer, "nope", &json!({})).unwrap_err();
        assert_eq!(err.code, ErrorCode::MethodNotFound);
    }
}
