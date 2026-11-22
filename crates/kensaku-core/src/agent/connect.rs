//! Generates and merges Kensaku MCP configs into common clients' config files.
//! Installs merge (never rewrite) and leave a `.bak` backup.

use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
}

impl Client {
    pub const ALL: [Self; 3] = [Self::ClaudeDesktop, Self::ClaudeCode, Self::Cursor];

    pub fn id(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "claude-desktop",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|client| client.id() == id)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub config_path: Option<String>,
    /// Config file exists on disk.
    pub installed: bool,
    /// Config file already wires `kensaku`.
    pub configured: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallOutcome {
    pub status: ClientStatus,
    pub wrote: bool,
    pub backup: Option<String>,
    pub message: String,
}

/// Candidate config paths; the first existing file wins, else the first locatable path.
fn config_paths(client: Client) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    match client {
        Client::ClaudeDesktop => {
            // macOS: ~/Library/Application Support; Linux/Windows: platform config dir.
            #[cfg(target_os = "macos")]
            let base = dirs::data_dir();
            #[cfg(not(target_os = "macos"))]
            let base = dirs::config_dir();
            base.map(|base| vec![base.join("Claude/claude_desktop_config.json")])
                .unwrap_or_default()
        }
        Client::ClaudeCode => vec![home.join(".claude.json")],
        Client::Cursor => vec![home.join(".cursor/mcp.json")],
    }
}

pub fn primary_path(client: Client) -> Option<PathBuf> {
    let paths = config_paths(client);
    paths
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .or_else(|| paths.into_iter().next())
}

/// Kensaku MCP server entry, with the best absolute `kensaku` path found.
pub fn server_entry() -> Value {
    json!({
        "command": cli_command(),
        "args": ["mcp"],
    })
}

pub fn cli_command() -> String {
    crate::daemon::kensaku_binary()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "kensaku".to_string())
}

/// Status for every supported client; never fails.
pub fn detect_clients() -> Vec<ClientStatus> {
    Client::ALL
        .iter()
        .map(|client| {
            let path = primary_path(*client);
            let installed = path.as_ref().map(|path| path.is_file()).unwrap_or(false);
            let configured = installed
                && path
                    .as_ref()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .map(|raw| is_configured(&raw))
                    .unwrap_or(false);
            ClientStatus {
                id: client.id(),
                name: client.name(),
                config_path: path.map(|path| path.to_string_lossy().into_owned()),
                installed,
                configured,
            }
        })
        .collect()
}

fn is_configured(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if has_kensaku(value.get("mcpServers")) {
        return true;
    }
    // Claude Code also stores local-scope servers per project.
    value
        .get("projects")
        .and_then(Value::as_object)
        .map(|projects| {
            projects
                .values()
                .any(|project| has_kensaku(project.get("mcpServers")))
        })
        .unwrap_or(false)
}

fn has_kensaku(servers: Option<&Value>) -> bool {
    servers
        .and_then(Value::as_object)
        .and_then(|servers| servers.get("kensaku"))
        .map(|entry| entry.get("command").is_some())
        .unwrap_or(false)
}

pub fn config_snippet(_client: Client) -> Result<String, String> {
    let pretty = serde_json::to_string_pretty(&json!({
        "mcpServers": {
            "kensaku": server_entry(),
        }
    }))
    .map_err(|e| e.to_string())?;
    Ok(pretty)
}

/// `<path>.bak`, keeping the full file name (`~/.claude.json.bak`).
fn backup_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Temp-file + rename so an interrupted write can't truncate the config. New files 0600; existing keep mode.
fn write_atomic(path: &std::path::Path, contents: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("{} has no parent directory", path.display()));
    };
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("create dir {}: {}", parent.display(), e))?;

    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        ".{}.tmp-{}-{}",
        file_name,
        std::process::id(),
        stamp
    ));

    std::fs::write(&tmp, contents).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode())
            .unwrap_or(0o600);
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replace {}: {}", path.display(), e)
    })
}

