//! Best-effort local crash log at `<config_dir>/crash.log`; logging must never
//! itself panic.

use crate::config::KensakuConfig;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 256 * 1024;

const TAIL_READ_BYTES: u64 = 32 * 1024;

const FILE_NAME: &str = "crash.log";

/// Milestones go to a separate file so the crash log holds only failures.
const INFO_FILE_NAME: &str = "kensaku.log";

pub fn path() -> PathBuf {
    KensakuConfig::config_dir().join(FILE_NAME)
}

/// Milestone log path (non-failure lines).
pub fn info_path() -> PathBuf {
    KensakuConfig::config_dir().join(INFO_FILE_NAME)
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Panic from std print macros on a closed stream; not an app fault.
fn is_broken_pipe(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("failed printing to stdout")
        || lower.contains("failed printing to stderr")
        || lower.contains("the pipe is being closed")
        || lower.contains("broken pipe")
}

/// Installs the panic hook; call once, before config load. Chains the previous hook.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        // A closed stdout/stderr is not a crash; recording it would flag healthy installs forever.
        let message = panic_message(info);
        if is_broken_pipe(&message) {
            previous(info);
            return;
        }

        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "<unknown location>".to_string());

        let mut entry = format!("[{}] {} {}\n", now_unix_secs(), location, message);

        // Backtraces are expensive; capture only when requested.
        if let Some(value) = std::env::var_os("RUST_BACKTRACE")
            && value.to_string_lossy() != "0"
        {
            entry.push_str(&format!("{}\n", std::backtrace::Backtrace::force_capture()));
        }

        append_line(&path(), &entry);
        previous(info);
    }));
}

fn append_line(path: &Path, entry: &str) {
    if path
        .parent()
        .is_some_and(|parent| std::fs::create_dir_all(parent).is_err())
    {
        return;
    }

    rotate_if_needed(path, entry.len() as u64);

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(entry.as_bytes());
    }
}

/// Rotates to `crash.log.1` before crossing [`MAX_BYTES`]; at most two files are kept.
fn rotate_if_needed(path: &Path, incoming: u64) {
    let len = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if len == 0 || len.saturating_add(incoming) <= MAX_BYTES {
        return;
    }

    let rotated = rotated_path(path);
    let _ = std::fs::remove_file(&rotated);
    let _ = std::fs::rename(path, &rotated);
}

pub fn tail(limit: usize) -> Vec<String> {
    tail_from(&path(), limit)
}

/// Last `limit` non-empty lines (oldest first), reading at most [`TAIL_READ_BYTES`].
fn tail_from(path: &Path, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }

    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_READ_BYTES);

    // Drop a partial first line when the window starts mid-line.
    let mut starts_mid_line = false;
    if start > 0 {
        let mut previous_byte = [0u8; 1];
        if file.seek(SeekFrom::Start(start - 1)).is_ok()
            && file.read_exact(&mut previous_byte).is_ok()
        {
            starts_mid_line = previous_byte[0] != b'\n';
        }
    }

    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }

    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if starts_mid_line && !lines.is_empty() {
        lines.remove(0);
    }

    let skip = lines.len().saturating_sub(limit);
    lines
        .into_iter()
        .skip(skip)
        .map(|line| line.to_string())
        .collect()
}

pub fn record_error(context: &str, err: &str) {
    record_error_at(&path(), context, err);
}

/// Records a non-fatal milestone to [`INFO_FILE_NAME`], never the crash log.
pub fn info(message: &str) {
    append_line(
        &info_path(),
        &format!("[{}] INFO {}\n", now_unix_secs(), message),
    );
}

/// Pre-1.0.16 logs mix `INFO` lines in; the message follows the bracketed timestamp.
fn is_failure_line(line: &str) -> bool {
    let body = match line.find(']') {
        Some(end) => line[end + 1..].trim_start(),
        None => line.trim_start(),
    };
    !body.starts_with("INFO ")
}

/// Most recent failure line; `None` is normal on a healthy install.
pub fn last_failure() -> Option<String> {
    last_failure_at(&path())
}

fn last_failure_at(path: &Path) -> Option<String> {
    tail_from(path, 64)
        .into_iter()
        .rev()
        .find(|line| is_failure_line(line))
}

