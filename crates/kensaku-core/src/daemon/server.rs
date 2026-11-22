//! The daemon: owns the index and serves [`crate::api::dispatch`] over local IPC.

use crate::api::{ApiError, dispatch};
use crate::config::KensakuConfig;
use crate::indexer::IndexerManager;
use crate::runtime::{Runtime, StartError};
use interprocess::local_socket::prelude::*;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Longest request line accepted; larger requests drop the connection.
const MAX_LINE_BYTES: u64 = 1024 * 1024;
/// Concurrent client connections served.
const MAX_CONNECTIONS: usize = 64;
/// Back-off after a failed `accept`, so a persistent error cannot spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);
/// Lets a `shutdown`/`reset` reply reach the client before the process exits.
const EXIT_GRACE: Duration = Duration::from_millis(150);

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Why the daemon stopped before serving.
#[derive(Debug)]
pub enum ServeError {
    /// Another daemon already owns this data directory.
    AlreadyRunning,
    Fatal(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "another daemon is running for this data directory"),
            Self::Fatal(e) => write!(f, "{e}"),
        }
    }
}

/// Runs the daemon in the foreground until `shutdown`/`reset` or a signal.
pub fn run() -> Result<(), ServeError> {
    crate::crashlog::install_panic_hook();
    let config_dir = KensakuConfig::config_dir();
    fs::create_dir_all(&config_dir)
        .map_err(|e| ServeError::Fatal(format!("{}: {e}", config_dir.display())))?;

    // Lock first: a second daemon must exit before touching the token,
    // socket or stores of the running one.
    let runtime = Runtime::start().map_err(|e| match e {
        StartError::AlreadyRunning => ServeError::AlreadyRunning,
        StartError::Io(e) => ServeError::Fatal(e),
    })?;
    let token = ensure_token(&config_dir).map_err(|e| ServeError::Fatal(format!("token: {e}")))?;
    let listener = bind()?;

    let pid_path = config_dir.join("daemon.pid");
    let _ = fs::write(&pid_path, std::process::id().to_string());
    cleanup::remember(&pid_path);
    cleanup::install_signal_handlers();
    crate::say!(
        "[kensaku daemon] pid {} serving {} (protocol {})",
        std::process::id(),
        config_dir.display(),
        super::PROTOCOL_VERSION
    );

    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            std::thread::sleep(ACCEPT_BACKOFF);
            continue;
        };
        if ACTIVE.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            continue; // dropping the stream closes it
        }
        let token = token.clone();
        let indexer = Arc::clone(&runtime.indexer);
        ACTIVE.fetch_add(1, Ordering::Relaxed);
        let spawned = std::thread::Builder::new()
            .name("kensaku-rpc".into())
            .spawn(move || {
                handle_client(stream, &token, &indexer);
                ACTIVE.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            ACTIVE.fetch_sub(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn bind() -> Result<interprocess::local_socket::Listener, ServeError> {
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};
    let name = super::pipe_name()
        .to_ns_name::<GenericNamespaced>()
        .map_err(|e| ServeError::Fatal(format!("pipe name: {e}")))?;
    ListenerOptions::new()
        .name(name)
        .create_sync()
        .map_err(|e| ServeError::Fatal(format!("bind: {e}")))
}

#[cfg(unix)]
fn bind() -> Result<interprocess::local_socket::Listener, ServeError> {
    use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName};
    use std::os::unix::fs::PermissionsExt;
    let sock = super::socket_path();
    // Only reached while holding the writer lock, so any socket file left
    // here belongs to a dead daemon.
    let _ = fs::remove_file(&sock);
    let name = sock
        .clone()
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| ServeError::Fatal(format!("socket name: {e}")))?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_sync()
        .map_err(|e| ServeError::Fatal(format!("bind {}: {e}", sock.display())))?;
    let _ = fs::set_permissions(&sock, fs::Permissions::from_mode(0o600));
    cleanup::remember_socket(&sock);
    Ok(listener)
}

/// Reads or creates the per-data-dir token (32 random bytes, hex).
fn ensure_token(config_dir: &Path) -> std::io::Result<String> {
    let path = config_dir.join("daemon.token");
    if let Ok(existing) = fs::read_to_string(&path) {
        let token = existing.trim();
        if token.len() >= 32 {
            return Ok(token.to_string());
        }
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        // Owner-only from creation, not chmod-after-write.
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&path)?.write_all(token.as_bytes())?;
    Ok(token)
}

