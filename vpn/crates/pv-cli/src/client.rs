//! Reversible, approval-gated edits to OpenAI-compatible clients.
//!
//! Every edit backs up the original file (or records that it did not exist)
//! before writing, and `rollback` restores it byte-for-byte.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use pv_core::ClientIntegration;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Codex,
    Aider,
    Opencode,
    Env,
}

impl Client {
    pub fn parse(s: &str) -> Option<Client> {
        match s {
            "codex" => Some(Client::Codex),
            "aider" => Some(Client::Aider),
            "opencode" => Some(Client::Opencode),
            "env" | "openai-sdk" => Some(Client::Env),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Client::Codex => "codex",
            Client::Aider => "aider",
            Client::Opencode => "opencode",
            Client::Env => "env",
        }
    }
    pub fn all() -> [Client; 4] {
        [Client::Codex, Client::Aider, Client::Opencode, Client::Env]
    }
    pub fn config_path(self, home: &Path) -> Option<PathBuf> {
        match self {
            Client::Codex => Some(home.join(".codex/config.toml")),
            Client::Aider => Some(home.join(".aider.conf.yml")),
            Client::Opencode => Some(home.join(".config/opencode/opencode.json")),
            Client::Env => None,
        }
    }
    pub fn binary(self) -> &'static str {
        match self {
            Client::Codex => "codex",
            Client::Aider => "aider",
            Client::Opencode => "opencode",
            Client::Env => "",
        }
    }
}

pub fn on_path(bin: &str) -> bool {
    !bin.is_empty() && std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file())).unwrap_or(false)
}

/// Detect installed clients without reading any secret material.
pub fn detect(home: &Path) -> Vec<Value> {
    Client::all()
        .iter()
        .map(|c| {
            let cfg = c.config_path(home);
            json!({
                "client": c.name(),
                "installed": *c == Client::Env || on_path(c.binary()) || cfg.as_ref().is_some_and(|p| p.exists()),
                "config_path": cfg.map(|p| p.display().to_string()),
                "config_exists": c.config_path(home).is_some_and(|p| p.exists()),
            })
        })
        .collect()
}

/// Content to write for each client. Pure function for testing.
pub fn render(client: Client, existing: Option<&str>, base_url: &str, api_key: &str) -> Result<String> {
    match client {
        Client::Codex => {
            let mut doc: toml_edit::DocumentMut = existing.unwrap_or("").parse().context("existing codex config.toml is not valid TOML")?;
            let mut prov = toml_edit::Table::new();
            prov["name"] = toml_edit::value("Prompt VPN (local privacy proxy)");
            prov["base_url"] = toml_edit::value(base_url);
            prov["env_key"] = toml_edit::value("PROMPTVPN_API_KEY");
            prov["wire_api"] = toml_edit::value("responses");
            if !doc.contains_key("model_providers") {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                doc["model_providers"] = toml_edit::Item::Table(t);
            }
            doc["model_providers"]["promptvpn"] = toml_edit::Item::Table(prov);
            doc["model_provider"] = toml_edit::value("promptvpn");
            Ok(doc.to_string())
        }
        Client::Aider => {
            let mut lines: Vec<String> = existing.unwrap_or("").lines().filter(|l| !l.starts_with("openai-api-base:") && !l.starts_with("openai-api-key:")).map(String::from).collect();
            lines.push("# Added by Prompt VPN (rollback: promptvpn client rollback aider)".into());
            lines.push(format!("openai-api-base: {base_url}"));
            lines.push(format!("openai-api-key: {api_key}"));
            Ok(lines.join("\n") + "\n")
        }
        Client::Opencode => {
            let mut v: Value = match existing {
                Some(s) if !s.trim().is_empty() => serde_json::from_str(s).context("existing opencode.json is not valid JSON")?,
                _ => json!({"$schema": "https://opencode.ai/config.json"}),
            };
            if !v["provider"].is_object() {
                v["provider"] = json!({});
            }
            v["provider"]["promptvpn"] = json!({
                "npm": "@ai-sdk/openai-compatible",
                "name": "Prompt VPN (local privacy proxy)",
                "options": {"baseURL": base_url, "apiKey": api_key},
                "models": {"default": {"name": "Prompt VPN default destination"}}
            });
            Ok(serde_json::to_string_pretty(&v)? + "\n")
        }
        Client::Env => bail!("env integration writes no files"),
    }
}

