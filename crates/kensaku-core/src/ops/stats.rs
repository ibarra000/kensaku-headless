//! Process and index statistics (`get_stats`).

use crate::config::KensakuConfig;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const QUERY_LATENCY_CAPACITY: usize = 256;

const QUERY_HISTORY_SHOWN: usize = 50;

/// Ring buffer of recent query wall times (µs).
static QUERY_LATENCIES: OnceLock<Mutex<VecDeque<u64>>> = OnceLock::new();

static QUERY_TOTAL: AtomicU64 = AtomicU64::new(0);

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// Records the process start time; call once from `run()`.
pub fn init_process_start() {
    PROCESS_START.get_or_init(Instant::now);
}

pub fn record_query_latency(micros: u64) {
    QUERY_TOTAL.fetch_add(1, Ordering::Relaxed);
    let buffer =
        QUERY_LATENCIES.get_or_init(|| Mutex::new(VecDeque::with_capacity(QUERY_LATENCY_CAPACITY)));
    // Stats are best-effort: recover from a poisoned lock.
    let mut guard = match buffer.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.len() >= QUERY_LATENCY_CAPACITY {
        guard.pop_front();
    }
    guard.push_back(micros);
}

fn query_latency_samples() -> Vec<u64> {
    let buffer =
        QUERY_LATENCIES.get_or_init(|| Mutex::new(VecDeque::with_capacity(QUERY_LATENCY_CAPACITY)));
    let guard = match buffer.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.iter().copied().collect()
}

/// Nearest-rank percentile over sorted µs samples.
fn percentile_ms(sorted_micros: &[u64], percentile: f64) -> f64 {
    if sorted_micros.is_empty() {
        return 0.0;
    }
    let rank = ((percentile / 100.0) * sorted_micros.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted_micros.len() - 1);
    sorted_micros[index] as f64 / 1000.0
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Recursively sums regular files; symlinks not followed.
fn dir_size_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            total = total.saturating_add(dir_size_bytes(&entry.path()));
        } else if file_type.is_file() {
            total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
        }
    }
    total
}

/// Sums entries in the config dir whose name starts with `prefix` (e.g. `metadata.db*`).
fn config_prefixed_bytes(prefix: &str, include_dirs: bool) -> u64 {
    let dir = KensakuConfig::config_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(prefix) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if include_dirs {
                total = total.saturating_add(dir_size_bytes(&entry.path()));
            }
        } else if file_type.is_file() {
            total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
        }
    }
    total
}

#[cfg(target_os = "linux")]
fn resident_set_bytes() -> u64 {
    // /proc/self/statm: field 1 is RSS in pages.
    const PAGE_SIZE: u64 = 4096;
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    statm
        .split_whitespace()
        .nth(1)
        .and_then(|pages| pages.parse::<u64>().ok())
        .map(|pages| pages.saturating_mul(PAGE_SIZE))
        .unwrap_or(0)
}