fn reply(writer: &mut impl Write, id: &Value, result: Result<Value, ApiError>) {
    let resp = match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e.to_rpc()}),
    };
    let _ = writeln!(writer, "{resp}");
    let _ = writer.flush();
}

/// Reads one newline-terminated line of at most [`MAX_LINE_BYTES`].
/// `None` on EOF, I/O error, an over-long line, or invalid UTF-8.
fn read_line_bounded(reader: &mut impl BufRead) -> Option<String> {
    use std::io::Read as _;
    let mut buf = Vec::new();
    let read = reader
        .by_ref()
        .take(MAX_LINE_BYTES + 1)
        .read_until(b'\n', &mut buf)
        .ok()?;
    if read == 0 || buf.len() as u64 > MAX_LINE_BYTES {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn handle_client(stream: LocalSocketStream, token: &str, indexer: &Arc<IndexerManager>) {
    let mut reader = BufReader::new(&stream);
    let mut writer = &stream;
    while let Some(line) = read_line_bounded(&mut reader) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(trimmed) else {
            let _ = writeln!(
                writer,
                "{}",
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}})
            );
            continue;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let mut params = req.get("params").cloned().unwrap_or_else(|| json!({}));

        let provided = params.get("token").and_then(Value::as_str);
        let version = params.get("protocol_version").and_then(Value::as_u64);
        if let Err((code, message)) = super::authenticate(token, provided, version) {
            let _ = writeln!(
                writer,
                "{}",
                json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message,"data":{"code":"forbidden"}}})
            );
            continue;
        }
        if let Value::Object(map) = &mut params {
            map.remove("token");
            map.remove("protocol_version");
        }

        let result = dispatch(indexer, method, &params);
        let stop = result.is_ok() && matches!(method, "shutdown" | "reset");
        reply(&mut writer, &id, result);
        if stop {
            crate::say!("[kensaku daemon] {method} requested; exiting");
            drop(reader);
            std::thread::sleep(EXIT_GRACE);
            cleanup::cleanup_paths();
            std::process::exit(0);
        }
    }
}

/// Removes the pid file (and socket on Unix) on exit.
mod cleanup {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    static PID: OnceLock<PathBuf> = OnceLock::new();
    #[cfg(unix)]
    static SOCKET_C: OnceLock<std::ffi::CString> = OnceLock::new();
    #[cfg(unix)]
    static PID_C: OnceLock<std::ffi::CString> = OnceLock::new();

    pub fn remember(pid: &Path) {
        let _ = PID.set(pid.to_path_buf());
        #[cfg(unix)]
        if let Ok(c) = std::ffi::CString::new(pid.to_string_lossy().as_bytes()) {
            let _ = PID_C.set(c);
        }
    }

    #[cfg(unix)]
    pub fn remember_socket(socket: &Path) {
        if let Ok(c) = std::ffi::CString::new(socket.to_string_lossy().as_bytes()) {
            let _ = SOCKET_C.set(c);
        }
    }

    pub fn cleanup_paths() {
        if let Some(pid) = PID.get() {
            let _ = std::fs::remove_file(pid);
        }
        #[cfg(unix)]
        if let Some(socket) = SOCKET_C.get() {
            // SAFETY: `socket` is a valid NUL-terminated path owned by a static.
            unsafe { libc::unlink(socket.as_ptr()) };
        }
    }

    #[cfg(unix)]
    extern "C" fn handle_term(_sig: libc::c_int) {
        // Only async-signal-safe calls: `unlink` with precomputed paths.
        // SAFETY: both CStrings are statics initialized before the handler
        // was installed and never mutated afterwards.
        unsafe {
            if let Some(socket) = SOCKET_C.get() {
                libc::unlink(socket.as_ptr());
            }
            if let Some(pid) = PID_C.get() {
                libc::unlink(pid.as_ptr());
            }
            libc::_exit(0);
        }
    }

    #[cfg(unix)]
    pub fn install_signal_handlers() {
        // SAFETY: installing a handler that only performs async-signal-safe calls.
        unsafe {
            libc::signal(libc::SIGTERM, handle_term as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, handle_term as *const () as libc::sighandler_t);
        }
    }

    /// Windows: Ctrl+C/close in a foreground console ends the process and
    /// the OS releases the lock and pipe; the stale pid file is harmless.
    #[cfg(not(unix))]
    pub fn install_signal_handlers() {}
}