/// Merges the Kensaku entry atomically, backing up to `<path>.bak`; unrelated entries are kept.
pub fn install(client: Client) -> Result<InstallOutcome, String> {
    let Some(path) = primary_path(client) else {
        return Err(format!(
            "could not locate the {} config directory on this machine",
            client.name()
        ));
    };

    let previous = if path.is_file() {
        Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {}", path.display(), e))?,
        )
    } else {
        None
    };

    let mut root = match &previous {
        Some(raw) => serde_json::from_str::<Value>(raw)
            .map_err(|e| format!("{} is not valid JSON: {}", path.display(), e))?,
        None => json!({}),
    };
    if !root.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }

    let servers = root
        .as_object_mut()
        .expect("checked object")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        return Err(format!(
            "{} has a non-object \"mcpServers\" key; merge manually",
            path.display()
        ));
    }
    servers
        .as_object_mut()
        .expect("checked object")
        .insert("kensaku".to_string(), server_entry());

    let merged = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;

    let mut backup = None;
    if let Some(previous) = &previous {
        let backup_path = backup_path(&path);
        write_atomic(&backup_path, previous)
            .map_err(|e| format!("write backup {}: {}", backup_path.display(), e))?;
        backup = Some(backup_path.to_string_lossy().into_owned());
    }
    write_atomic(&path, &merged)?;

    let status = ClientStatus {
        id: client.id(),
        name: client.name(),
        config_path: Some(path.to_string_lossy().into_owned()),
        installed: true,
        configured: true,
    };
    Ok(InstallOutcome {
        status,
        wrote: true,
        backup,
        message: format!(
            "Kensaku MCP added to {}. Restart {} to pick it up.",
            path.display(),
            client.name()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_well_formed_and_local() {
        let statuses = detect_clients();
        assert_eq!(statuses.len(), 3);
        for status in &statuses {
            assert!(!status.name.is_empty());
            if status.configured {
                assert!(status.installed, "configured implies installed");
            }
        }
    }

    #[test]
    fn snippet_is_valid_json_with_kensaku_entry() {
        let snippet = config_snippet(Client::Cursor).unwrap();
        let value: Value = serde_json::from_str(&snippet).unwrap();
        assert_eq!(value["mcpServers"]["kensaku"]["args"][0], "mcp");
        assert!(
            !value["mcpServers"]["kensaku"]["command"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn is_configured_matches_only_real_entries() {
        assert!(!is_configured("{}"));
        assert!(!is_configured("not json"));
        assert!(is_configured(
            r#"{"mcpServers": {"kensaku": {"command": "kensaku", "args": ["mcp"]}}}"#
        ));
        assert!(!is_configured(
            r#"{"mcpServers": {"other": {"command": "x"}}}"#
        ));
    }

    #[test]
    fn is_configured_detects_project_scoped_entries() {
        assert!(is_configured(
            r#"{"projects": {"/a": {"mcpServers": {"kensaku": {"command": "kensaku"}}}}}"#
        ));
        assert!(!is_configured(
            r#"{"projects": {"/a": {"mcpServers": {"other": {"command": "x"}}}}}"#
        ));
    }

    #[test]
    fn backup_path_keeps_the_full_file_name() {
        assert_eq!(
            backup_path(std::path::Path::new("/home/u/.claude.json")),
            PathBuf::from("/home/u/.claude.json.bak")
        );
        assert_eq!(
            backup_path(std::path::Path::new("/home/u/.cursor/mcp.json")),
            PathBuf::from("/home/u/.cursor/mcp.json.bak")
        );
    }

    #[test]
    fn atomic_write_replaces_and_creates() {
        let dir = std::env::temp_dir().join(format!(
            "kensaku-connect-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        let path = dir.join("nested/config.json");
        write_atomic(&path, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        write_atomic(&path, "{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":2}");
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("nested"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