fn record_error_at(path: &Path, context: &str, err: &str) {
    append_line(
        path,
        &format!("[{}] ERROR {}: {}\n", now_unix_secs(), context, err),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh directory + crash.log path; remove with [`cleanup`].
    fn temp_log(tag: &str) -> (PathBuf, PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "kensaku-crashlog-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join(FILE_NAME);
        (dir, log)
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rotation_moves_oversized_log_to_dot_one() {
        let (dir, log) = temp_log("rotate");
        let chunk = "x".repeat(8 * 1024);

        // Comfortably over the 256 KiB cap.
        for _ in 0..40 {
            append_line(&log, &format!("{}\n", chunk));
        }

        let rotated = rotated_path(&log);
        assert!(rotated.is_file(), "crash.log.1 should exist after rotation");
        assert!(log.is_file(), "a fresh crash.log should remain");

        let rotated_len = std::fs::metadata(&rotated).unwrap().len();
        assert!(rotated_len > 0);
        assert!(
            rotated_len <= MAX_BYTES,
            "rotated log should not exceed the cap: {rotated_len}"
        );

        let active_len = std::fs::metadata(&log).unwrap().len();
        assert!(
            active_len < MAX_BYTES,
            "active log should have been reset: {active_len}"
        );

        cleanup(&dir);
    }

    #[test]
    fn rotation_truncates_previous_dot_one() {
        let (dir, log) = temp_log("rotate-old");
        std::fs::write(rotated_path(&log), "stale entry\n").unwrap();
        std::fs::write(&log, "y".repeat(MAX_BYTES as usize)).unwrap();

        append_line(&log, "newest entry\n");

        let rotated = std::fs::read_to_string(rotated_path(&log)).unwrap();
        assert!(!rotated.contains("stale entry"));
        assert!(rotated.contains('y'));

        cleanup(&dir);
    }

    #[test]
    fn tail_returns_last_lines_in_order() {
        let (dir, log) = temp_log("tail");

        for i in 0..5 {
            append_line(&log, &format!("line-{}\n", i));
        }
        append_line(&log, "\n");

        assert_eq!(tail_from(&log, 2), vec!["line-3", "line-4"]);
        assert_eq!(
            tail_from(&log, 10),
            vec!["line-0", "line-1", "line-2", "line-3", "line-4"]
        );
        assert!(tail_from(&log, 0).is_empty());

        cleanup(&dir);
    }

    #[test]
    fn tail_only_reads_the_end_of_large_logs() {
        let (dir, log) = temp_log("tail-large");

        for i in 0..4096 {
            append_line(&log, &format!("entry-{}\n", i));
        }

        let lines = tail_from(&log, 3);
        assert_eq!(lines, vec!["entry-4093", "entry-4094", "entry-4095"]);

        cleanup(&dir);
    }

    #[test]
    fn record_error_uses_expected_format() {
        let (dir, log) = temp_log("record-error");

        record_error_at(&log, "indexer", "disk full");

        let lines = tail_from(&log, 1);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];

        let close = line.find(']').expect("timestamp should be bracketed");
        let secs = line[1..close]
            .parse::<u64>()
            .expect("timestamp should be unix seconds");
        assert!(secs > 0);
        assert!(line.ends_with(" ERROR indexer: disk full"));

        cleanup(&dir);
    }

    #[test]
    fn broken_pipe_panics_are_not_crashes() {
        assert!(super::is_broken_pipe(
            "failed printing to stderr: The pipe is being closed. (os error 232)"
        ));
        assert!(super::is_broken_pipe("failed printing to stdout: Broken pipe (os error 32)"));
        assert!(!super::is_broken_pipe("index out of bounds: the len is 3"));
    }

    #[test]
    fn info_milestones_are_not_failures() {
        let (dir, log) = temp_log("info-classify");

        append_line(&log, "[1793822770] INFO starting kensaku 1.0.15 (windows x86_64)\n");
        append_line(
            &log,
            "[1793822771] INFO config loaded: setup_complete=false app_mode=launcher\n",
        );

        assert_eq!(last_failure_at(&log), None);
        assert!(!is_failure_line(
            "[1793822771] INFO config loaded: setup_complete=false"
        ));

        cleanup(&dir);
    }

    #[test]
    fn last_failure_skips_newer_milestones() {
        let (dir, log) = temp_log("failure-order");

        append_line(&log, "[100] ERROR indexer: disk full\n");
        append_line(&log, "[101] INFO config loaded: setup_complete=true\n");

        assert_eq!(
            last_failure_at(&log).as_deref(),
            Some("[100] ERROR indexer: disk full")
        );

        // A panic entry has no ERROR marker but must still count.
        append_line(&log, "[102] src/indexer/mod.rs:41:9 index out of bounds\n");
        assert_eq!(
            last_failure_at(&log).as_deref(),
            Some("[102] src/indexer/mod.rs:41:9 index out of bounds")
        );

        cleanup(&dir);
    }

    #[test]
    fn missing_log_tails_as_empty() {
        let (dir, log) = temp_log("missing");
        assert!(tail_from(&log, 5).is_empty());
        cleanup(&dir);
    }
}