/// Apply with backup. Returns the integration record.
/// `prior_backup`: when the client is already configured (not rolled back), the
/// original backup is kept so repeated runs stay idempotent and rollback still
/// restores the user's true original.
pub fn configure(client: Client, home: &Path, backups: &Path, base_url: &str, api_key: &str, app_id: &str, prior_backup: Option<&str>) -> Result<ClientIntegration> {
    let path = client.config_path(home).context("this client has no config file; use `promptvpn connect show`")?;
    let existing = std::fs::read_to_string(&path).ok();
    let rendered = render(client, existing.as_deref(), base_url, api_key)?;
    std::fs::create_dir_all(backups)?;
    let ts = Utc::now().format("%Y%m%dT%H%M%S");
    let backup = match prior_backup.filter(|b| Path::new(b).exists()) {
        Some(b) => PathBuf::from(b),
        None => {
            let b = backups.join(format!("{}-{ts}.bak", client.name()));
            match &existing {
                Some(s) => std::fs::write(&b, s)?,
                None => std::fs::write(&b, "__PROMPTVPN_FILE_DID_NOT_EXIST__")?,
            }
            b
        }
    };
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("promptvpn.tmp");
    std::fs::write(&tmp, rendered)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(ClientIntegration {
        schema_version: pv_core::CLIENT_INTEGRATION_SCHEMA_VERSION,
        id: format!("{}-{ts}", client.name()),
        client: client.name().into(),
        config_path: path.display().to_string(),
        backup_path: Some(backup.display().to_string()),
        applied_at: Utc::now(),
        base_url: base_url.into(),
        app_id: app_id.into(),
        rolled_back_at: None,
    })
}

/// Restore the backup exactly (or remove the file if it did not exist).
pub fn rollback(rec: &ClientIntegration) -> Result<()> {
    let backup = rec.backup_path.as_deref().context("no backup recorded")?;
    let content = std::fs::read_to_string(backup).with_context(|| format!("backup {backup} missing"))?;
    if content == "__PROMPTVPN_FILE_DID_NOT_EXIST__" {
        let _ = std::fs::remove_file(&rec.config_path);
    } else {
        std::fs::write(&rec.config_path, content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_preserves_existing_and_rolls_back() {
        let home = tempfile::tempdir().unwrap();
        let backups = home.path().join("b");
        let p = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let original = "# my settings\nmodel = \"gpt-5\"\n\n[model_providers.other]\nname = \"x\"\nbase_url = \"https://x\"\n";
        std::fs::write(&p, original).unwrap();
        let rec = configure(Client::Codex, home.path(), &backups, "http://127.0.0.1:8787/v1", "pvk_x", "app_1", None).unwrap();
        // Second run is idempotent and keeps the original backup.
        let rec2 = configure(Client::Codex, home.path(), &backups, "http://127.0.0.1:8787/v1", "pvk_x", "app_1", rec.backup_path.as_deref()).unwrap();
        assert_eq!(rec.backup_path, rec2.backup_path);
        let now = std::fs::read_to_string(&p).unwrap();
        assert_eq!(now.matches("[model_providers.promptvpn]").count(), 1);
        assert!(now.contains("# my settings") && now.contains("[model_providers.other]"));
        assert!(now.contains("[model_providers.promptvpn]") && now.contains("model_provider = \"promptvpn\""));
        assert!(!now.contains("pvk_x"), "codex reads the key from PROMPTVPN_API_KEY");
        rollback(&rec).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), original);
    }

    #[test]
    fn opencode_creates_then_rollback_removes() {
        let home = tempfile::tempdir().unwrap();
        let p = home.path().join(".config/opencode/opencode.json");
        let rec = configure(Client::Opencode, home.path(), &home.path().join("b"), "http://127.0.0.1:8787/v1", "pvk_y", "app", None).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["provider"]["promptvpn"]["options"]["baseURL"], "http://127.0.0.1:8787/v1");
        rollback(&rec).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn aider_replaces_only_its_keys() {
        let out = render(Client::Aider, Some("model: x\nopenai-api-base: https://old\n"), "http://127.0.0.1:8787/v1", "pvk").unwrap();
        assert!(out.contains("model: x") && !out.contains("https://old") && out.contains("openai-api-base: http://127.0.0.1:8787/v1"));
    }

    #[test]
    fn invalid_existing_config_is_not_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let p = home.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        assert!(configure(Client::Opencode, home.path(), &home.path().join("b"), "u", "k", "a", None).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json");
    }
}
