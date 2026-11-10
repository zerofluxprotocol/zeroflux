//! Start a local nearcore sandbox node.
//!
//! Binary: `$NEAR_SANDBOX_BIN`, else `~/.local/share/basis-amm/near-sandbox` (installed by
//! basis-amm's `infra/local-dev/install-sandbox.sh`). On macOS, `DYLD_LIBRARY_PATH` is pointed at
//! the self-built liblzma next to it if present.

use crate::Actor;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::{Child, Command};

pub struct Sandbox {
    pub rpc_url: String,
    pub root: Actor,
    child: Child,
    _home: tempfile::TempDir,
}

fn tools() -> PathBuf {
    std::env::var("OBKIT_TOOLS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/basis-amm"))
}

pub fn binary() -> Result<PathBuf> {
    let p = std::env::var("NEAR_SANDBOX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| tools().join("near-sandbox"));
    if !p.exists() {
        bail!("near-sandbox not found at {} (set NEAR_SANDBOX_BIN)", p.display());
    }
    Ok(p)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Sandbox {
    pub async fn start() -> Result<Self> {
        let bin = binary()?;
        let home = tempfile::tempdir()?;
        let xz = tools().join("xz/lib");
        let env = |c: &mut Command| {
            if cfg!(target_os = "macos") && xz.exists() {
                c.env("DYLD_LIBRARY_PATH", &xz);
            }
        };
        let mut init = Command::new(&bin);
        init.args(["--home", home.path().to_str().unwrap(), "init"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        env(&mut init);
        if !init.status().await?.success() {
            bail!("near-sandbox init failed");
        }
        let (port, net) = (free_port(), free_port());
        let mut run = Command::new(&bin);
        run.args([
            "--home",
            home.path().to_str().unwrap(),
            "run",
            "--rpc-addr",
            &format!("127.0.0.1:{port}"),
            "--network-addr",
            &format!("127.0.0.1:{net}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
        env(&mut run);
        let child = run.spawn().context("spawn near-sandbox")?;
        let rpc_url = format!("http://127.0.0.1:{port}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if let Ok(r) = reqwest::get(format!("{rpc_url}/status")).await
                && let Ok(v) = r.json::<serde_json::Value>().await
                && v["sync_info"]["latest_block_height"].as_u64().unwrap_or(0) > 1
            {
                break;
            }
            if std::time::Instant::now() > deadline {
                bail!("sandbox did not start");
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let key: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.path().join("validator_key.json"))?)?;
        let sk = key["secret_key"]
            .as_str()
            .or(key["private_key"].as_str())
            .context("validator key")?;
        let root = Actor::new(key["account_id"].as_str().context("validator account")?, sk)?;
        Ok(Sandbox {
            rpc_url,
            root,
            child,
            _home: home,
        })
    }

    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
    }
}
