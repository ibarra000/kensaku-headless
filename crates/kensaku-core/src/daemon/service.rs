//! Start-at-login registration: HKCU Run key (Windows), systemd user unit (Linux), LaunchAgent (macOS).
//! Honors the current `KENSAKU_DATA_DIR`.

use std::path::{Path, PathBuf};

pub fn location() -> String {
    imp::location()
}

pub fn install() -> Result<String, String> {
    let exe = super::kensaku_binary().ok_or("could not find the `kensaku` binary")?;
    if cfg!(debug_assertions) {
        crate::say_err!("[kensaku] note: registering a debug build to start at login");
    }
    let data_dir = std::env::var_os("KENSAKU_DATA_DIR").map(PathBuf::from);
    imp::install(&exe, data_dir.as_deref())?;
    Ok(location())
}

/// Removes the registration; a missing one is fine.
pub fn uninstall() -> Result<(), String> {
    imp::uninstall()
}

pub fn is_installed() -> bool {
    imp::is_installed()
}

#[cfg(any(windows, target_os = "linux"))]
fn run(cmd: &mut std::process::Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::process::Command;

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    /// Distinct from the legacy `Kensaku` Run value so the two never collide.
    const VALUE_NAME: &str = "KensakuDaemon";

    pub fn location() -> String {
        format!(r"{RUN_KEY}\{VALUE_NAME}")
    }

    /// `reg.exe` by absolute path, never whatever `reg` is first on PATH.
    fn reg() -> Command {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        Command::new(PathBuf::from(root).join("System32").join("reg.exe"))
    }

    /// Quotes a path for the Run key; a trailing backslash is dropped so it can't escape the quote.
    pub(super) fn quoted(path: &Path) -> String {
        format!("\"{}\"", path.display().to_string().trim_end_matches('\\'))
    }

    pub fn install(exe: &Path, data_dir: Option<&Path>) -> Result<(), String> {
        // `daemon start` (not `run`) avoids a session-long console window; Run has no env, so pass the data dir as a flag.
        let value = match data_dir {
            Some(dir) => format!("{} --data-dir {} daemon start", quoted(exe), quoted(dir)),
            None => format!("{} daemon start", quoted(exe)),
        };
        run(reg()
            .args(["add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d"])
            .arg(value)
            .arg("/f"))
    }

    pub fn uninstall() -> Result<(), String> {
        if !is_installed() {
            return Ok(());
        }
        run(reg().args(["delete", RUN_KEY, "/v", VALUE_NAME, "/f"]))
    }

    pub fn is_installed() -> bool {
        reg()
            .args(["query", RUN_KEY, "/v", VALUE_NAME])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::process::Command;

    fn unit_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("systemd/user/kensaku.service")
    }

    pub fn location() -> String {
        unit_path().display().to_string()
    }

    /// Double-quoted systemd value: escapes `\`, `"`, `%` and, for `ExecStart`, `$`.
    pub(super) fn systemd_quote(value: &str, exec: bool) -> Result<String, String> {
        if value.contains('\n') {
            return Err(format!("path contains a newline: {value:?}"));
        }
        let mut out = value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%");
        if exec {
            out = out.replace('$', "$$");
        }
        Ok(format!("\"{out}\""))
    }

    pub fn install(exe: &Path, data_dir: Option<&Path>) -> Result<(), String> {
        let env = match data_dir {
            Some(dir) => {
                let assignment = format!("KENSAKU_DATA_DIR={}", dir.display());
                format!("Environment={}\n", systemd_quote(&assignment, false)?)
            }
            None => String::new(),
        };
        let exec = systemd_quote(&exe.display().to_string(), true)?;
        let unit = format!(
            "[Unit]\nDescription=Kensaku index daemon\n\n[Service]\n{env}\
             ExecStart={exec} daemon run\nRestart=on-failure\n\n\
             [Install]\nWantedBy=default.target\n"
        );
        let path = unit_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, unit).map_err(|e| format!("{}: {e}", path.display()))?;
        run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
        run(Command::new("systemctl").args(["--user", "enable", "kensaku.service"]))
    }

    pub fn uninstall() -> Result<(), String> {
        let path = unit_path();
        if !path.exists() {
            return Ok(());
        }
        let _ = run(Command::new("systemctl").args(["--user", "disable", "kensaku.service"]));
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        run(Command::new("systemctl").args(["--user", "daemon-reload"]))
    }

    pub fn is_installed() -> bool {
        unit_path().exists()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    const LABEL: &str = "dev.kensaku.daemon";

    fn plist_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_default()
            .join(format!("Library/LaunchAgents/{LABEL}.plist"))
    }

    pub fn location() -> String {
        format!("{} (active from next login)", plist_path().display())
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    pub fn install(exe: &Path, data_dir: Option<&Path>) -> Result<(), String> {
        let env = data_dir
            .map(|dir| {
                format!(
                    "  <key>EnvironmentVariables</key>\n  <dict><key>KENSAKU_DATA_DIR</key><string>{}</string></dict>\n",
                    xml_escape(&dir.display().to_string())
                )
            })
            .unwrap_or_default();
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{LABEL}</string>\n  \
             <key>ProgramArguments</key>\n  <array><string>{}</string><string>daemon</string><string>run</string></array>\n\
             {env}  <key>RunAtLoad</key><true/>\n</dict>\n</plist>\n",
            xml_escape(&exe.display().to_string())
        );
        let path = plist_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, plist).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn uninstall() -> Result<(), String> {
        let path = plist_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn is_installed() -> bool {
        plist_path().exists()
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;

    pub fn location() -> String {
        "(unsupported on this platform)".to_string()
    }

    pub fn install(_exe: &Path, _data_dir: Option<&Path>) -> Result<(), String> {
        Err("start-at-login registration is not supported on this platform".to_string())
    }

    pub fn uninstall() -> Result<(), String> {
        Ok(())
    }

    pub fn is_installed() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn windows_quoting_drops_trailing_backslash() {
        assert_eq!(
            super::imp::quoted(std::path::Path::new(r"C:\data dir\")),
            r#""C:\data dir""#
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_quoting_escapes_specials() {
        let quoted = super::imp::systemd_quote(r#"/a b/%h/$X/"q"\z"#, true).unwrap();
        assert_eq!(quoted, r#""/a b/%%h/$$X/\"q\"\\z""#);
        assert!(super::imp::systemd_quote("a\nb", false).is_err());
    }
}
