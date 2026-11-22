//! Daemon client: newline-delimited JSON-RPC 2.0 over a Unix socket / Windows named pipe.
//! Requests carry the `<data dir>/daemon.token` token; endpoint names derive from the data dir.

pub mod server;
pub mod service;

use crate::api::{ApiError, ErrorCode};
use crate::config::KensakuConfig;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wire protocol version. v2: the unified method table in [`crate::api`].
pub const PROTOCOL_VERSION: u64 = 2;

const SPAWN_WAIT: Duration = Duration::from_secs(30);
const SPAWN_POLL: Duration = Duration::from_millis(100);
#[cfg(unix)]
const RECV_TIMEOUT: Duration = Duration::from_secs(60);

pub fn endpoint_id(config_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let normalized = config_dir.to_string_lossy().to_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// Socket paths, most specific first: systemd system units may lack `XDG_RUNTIME_DIR`, so try both.
fn socket_candidates(runtime: Option<&Path>, config_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(runtime) = runtime {
        paths.push(runtime.join(format!("kensaku-{}.sock", endpoint_id(config_dir))));
    }
    paths.push(config_dir.join("kensaku.sock"));
    paths
}

pub fn socket_paths() -> Vec<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    socket_candidates(runtime.as_deref(), &KensakuConfig::config_dir())
}

pub fn socket_path() -> PathBuf {
    socket_paths()
        .into_iter()
        .next()
        .unwrap_or_else(|| KensakuConfig::config_dir().join("kensaku.sock"))
}

pub fn pipe_name() -> String {
    format!("kensaku-{}", endpoint_id(&KensakuConfig::config_dir()))
}

pub fn token_path() -> PathBuf {
    KensakuConfig::config_dir().join("daemon.token")
}

pub fn read_token() -> Option<String> {
    std::fs::read_to_string(token_path())
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn unavailable(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::DaemonUnavailable, message)
}

fn connect() -> Result<interprocess::local_socket::Stream, ApiError> {
    use interprocess::local_socket::prelude::*;

    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        let name = pipe_name()
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| unavailable(format!("daemon pipe name: {e}")))?;
        LocalSocketStream::connect(name).map_err(|e| unavailable(format!("daemon unavailable: {e}")))
    }
    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        let mut last_error = String::from("no candidate socket");
        for path in socket_paths() {
            let name = match path.clone().to_fs_name::<GenericFilePath>() {
                Ok(name) => name,
                Err(e) => {
                    last_error = format!("daemon socket name: {e}");
                    continue;
                }
            };
            match LocalSocketStream::connect(name) {
                Ok(stream) => return Ok(stream),
                Err(e) => last_error = format!("daemon unavailable at {}: {e}", path.display()),
            }
        }
        Err(unavailable(last_error))
    }
}

pub fn call(method: &str, params: Value) -> Result<Value, ApiError> {
    #[cfg(unix)]
    use interprocess::local_socket::traits::Stream as _;
    use std::io::{BufRead, BufReader, Write};

    if std::env::var_os("KENSAKU_NO_DAEMON").is_some() {
        return Err(unavailable("daemon disabled by KENSAKU_NO_DAEMON"));
    }
    let token = read_token().ok_or_else(|| unavailable("daemon token missing"))?;
    let stream = connect()?;
    // Windows named pipes do not support I/O timeouts.
    #[cfg(unix)]
    stream
        .set_recv_timeout(Some(RECV_TIMEOUT))
        .map_err(|e| unavailable(e.to_string()))?;
    let mut reader = BufReader::new(&stream);
    let mut writer = &stream;

    let mut params = match params {
        Value::Null => json!({}),
        other => other,
    };
    let Value::Object(map) = &mut params else {
        return Err(ApiError::invalid("params must be a JSON object"));
    };
    map.insert("token".into(), Value::String(token));
    map.insert("protocol_version".into(), json!(PROTOCOL_VERSION));
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    writeln!(writer, "{req}").map_err(|e| unavailable(e.to_string()))?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| unavailable(e.to_string()))?;
    if line.trim().is_empty() {
        // `shutdown`/`reset` end the process; a lost reply still means done.
        if matches!(method, "shutdown" | "reset") {
            return Ok(json!({ "ok": true }));
        }
        return Err(unavailable("daemon closed the connection"));
    }
    let response: Value =
        serde_json::from_str(&line).map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(err) = response.get("error") {
        return Err(ApiError::from_rpc(err));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

/// [`call`] with a `String` error.
pub fn request(method: &str, params: Value) -> Result<Value, String> {
    call(method, params).map_err(|e| e.message)
}

pub fn is_running() -> bool {
    call("ping", json!({})).is_ok()
}

pub fn authenticate(
    expected_token: &str,
    provided_token: Option<&str>,
    protocol_version: Option<u64>,
) -> Result<(), (i32, String)> {
    if provided_token != Some(expected_token) {
        return Err((-32001, "bad token".to_string()));
    }
    if protocol_version != Some(PROTOCOL_VERSION) {
        return Err((
            -32002,
            format!(
                "protocol version mismatch: expected {PROTOCOL_VERSION}, got {protocol_version:?} \
                 (stop the old daemon with `kensaku daemon stop`)"
            ),
        ));
    }
    Ok(())
}

/// Asks the daemon to stop and waits for exit; `Ok(false)` when none was running.
pub fn stop() -> Result<bool, ApiError> {
    if !is_running() {
        return Ok(false);
    }
    call("shutdown", json!({}))?;
    wait_stopped()?;
    Ok(true)
}

/// Waits for the endpoint to disappear so the next command can't reach the exiting process.
pub fn wait_stopped() -> Result<(), ApiError> {
    let deadline = std::time::Instant::now() + SPAWN_WAIT;
    while std::time::Instant::now() < deadline {
        if !is_running() {
            // The endpoint closes slightly before the process releases its files.
            std::thread::sleep(SPAWN_POLL * 3);
            return Ok(());
        }
        std::thread::sleep(SPAWN_POLL);
    }
    Err(ApiError::new(ErrorCode::Busy, "the daemon did not stop in time"))
}

/// Spawns a daemon if none answers; `Ok(true)` when one was spawned.
pub fn ensure_running() -> Result<bool, ApiError> {
    match call("ping", json!({})) {
        Ok(_) => return Ok(false),
        // Bad token / protocol mismatch means one is running; don't spawn another.
        Err(e) if e.code != ErrorCode::DaemonUnavailable => return Err(e),
        Err(_) if std::env::var_os("KENSAKU_NO_DAEMON").is_some() => {
            return Err(unavailable("daemon disabled by KENSAKU_NO_DAEMON"));
        }
        Err(_) => {}
    }
    spawn()?;
    let deadline = std::time::Instant::now() + SPAWN_WAIT;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(SPAWN_POLL);
        if is_running() {
            return Ok(true);
        }
    }
    Err(unavailable(format!(
        "the daemon did not start within {}s; see {}",
        SPAWN_WAIT.as_secs(),
        log_path().display()
    )))
}

