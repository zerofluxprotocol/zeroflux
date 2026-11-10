//! `promptvpn init`: the onboarding wizard (guided CLI, or --dashboard).
//! Consent is never assumed: TEE routing requires an explicit answer or flag.

use crate::api::Api;
use crate::{confirm, read_secret, start_daemon};
use anyhow::{bail, Result};
use clap::Args;
use pv_daemon::state::Paths;
use serde_json::{json, Value};
use std::io::{IsTerminal, Write};

#[derive(Args, Debug, Default)]
pub struct InitArgs {
    /// Open the web onboarding wizard instead of the CLI flow.
    #[arg(long)]
    pub dashboard: bool,
    /// Do not prompt; use flags and safe defaults.
    #[arg(long)]
    pub non_interactive: bool,
    /// Noncritical choices, e.g. EMAIL=TOKENIZE,PERSON=ALLOW
    #[arg(long)]
    pub categories: Option<String>,
    /// Uncertain/failure handling: review (block-and-review, default) or tee
    #[arg(long, default_value = "review")]
    pub fallback: String,
    /// Explicit informed consent to send uncertain content to a verified NEAR TEE.
    #[arg(long)]
    pub tee_consent: bool,
    /// Allow a local Ollama generation model to handle content that needs privacy.
    #[arg(long)]
    pub local_private: bool,
    /// Detection model size: edge (~49 MB), small (~86 MB), base (~205 MB), or none.
    #[arg(long)]
    pub model: Option<String>,
    /// Finish local-only (NEAR_NOT_CONFIGURED).
    #[arg(long)]
    pub near_skip: bool,
    #[arg(long)]
    pub near_model: Option<String>,
    /// Read the NEAR API key from stdin.
    #[arg(long)]
    pub near_key_stdin: bool,
    /// Local Ollama generation model (e.g. qwen3.5:4b).
    #[arg(long)]
    pub ollama_model: Option<String>,
    #[arg(long)]
    pub custom_url: Option<String>,
    #[arg(long)]
    pub custom_model: Option<String>,
    /// Acknowledge a custom endpoint is EXTERNAL_UNVERIFIED (required to add one).
    #[arg(long)]
    pub custom_ack: bool,
    #[arg(long)]
    pub custom_failover: bool,
    #[arg(long)]
    pub default_destination: Option<String>,
    #[arg(long)]
    pub no_notifications: bool,
}

