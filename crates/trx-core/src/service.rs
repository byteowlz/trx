//! Service management for trx-api
//!
//! Provides start/stop/status functionality for the trx-api daemon.

use crate::Result;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use sysinfo::{Pid, System};

/// Service manager for trx-api
pub struct ServiceManager {
    state_dir: PathBuf,
}

/// Service status
#[derive(Debug, Clone)]
pub enum ServiceStatus {
    Running { pid: u32, port: Option<u16> },
    Stopped,
    Dead, // PID file exists but process not running
}

impl ServiceManager {
    /// Create a new service manager
    pub fn new() -> Result<Self> {
        let state_dir = get_state_dir()?;
        std::fs::create_dir_all(&state_dir)?;
        Ok(Self { state_dir })
    }

    /// Path to the PID file
    pub fn pid_file(&self) -> PathBuf {
        self.state_dir.join("trx-api.pid")
    }

    /// Path to the port file
    pub fn port_file(&self) -> PathBuf {
        self.state_dir.join("trx-api.port")
    }

    /// Check if the service is running
    pub fn is_running(&self) -> bool {
        if let Ok(pid) = self.read_pid() {
            process_exists(pid)
        } else {
            false
        }
    }

    /// Read the PID from the PID file
    pub fn read_pid(&self) -> Result<u32> {
        let content = std::fs::read_to_string(self.pid_file())?;
        content
            .trim()
            .parse()
            .map_err(|e| crate::Error::Service(format!("Invalid PID: {e}")))
    }

    /// Read the port from the port file
    pub fn read_port(&self) -> Result<u16> {
        let content = std::fs::read_to_string(self.port_file())?;
        content
            .trim()
            .parse()
            .map_err(|e| crate::Error::Service(format!("Invalid port: {e}")))
    }

    /// Start the service
    ///
    /// If `foreground` is true, runs in foreground (blocking).
    /// Otherwise, spawns as a background daemon.
    pub fn start(&self, foreground: bool, workdir: Option<&PathBuf>) -> Result<()> {
        if self.is_running() {
            return Err(crate::Error::Service("Service already running".into()));
        }

        let exe = std::env::current_exe()?;
        let service_exe = exe
            .parent()
            .ok_or_else(|| crate::Error::Service("Cannot find service binary".into()))?
            .join("trx-api");

        if !service_exe.exists() {
            return Err(crate::Error::Service(
                "trx-api binary not found. Please install it first.".into(),
            ));
        }

        let mut cmd = Command::new(&service_exe);

        // Pass workdir if specified
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }

        if foreground {
            let status = cmd.status()?;
            if !status.success() {
                return Err(crate::Error::Service("Service failed to start".into()));
            }
        } else {
            // Start in background
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0) // Create new process group
                    .spawn()?;
            }

            #[cfg(not(unix))]
            {
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;
            }

            // Wait for service to start
            let mut attempts = 0;
            while attempts < 20 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if self.is_running() {
                    break;
                }
                attempts += 1;
            }

            if !self.is_running() {
                return Err(crate::Error::Service("Service failed to start".into()));
            }
        }

        Ok(())
    }

    /// Stop the service
    pub fn stop(&self) -> Result<()> {
        let pid = self.read_pid()?;

        if !process_exists(pid) {
            // Cleanup stale PID file
            std::fs::remove_file(self.pid_file()).ok();
            return Err(crate::Error::Service("Service not running".into()));
        }

        // Send termination signal
        #[cfg(unix)]
        {
            Command::new("kill").arg(pid.to_string()).status()?;
        }

        #[cfg(windows)]
        {
            Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .status()?;
        }

        // Wait for process to exit
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !process_exists(pid) {
                break;
            }
        }

        // Cleanup files
        std::fs::remove_file(self.pid_file()).ok();
        std::fs::remove_file(self.port_file()).ok();

        Ok(())
    }

    /// Restart the service
    pub fn restart(&self, workdir: Option<&PathBuf>) -> Result<()> {
        if self.is_running() {
            self.stop()?;
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        self.start(false, workdir)
    }

    /// Get the service status
    pub fn status(&self) -> ServiceStatus {
        if let Ok(pid) = self.read_pid() {
            if process_exists(pid) {
                let port = self.read_port().ok();
                ServiceStatus::Running { pid, port }
            } else {
                ServiceStatus::Dead
            }
        } else {
            ServiceStatus::Stopped
        }
    }

    /// Write PID file (called by trx-api on startup)
    pub fn write_pid(&self, pid: u32) -> Result<()> {
        std::fs::write(self.pid_file(), pid.to_string())?;
        Ok(())
    }

    /// Write port file (called by trx-api on startup)
    pub fn write_port(&self, port: u16) -> Result<()> {
        std::fs::write(self.port_file(), port.to_string())?;
        Ok(())
    }

    /// Cleanup PID and port files (called by trx-api on shutdown)
    pub fn cleanup(&self) {
        std::fs::remove_file(self.pid_file()).ok();
        std::fs::remove_file(self.port_file()).ok();
    }
}

