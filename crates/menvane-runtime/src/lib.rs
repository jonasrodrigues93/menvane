use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

pub const DEFAULT_ADDRESS: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 47_831;

pub fn home_from_environment() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("MENVANE_HOME") {
        return Ok(PathBuf::from(home));
    }
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".menvane"))
}

pub fn daemon_running(home: &Path) -> bool {
    let Ok(pid) = fs::read_to_string(home.join("daemon.pid")) else {
        return false;
    };
    Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .is_ok_and(|status| status.success())
}

pub fn start_daemon(home: &Path, executable: &Path) -> Result<u32> {
    if daemon_running(home) {
        anyhow::bail!("Menvane daemon is already running");
    }
    fs::create_dir_all(home.join("logs"))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("logs/daemon.log"))?;
    let child = Command::new(executable)
        .arg("serve")
        .env("MENVANE_HOME", home)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    Ok(child.id())
}

pub fn stop_daemon(home: &Path) -> Result<()> {
    let pid = fs::read_to_string(home.join("daemon.pid")).context("daemon is not running")?;
    let status = Command::new("kill").arg(pid.trim()).status()?;
    if !status.success() {
        anyhow::bail!("failed to stop daemon process {}", pid.trim());
    }
    Ok(())
}
