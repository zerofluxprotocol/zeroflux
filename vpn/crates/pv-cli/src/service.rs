//! Optional daemon autostart (launchd user agent / systemd user unit).
//! Installed only after explicit confirmation. Installing the ZFX VPN service
//! removes a Prompt VPN 0.1 service if one exists, so two daemons never race
//! for the same port and data.

use anyhow::{bail, Result};
use std::path::PathBuf;

pub const LABEL: &str = "network.zeroflux.zfx-vpn.daemon";
pub const UNIT: &str = "zfx-vpn.service";
pub const LEGACY_LABEL: &str = "dev.promptvpn.daemon";
pub const LEGACY_UNIT: &str = "promptvpn.service";

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

fn path_for(label: &str, unit: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join(format!("Library/LaunchAgents/{label}.plist"))
    } else {
        home().join(format!(".config/systemd/user/{unit}"))
    }
}

pub fn unit_path() -> PathBuf {
    path_for(LABEL, UNIT)
}

pub fn legacy_unit_path() -> PathBuf {
    path_for(LEGACY_LABEL, LEGACY_UNIT)
}

pub fn render(exe: &str, data_dir: &str, port: u16) -> String {
    if cfg!(target_os = "macos") {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{exe}</string><string>daemon</string><string>--port</string><string>{port}</string></array>
  <key>EnvironmentVariables</key><dict><key>ZFX_VPN_HOME</key><string>{data_dir}</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardErrorPath</key><string>{data_dir}/daemon.log</string>
  <key>StandardOutPath</key><string>{data_dir}/daemon.log</string>
</dict>
</plist>
"#
        )
    } else {
        format!(
            "[Unit]\nDescription=ZFX VPN local prompt privacy gateway\nAfter=network.target\n\n[Service]\nExecStart={exe} daemon --port {port}\nEnvironment=ZFX_VPN_HOME={data_dir}\nRestart=on-failure\nNoNewPrivileges=true\nPrivateTmp=true\n\n[Install]\nWantedBy=default.target\n"
        )
    }
}

fn stop(label: &str, unit: &str) {
    if cfg!(target_os = "macos") {
        let uid = unsafe { libc::getuid() };
        let _ = std::process::Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{label}")]).output();
    } else {
        let _ = std::process::Command::new("systemctl").args(["--user", "disable", "--now", unit]).output();
    }
}

/// Remove a Prompt VPN 0.1 autostart entry. Returns whether one existed.
pub fn remove_legacy() -> Result<bool> {
    let p = legacy_unit_path();
    if !p.exists() {
        return Ok(false);
    }
    stop(LEGACY_LABEL, LEGACY_UNIT);
    std::fs::remove_file(&p)?;
    Ok(true)
}

pub fn install(exe: &str, data_dir: &str, port: u16) -> Result<String> {
    remove_legacy()?;
    let p = unit_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, render(exe, data_dir, port))?;
    if cfg!(target_os = "macos") {
        let uid = unsafe { libc::getuid() };
        let _ = std::process::Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{LABEL}")]).output();
        let o = std::process::Command::new("launchctl").args(["bootstrap", &format!("gui/{uid}"), &p.to_string_lossy()]).output()?;
        if !o.status.success() {
            bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&o.stderr));
        }
    } else {
        let _ = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).output();
        let o = std::process::Command::new("systemctl").args(["--user", "enable", "--now", UNIT]).output()?;
        if !o.status.success() {
            bail!("systemctl failed: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
    Ok(p.display().to_string())
}

pub fn uninstall() -> Result<bool> {
    let legacy = remove_legacy()?;
    let p = unit_path();
    if !p.exists() {
        return Ok(legacy);
    }
    stop(LABEL, UNIT);
    std::fs::remove_file(&p)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unit_contents() {
        let s = super::render("/usr/local/bin/zfx-vpn", "/tmp/zfx", 8787);
        assert!(s.contains("daemon") && s.contains("8787") && s.contains("/tmp/zfx") && s.contains("ZFX_VPN_HOME"));
        assert!(!s.contains("PV_HOME"));
    }
}