pub fn log_path() -> PathBuf {
    KensakuConfig::config_dir().join("daemon.log")
}

/// Spawns `kensaku daemon run` detached from the caller's console/session.
pub fn spawn() -> Result<(), ApiError> {
    let exe = kensaku_binary()
        .ok_or_else(|| unavailable("could not find the `kensaku` binary (set KENSAKU_BIN)"))?;
    let dir = KensakuConfig::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| unavailable(format!("{}: {e}", dir.display())))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
        .map_err(|e| unavailable(format!("daemon log: {e}")))?;
    let log_err = log.try_clone().map_err(|e| unavailable(e.to_string()))?;

    let mut command = std::process::Command::new(exe);
    command
        .args(["daemon", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    detach(&mut command);
    #[cfg(windows)]
    let _guard = std_handles_not_inherited();
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| unavailable(format!("could not start the daemon: {e}")))
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    // No console, and not in the caller's Ctrl+C process group.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Windows spawns with handle inheritance on, so the daemon would keep the caller's
/// stdout/stderr pipes open and `kensaku ... | tail` would never see EOF. Clears the
/// inherit flag on our std handles for the spawn; the guard restores it.
#[cfg(windows)]
fn std_handles_not_inherited() -> impl Drop {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{
        GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };

    struct Restore(Vec<HANDLE>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for &h in &self.0 {
                // SAFETY: h is one of this process's std handles, still open.
                let _ = unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) };
            }
        }
    }

    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    let mut cleared = Vec::new();
    for raw in handles.into_iter().filter(|h| !h.is_null()) {
        let h = HANDLE(raw);
        let mut flags = 0u32;
        // SAFETY: valid std handle; failures leave the handle unchanged.
        let inheritable = unsafe { GetHandleInformation(h, &mut flags) }.is_ok()
            && flags & HANDLE_FLAG_INHERIT.0 != 0;
        if inheritable && unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }.is_ok() {
            cleared.push(h);
        }
    }
    Restore(cleared)
}


#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // Own process group: a terminal Ctrl+C aimed at the CLI must not stop it.
    command.process_group(0);
}

const BIN_NAME: &str = if cfg!(windows) { "kensaku.exe" } else { "kensaku" };

/// Locates `kensaku`: `KENSAKU_BIN`, current exe, sibling of current exe, then `PATH`.
pub fn kensaku_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("KENSAKU_BIN").map(PathBuf::from)
        && explicit.is_absolute()
        && explicit.is_file()
    {
        return Some(explicit);
    }
    let exe = std::env::current_exe().ok();
    let mut candidates = Vec::new();
    if let Some(exe) = &exe {
        if exe.file_name().is_some_and(|n| n == BIN_NAME) {
            return Some(exe.clone());
        }
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(BIN_NAME));
            // Test binaries live in target/<profile>/deps.
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join(BIN_NAME));
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(BIN_NAME)));
    }
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_token() {
        let err = authenticate("good", Some("bad"), Some(PROTOCOL_VERSION)).unwrap_err();
        assert_eq!(err.0, -32001);
    }

    #[test]
    fn rejects_version_mismatch() {
        let err = authenticate("good", Some("good"), Some(999)).unwrap_err();
        assert_eq!(err.0, -32002);
    }

    #[test]
    fn accepts_matching_handshake() {
        assert!(authenticate("good", Some("good"), Some(PROTOCOL_VERSION)).is_ok());
    }

    #[test]
    fn endpoint_id_is_stable_and_differs_per_data_dir() {
        let a = endpoint_id(Path::new("/home/u/.config/kensaku"));
        assert_eq!(a, endpoint_id(Path::new("/home/u/.config/kensaku")));
        assert_ne!(a, endpoint_id(Path::new("/tmp/other")));
        assert_eq!(a.len(), 12);
    }

    #[test]
    fn socket_candidates_prefer_runtime_and_fall_back_to_config_dir() {
        let runtime = PathBuf::from("/run/user/1000");
        let config = PathBuf::from("/home/u/.config/kensaku");
        let id = endpoint_id(&config);
        assert_eq!(
            socket_candidates(Some(&runtime), &config),
            vec![
                runtime.join(format!("kensaku-{id}.sock")),
                config.join("kensaku.sock")
            ]
        );
        assert_eq!(
            socket_candidates(None, &config),
            vec![config.join("kensaku.sock")]
        );
    }
}
