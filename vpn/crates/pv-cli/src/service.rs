//! Optional daemon autostart (launchd user agent / systemd user unit).
//! Installed only after explicit confirmation.

use anyhow::{bail, Result};
use std::path::PathBuf;

pub const LABEL: &str = "dev.promptvpn.daemon";

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

pub fn unit_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
    } else {
        home().join(".config/systemd/user/promptvpn.service")
    }
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
  <key>EnvironmentVariables</key><dict><key>PV_HOME</key><string>{data_dir}</string></dict>
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
            "[Unit]\nDescription=Prompt VPN local privacy proxy\nAfter=network.target\n\n[Service]\nExecStart={exe} daemon --port {port}\nEnvironment=PV_HOME={data_dir}\nRestart=on-failure\nNoNewPrivileges=true\nPrivateTmp=true\n\n[Install]\nWantedBy=default.target\n"
        )
    }
}

pub fn install(exe: &str, data_dir: &str, port: u16) -> Result<String> {
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
        let o = std::process::Command::new("systemctl").args(["--user", "enable", "--now", "promptvpn.service"]).output()?;
        if !o.status.success() {
            bail!("systemctl failed: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
    Ok(p.display().to_string())
}

pub fn uninstall() -> Result<bool> {
    let p = unit_path();
    if !p.exists() {
        return Ok(false);
    }
    if cfg!(target_os = "macos") {
        let uid = unsafe { libc::getuid() };
        let _ = std::process::Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{LABEL}")]).output();
    } else {
        let _ = std::process::Command::new("systemctl").args(["--user", "disable", "--now", "promptvpn.service"]).output();
    }
    std::fs::remove_file(&p)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unit_contents() {
        let s = super::render("/usr/local/bin/promptvpn", "/tmp/pv", 8787);
        assert!(s.contains("daemon") && s.contains("8787") && s.contains("/tmp/pv"));
    }
}