fn ask(q: &str, default: &str) -> String {
    print!("{q} [{default}]: ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    let s = s.trim();
    if s.is_empty() {
        default.to_string()
    } else {
        s.to_string()
    }
}

fn h(t: &str) {
    println!("\n\x1b[1m{t}\x1b[0m");
}

pub async fn run(paths: &Paths, port: u16, a: InitArgs) -> Result<()> {
    start_daemon(paths, port).await?;
    let api = Api::new(paths, port)?;
    if a.dashboard {
        let t = api.post("/session/ticket", json!({})).await?;
        let url = format!("{}#/onboarding", t["url"].as_str().unwrap_or_default());
        println!("Open the onboarding wizard:\n  {url}");
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(&url).spawn();
        return Ok(());
    }
    let interactive = !a.non_interactive && std::io::stdin().is_terminal();
    let ob = api.get("/onboarding").await?;

    h("1/6  What Prompt VPN does");
    println!("Prompt VPN runs on this device and exposes an OpenAI-compatible API at http://127.0.0.1:{port}/v1.");
    println!("Point a client's base URL there. Each request is classified locally, then allowed, transformed,");
    println!("held for your review, or blocked before anything leaves this machine. Detection rules, registered");
    println!("secrets and placeholder mappings never leave the device.");
    println!("Limit: only traffic sent to this API is inspected (including tool-call arguments and tool results");
    println!("inside it). Shell, HTTP or browser tools that an agent runs separately are NOT intercepted.");

    h("2/6  Critical protections (on by default)");
    for c in ob["categories"].as_array().into_iter().flatten().filter(|c| c["critical"] == true) {
        println!("  [on] {:<28} e.g. {}", c["label"].as_str().unwrap_or(""), c["examples"][0].as_str().unwrap_or(""));
    }
    println!("These are blocked before any remote destination, including verified TEEs. You can relax one later");
    println!("under Settings/Categories with an explicit confirmation; there is no global off switch.");

    h("3/6  Other personal data");
    let mut cats: serde_json::Map<String, Value> = ob["defaults"].as_object().cloned().unwrap_or_default();
    if let Some(spec) = &a.categories {
        for kv in spec.split(',') {
            if let Some((k, v)) = kv.split_once('=') {
                cats.insert(k.trim().to_ascii_uppercase(), Value::String(v.trim().to_ascii_uppercase()));
            }
        }
    }
    if interactive {
        println!("Choose for each: ALLOW, TOKENIZE (reversible placeholder), REDACT, BLOCK.");
        for (k, v) in cats.clone() {
            let ans = ask(&format!("  {k}"), v.as_str().unwrap_or("TOKENIZE")).to_ascii_uppercase();
            cats.insert(k, Value::String(ans));
        }
    } else {
        for (k, v) in &cats {
            println!("  {k:<20} {}", v.as_str().unwrap_or(""));
        }
    }
    println!("Add exact terms later with `promptvpn secret add` and custom entity types with `promptvpn entity add`.");

    h("4/6  When detection is uncertain or the detection model fails");
    println!("  review: stop locally, notify you, queue for your decision (nothing is sent)  [default]");
    println!("  tee:    send only to a NEAR model whose TEE attestation verifies. This STILL sends data off-device.");
    let mut fallback = a.fallback.clone();
    let mut consent = a.tee_consent;
    if interactive {
        fallback = ask("  choice (review/tee)", &fallback);
        if fallback == "tee" {
            println!("  Informed consent: uncertain prompts will leave this device for a remote enclave operated via NEAR AI.");
            println!("  The enclave is verified by attestation before each use, but a TEE is not on-device processing.");
            consent = ask("  Type 'I consent' to allow this, or press Enter to decline", "") == "I consent";
            if !consent {
                println!("  Not consented: uncertain content will be held for review instead.");
            }
        }
    }
    let local_private = a.local_private || (interactive && confirm("  Allow a local Ollama generation model to handle such content (stays on device)?"));

    h("5/6  Models");
    let options = ob["span_options"].as_array().cloned().unwrap_or_default();
    let default = options.iter().find(|o| o["default"] == true).and_then(|o| o["model"].as_str()).unwrap_or("gliner-pii-base").trim_start_matches("gliner-pii-").to_string();
    println!("  Detection model (runs inside Prompt VPN, no Ollama needed). It finds names, addresses,");
    println!("  health details and your custom entity types that patterns cannot. Apache-2.0.");
    for o in &options {
        println!("    {:<6} {:>4} MB{}", o["model"].as_str().unwrap_or("").trim_start_matches("gliner-pii-"), o["size_bytes"].as_u64().unwrap_or(0) / 1_000_000, if o["default"] == true { "  (recommended)" } else { "" });
    }
    println!("    none   deterministic recognizers only");
    let mut model = a.model.clone().unwrap_or(default);
    if interactive {
        model = ask("  model", &model);
    }
    let ollama_ok = ob["ollama"]["reachable"] == true;
    // NEAR
    println!("\n  NEAR AI (recommended): user-facing inference on NEAR's gateway, with TEE-verified private models.");
    let mut near_skip = a.near_skip;
    let mut near_model = a.near_model.clone();
    let mut near_key: Option<String> = None;
    if interactive && !near_skip {
        near_skip = ask("  Configure NEAR AI now? (yes / skip)", "yes") == "skip";
    }
    if !near_skip {
        let model = near_model.clone().unwrap_or_else(|| pv_provider::near::DEFAULT_NEAR_MODEL.into());
        println!("  Available models: `promptvpn near models`. Only models whose attestation verifies can be used.");
        let model = if interactive { ask("  NEAR model (must verify as Private TEE)", &model) } else { model };
        let v = api.post("/near/verify", json!({"model": model})).await?;
        if v["eligible"] == true {
            println!("  ✓ attestation verified for {model} (gateway TLS-bound, model TDX quote, GPU evidence)");
        } else {
            println!("  ✗ {model} is not TEE-eligible: {}", v["error"].as_str().unwrap_or(""));
        }
        near_model = Some(model);
        near_key = if a.near_key_stdin || interactive { Some(read_secret("  NEAR AI Cloud API key (hidden; empty to skip): ")?) } else { None };
        if near_key.as_deref().unwrap_or("").is_empty() {
            println!("  No key entered: finishing local-only. NEAR_NOT_CONFIGURED until you run `promptvpn near login`.");
            near_skip = true;
        }
    }
    let mut ollama_model = a.ollama_model.clone();
    if interactive && ollama_ok && ollama_model.is_none() {
        let m = ask("  Optional local Ollama generation model (Enter to skip)", "");
        if !m.is_empty() {
            ollama_model = Some(m);
        }
    }
    let custom = match (&a.custom_url, &a.custom_model) {
        (Some(u), Some(m)) => {
            if !a.custom_ack {
                bail!("custom endpoints are EXTERNAL_UNVERIFIED; pass --custom-ack to acknowledge");
            }
            Some(json!({"base_url": u, "model": m, "allow_failover": a.custom_failover, "acknowledged_privacy_tier": true}))
        }
        _ => None,
    };
    if near_skip && ollama_model.is_none() && custom.is_none() {
        println!("  Note: no generation destination configured yet; requests will be refused until you add one.");
    }

    h("6/6  Notifications and dashboard");
    let notifications = !a.no_notifications && (!interactive || ask("  Desktop notifications for blocks, redactions and reviews? (yes/no)", "yes") != "no");

    let payload = json!({
        "categories": cats,
        "fallback_mode": if fallback == "tee" { "TEE_PRIVATE_ROUTE" } else { "BLOCK_AND_REVIEW" },
        "tee_consent": fallback == "tee" && consent,
        "local_private_handling": local_private,
        "span_model": if model == "none" { None } else { Some(model.clone()) },
        "near": {"skip": near_skip, "api_key": near_key, "model": near_model},
        "ollama_generation": ollama_model.map(|m| json!({"model": m})),
        "custom": custom,
        "default_destination": a.default_destination,
        "notifications": notifications,
    });
    let r = api.post("/onboarding/complete", payload).await?;
    if model != "none" {
        match r["span_model"]["error"].as_str() {
            Some(e) => println!("Detection model not installed: {e}. Retry with `promptvpn model install {model}`."),
            None => println!("Detection model {} installed and loaded: {}", r["span_model"]["model"].as_str().unwrap_or(""), r["span_model"]["loaded"]),
        }
    }
    println!("\nDone. Policy {}", r["policy_hash"].as_str().unwrap_or(""));
    println!("OpenAI base URL: {}", r["base_url"].as_str().unwrap_or(""));
    if let Some(k) = r["api_key"].as_str() {
        println!("Local API key (shown once, also saved to {}): {k}", paths.app_key("default").display());
    }
    println!("NEAR: {}", r["near_status"].as_str().unwrap_or(""));
    println!("Next: `promptvpn connect test`, `promptvpn dashboard`, `promptvpn client configure <codex|aider|opencode>`.");
    println!("Re-run `promptvpn init` any time; unrelated data is kept.");
    Ok(())
}