fn process_exists(pid: u32) -> bool {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    sys.process(Pid::from_u32(pid)).is_some()
}

/// Resolve an XDG-style base directory across platforms (option B, zero-dep).
///
/// An explicit absolute `XDG_*` value always wins on any OS. Otherwise on unix
/// (incl. macOS) the path is `$HOME/<unix_rel>`; on Windows it is the supplied
/// `%APPDATA%`/`%LOCALAPPDATA%` directory.
fn resolve_base(
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
    win_dir: Option<PathBuf>,
    is_windows: bool,
    unix_rel: &str,
) -> Option<PathBuf> {
    if let Some(p) = xdg.filter(|p| p.is_absolute()) {
        return Some(p);
    }
    if is_windows {
        win_dir
    } else {
        home.map(|h| h.join(unix_rel))
    }
}

/// Resolve a base directory from the running environment, then join the app name.
fn base_dir(xdg_var: &str, unix_rel: &str, win_var: &str) -> Result<PathBuf> {
    resolve_base(
        std::env::var_os(xdg_var)
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty()),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os(win_var).map(PathBuf::from),
        cfg!(windows),
        unix_rel,
    )
    .ok_or_else(|| crate::Error::Service(format!("unable to determine base directory ({xdg_var})")))
}

fn get_state_dir() -> Result<PathBuf> {
    // state: ("XDG_STATE_HOME", ".local/state", "LOCALAPPDATA")
    Ok(base_dir("XDG_STATE_HOME", ".local/state", "LOCALAPPDATA")?.join("trx"))
}

#[cfg(test)]
mod tests {
    use super::resolve_base;
    use std::path::PathBuf;

    #[test]
    fn absolute_xdg_wins_on_unix() {
        let got = resolve_base(
            Some(PathBuf::from("/explicit/state")),
            Some(PathBuf::from("/home/u")),
            Some(PathBuf::from("C:\\Users\\u\\AppData\\Local")),
            false,
            ".local/state",
        );
        assert_eq!(got, Some(PathBuf::from("/explicit/state")));
    }

    #[test]
    fn absolute_xdg_wins_on_windows() {
        let got = resolve_base(
            Some(PathBuf::from("/explicit/state")),
            Some(PathBuf::from("/home/u")),
            Some(PathBuf::from("C:\\Users\\u\\AppData\\Local")),
            true,
            ".local/state",
        );
        assert_eq!(got, Some(PathBuf::from("/explicit/state")));
    }

    #[test]
    fn relative_xdg_is_ignored() {
        let got = resolve_base(
            Some(PathBuf::from("relative/state")),
            Some(PathBuf::from("/home/u")),
            None,
            false,
            ".local/state",
        );
        assert_eq!(got, Some(PathBuf::from("/home/u/.local/state")));
    }

    #[test]
    fn unix_falls_back_to_home_rel() {
        let got = resolve_base(
            None,
            Some(PathBuf::from("/home/u")),
            Some(PathBuf::from("C:\\Users\\u\\AppData\\Local")),
            false,
            ".local/state",
        );
        assert_eq!(got, Some(PathBuf::from("/home/u/.local/state")));
    }

    #[test]
    fn windows_falls_back_to_win_dir() {
        let got = resolve_base(
            None,
            Some(PathBuf::from("/home/u")),
            Some(PathBuf::from("C:\\Users\\u\\AppData\\Local")),
            true,
            ".local/state",
        );
        assert_eq!(got, Some(PathBuf::from("C:\\Users\\u\\AppData\\Local")));
    }

    #[test]
    fn none_when_no_source_available() {
        assert_eq!(resolve_base(None, None, None, false, ".local/state"), None);
        assert_eq!(resolve_base(None, None, None, true, ".local/state"), None);
    }
}