#[cfg(windows)]
fn resident_set_bytes() -> u64 {
    // Working set: resident pages, as Task Manager shows.
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut pmc = PROCESS_MEMORY_COUNTERS::default();
    let read = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    if read.is_ok() {
        pmc.WorkingSetSize as u64
    } else {
        0
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn resident_set_bytes() -> u64 {
    0
}

pub fn stats_for(indexer: &crate::indexer::IndexerManager) -> Result<serde_json::Value, String> {
    let status = indexer.get_status();

    let (files, dirs) = {
        let metadata = indexer.metadata.read();
        let mut files = 0u64;
        let mut dirs = 0u64;
        for i in 0..metadata.len() {
            if metadata.node(i as u32).is_dir() {
                dirs += 1;
            } else {
                files += 1;
            }
        }
        (files, dirs)
    };

    let semantic_units = indexer.unit_store.read().len() as u64;
    // Real Tantivy doc count; `content_indexed` resets every launch.
    let content_docs = indexer
        .content_index
        .read()
        .as_ref()
        .map(|index| index.num_docs())
        .unwrap_or(0);

    let metadata_db_bytes = config_prefixed_bytes("metadata.db", false);
    let tantivy_bytes = dir_size_bytes(&KensakuConfig::index_dir());
    let vector_bytes = {
        let db = indexer.metadata.read().db();
        db.map(|db| db.vector_bytes().unwrap_or(0)).unwrap_or(0)
    };

    let embed_queue_depth = {
        let db = indexer.metadata.read().db();
        db.map(|db| db.count_unembedded().unwrap_or(0)).unwrap_or(0)
    };

    let now_ms = now_unix_ms();
    let now_secs = now_ms / 1000;

    let last_reconcile_secs_ago: Option<u64> = {
        let last = indexer.last_reconcile_unix_secs.load(Ordering::Relaxed);
        if last == 0 {
            None
        } else {
            Some(now_secs.saturating_sub(last))
        }
    };

    let files_per_sec = if status.is_indexing {
        let started_ms = indexer.crawl_start_unix_ms.load(Ordering::Relaxed);
        let elapsed_secs = if started_ms > 0 && now_ms > started_ms {
            (now_ms - started_ms) as f64 / 1000.0
        } else {
            0.0
        };
        if elapsed_secs > 0.0 {
            status.files_indexed as f64 / elapsed_secs
        } else {
            0.0
        }
    } else {
        0.0
    };

    let mut samples = query_latency_samples();
    let total_queries = QUERY_TOTAL.load(Ordering::Relaxed);
    let history: Vec<f64> = samples
        .iter()
        .skip(samples.len().saturating_sub(QUERY_HISTORY_SHOWN))
        .map(|micros| *micros as f64 / 1000.0)
        .collect();
    let last_ms = samples
        .last()
        .map(|micros| *micros as f64 / 1000.0)
        .unwrap_or(0.0);

    samples.sort_unstable();
    let p50_ms = percentile_ms(&samples, 50.0);
    let p95_ms = percentile_ms(&samples, 95.0);
    let p99_ms = percentile_ms(&samples, 99.0);

    let uptime_secs = PROCESS_START.get_or_init(Instant::now).elapsed().as_secs();
    let rss_bytes = resident_set_bytes();
    let watcher_ok = indexer.watcher_ok.load(Ordering::Relaxed);
    let model_id = indexer.config.read().semantic_model.clone();

    Ok(serde_json::json!({
        "queries": {
            "total": total_queries,
            "p50_ms": p50_ms,
            "p95_ms": p95_ms,
            "p99_ms": p99_ms,
            "last_ms": last_ms,
            "history": history,
        },
        "index": {
            "files": files,
            "dirs": dirs,
            "content_docs": content_docs,
            "content_rebuilt": indexer.content_index.read().as_ref().map(|i| i.rebuilt).unwrap_or(false),
            "rebuild_reason": indexer.content_index.read().as_ref().and_then(|i| i.rebuild_reason.clone()),
            "semantic_units": semantic_units,
            "metadata_db_bytes": metadata_db_bytes,
            "tantivy_bytes": tantivy_bytes,
            "vector_bytes": vector_bytes,
            "last_reconcile_secs_ago": last_reconcile_secs_ago,
        },
        "ingestion": {
            "is_indexing": status.is_indexing,
            "phase": status.phase.clone(),
            "files_indexed": status.files_indexed as u64,
            "content_indexed": status.content_indexed as u64,
            "units_indexed": status.units_indexed as u64,
            "visual_units": indexer.visual_store.read().len() as u64,
            "visual_available": crate::embed::visual::visual_available(),
            "files_per_sec": files_per_sec,
            "eta_secs": Option::<u64>::None,
            "embed_queue_depth": embed_queue_depth as u64,
        },
        "runtime": {
            "uptime_secs": uptime_secs,
            "rss_bytes": rss_bytes,
            "watcher_ok": watcher_ok,
            "semantic_ready": status.semantic_ready,
            "model_id": model_id,
            "semantic_provider": crate::embed::ModelRegistry::global().provider_name(),
            "semantic_dim": crate::embed::ModelRegistry::global().dim,
            "semantic_fallback_reason": crate::embed::ModelRegistry::global().fallback_reason.clone(),
        },
    }))
}
