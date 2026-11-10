//! `promptvpn`: single binary for the daemon and every management command.

mod api;
mod client;
mod eval;
mod service;
mod wizard;

use anyhow::{anyhow, bail, Context, Result};
use api::Api;
use clap::{Parser, Subcommand};
use pv_daemon::state::Paths;
use serde_json::{json, Value};
use std::io::{IsTerminal, Read, Write};

#[derive(Parser)]
#[command(name = "promptvpn", version, about = "Local privacy proxy for OpenAI-compatible LLM clients")]
struct Cli {
    /// Daemon port (default 8787, or the running daemon's port).
    #[arg(long, global = true)]
    port: Option<u16>,
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon in the foreground (used by `start` and service managers).
    Daemon,
    /// Guided first-run setup (CLI wizard, or --dashboard for the web wizard).
    Init(wizard::InitArgs),
    /// Start the daemon in the background.
    Start,
    /// Stop the background daemon.
    Stop,
    /// Show daemon status.
    Status,
    /// Open the local dashboard (one-time login link).
    Dashboard {
        #[arg(long)]
        no_open: bool,
    },
    /// Diagnostics without secrets.
    Doctor,
    /// Detection model (GLiNER span model, embedded; no Ollama needed).
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// Ollama detection and install guidance (never installs silently).
    Ollama {
        #[command(subcommand)]
        cmd: OllamaCmd,
    },
    /// NEAR AI Gateway.
    Near {
        #[command(subcommand)]
        cmd: NearCmd,
    },
    /// Policies.
    Policy {
        #[command(subcommand)]
        cmd: PolicyCmd,
    },
    /// Registered exact sensitive values (read from stdin or a hidden prompt).
    Secret {
        #[command(subcommand)]
        cmd: SecretCmd,
    },
    /// Custom entity types found by the detection model (e.g. "insurance policy number").
    Entity {
        #[command(subcommand)]
        cmd: EntityCmd,
    },
    /// Audit events (metadata only).
    Events {
        #[command(subcommand)]
        cmd: EventsCmd,
    },
    /// Held requests.
    Approvals {
        #[command(subcommand)]
        cmd: ApprovalCmd,
    },
    /// Client connection details and compatibility test.
    Connect {
        #[command(subcommand)]
        cmd: ConnectCmd,
    },
    /// Generation destinations (NEAR via `near login`).
    Destination {
        #[command(subcommand)]
        cmd: DestCmd,
    },
    /// Reversible edits to OpenAI-compatible client configs.
    Client {
        #[command(subcommand)]
        cmd: ClientCmd,
    },
    /// Vault key rotation and recovery.
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
    /// Daemon autostart (launchd / systemd user service).
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
    /// Emergency stop: block all model traffic (or release with --off).
    EmergencyStop {
        #[arg(long)]
        off: bool,
    },
    /// Run a local OpenAI-compatible mock upstream that records exact bytes.
    MockUpstream {
        #[arg(long, default_value_t = 9797)]
        listen: u16,
        #[arg(long)]
        record: Option<String>,
    },
    /// Benchmark detection configurations on a labeled corpus.
    Eval {
        #[arg(long, default_value = "tests/fixtures/eval_corpus.jsonl")]
        corpus: String,
        /// deterministic, gliner-edge, gliner-small, gliner-base (comma separated) or all
        #[arg(long, default_value = "deterministic")]
        configs: String,
        #[arg(long)]
        out: Option<String>,
        /// Directory containing gliner-pii-<size>/ folders (default: the data dir's models/).
        #[arg(long)]
        models_dir: Option<String>,
    },
    /// Export JSON Schemas for all serialized contracts.
    Schema {
        #[arg(long, default_value = "schemas")]
        out: String,
    },
    /// Remove the service and stop the daemon. Keeps data unless --purge.
    Uninstall {
        #[arg(long)]
        purge: bool,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Show installed sizes and the active model.
    List,
    /// Download (pinned, sha256-verified) and activate a size: edge | small | base.
    Install { size: Option<String> },
    /// Activate an installed size.
    Select { size: String },
    /// Run detection on a synthetic sample (or --text) and show spans.
    Test {
        #[arg(long)]
        text: Option<String>,
    },
}

#[derive(Subcommand)]
enum OllamaCmd {
    Status,
    Setup,
}

#[derive(Subcommand)]
enum NearCmd {
    /// Store a NEAR AI Cloud API key (read without echo or from stdin).
    Login {
        #[arg(long)]
        model: Option<String>,
    },
    /// List NEAR models with privacy tier.
    Models,
    /// Send a synthetic, non-sensitive test request and verify the receipt.
    Test,
    /// Verify TEE attestation for a model (no API key needed).
    VerifyAttestation { model: Option<String> },
    /// Switch the NEAR model (verified first) for normal and private requests.
    Use { model: String },
}

#[derive(Subcommand)]
enum PolicyCmd {
    List,
    Validate { file: String },
    /// Run detection + policy on text (local only).
    Test {
        text: String,
        #[arg(long, default_value = "external_unverified")]
        tier: String,
        #[arg(long)]
        models: bool,
    },
    Import { file: String },
    Export {
        #[arg(long)]
        out: Option<String>,
    },
    /// Set the action for a built-in category (critical ones need --confirm).
    Category {
        category: String,
        action: String,
        #[arg(long)]
        confirm: Option<String>,
    },
}

#[derive(Subcommand)]
enum SecretCmd {
    Add {
        #[arg(long)]
        label: String,
        /// BLOCK (default), TOKENIZE, REDACT, REQUIRE_APPROVAL
        #[arg(long, default_value = "BLOCK")]
        action: String,
        /// value | pattern | file_fingerprint
        #[arg(long, default_value = "value")]
        kind: String,
        /// For file_fingerprint: hash this file.
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        separators: bool,
        #[arg(long)]
        allow: bool,
    },
    List,
    Remove { id: String },
}

#[derive(Subcommand)]
enum EntityCmd {
    /// Add a type of thing to find, e.g. --label "insurance policy number".
    Add {
        #[arg(long)]
        label: String,
        #[arg(long, default_value = "")]
        description: String,
        /// TOKENIZE (default, reversible), REDACT, REQUIRE_APPROVAL, BLOCK
        #[arg(long, default_value = "TOKENIZE")]
        action: String,
    },
    List,
    Remove { id: String },
}

#[derive(Subcommand)]
enum EventsCmd {
    List {
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        severity: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Export {
        #[arg(long)]
        out: Option<String>,
    },
    Verify,
    /// Delete all audit events (explicit).
    Delete {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ApprovalCmd {
    List {
        #[arg(long)]
        all: bool,
    },
    Approve { id: String },
    Deny { id: String },
    Cancel { id: String },
    /// Reveal the held prompt (owner only; audited).
    Reveal { id: String },
    /// Resubmit an edited request body from a JSON file.
    Edit { id: String, file: String },
    Whitelist { id: String },
    Blacklist { id: String },
    /// Save a durable rule from a held request.
    SaveRule {
        id: String,
        /// exact | category | entity
        #[arg(long)]
        kind: String,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        text: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConnectCmd {
    /// Show base URL and how to point clients at it.
    Show,
    /// Create an extra local API key for an app/agent.
    AddApp {
        name: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        destination: Option<String>,
    },
    /// Validate OpenAI compatibility end to end against a local mock upstream.
    Test,
}

#[derive(Subcommand)]
enum DestCmd {
    List,
    /// Add a local Ollama generation model (LOCAL tier).
    AddOllama {
        model: String,
        #[arg(long, default_value = "http://127.0.0.1:11434/v1")]
        url: String,
    },
    /// Add a custom OpenAI-compatible endpoint (EXTERNAL_UNVERIFIED). Key read hidden with --with-key.
    AddCustom {
        id: String,
        url: String,
        model: String,
        #[arg(long)]
        with_key: bool,
        #[arg(long)]
        allow_failover: bool,
        /// Acknowledge this endpoint has no verified confidentiality.
        #[arg(long)]
        ack_external_unverified: bool,
    },
    Remove { id: String },
    /// Make a destination the default for requests.
    Default { id: String },
    /// Connectivity and tier test with a synthetic prompt.
    Test { id: String },
}

#[derive(Subcommand)]
enum ClientCmd {
    Detect,
    /// Back up and edit a client's config (codex | aider | opencode | env).
    Configure {
        client: String,
        #[arg(long)]
        yes: bool,
    },
    Rollback { client: String },
}

#[derive(Subcommand)]
enum VaultCmd {
    Rotate,
    /// Export a passphrase-wrapped recovery bundle.
    RecoveryExport { out: String },
    /// Restore the data key from a recovery bundle (daemon must be stopped).
    Recover { bundle: String },
}

#[derive(Subcommand)]
enum ServiceCmd {
    Install {
        #[arg(long)]
        yes: bool,
    },
    Uninstall,
}

fn print(json_mode: bool, v: &Value, human: impl FnOnce(&Value)) {
    if json_mode {
        println!("{}", serde_json::to_string_pretty(v).unwrap());
    } else {
        human(v);
    }
}

pub fn confirm(prompt: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    matches!(s.trim(), "y" | "Y" | "yes")
}

/// Read a secret without echo (TTY) or from stdin (pipes). Never from argv.
pub fn read_secret(prompt: &str) -> Result<String> {
    if std::io::stdin().is_terminal() {
        Ok(rpassword::prompt_password(prompt)?)
    } else {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s.trim_end_matches(['\n', '\r']).to_string())
    }
}

async fn ensure_running(paths: &Paths, port: u16) -> Result<Api> {
    let api = Api::new(paths, port)?;
    if !api.healthy().await {
        bail!("Prompt VPN daemon is not running on 127.0.0.1:{port}; run `promptvpn start`");
    }
    Ok(api)
}

pub async fn start_daemon(paths: &Paths, port: u16) -> Result<()> {
    std::fs::create_dir_all(&paths.data)?;
    pv_daemon::state::load_or_create_admin_token(paths)?;
    let api = Api::new(paths, port)?;
    if api.healthy().await {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.log())?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon").arg("--port").arg(port.to_string()).env("PV_HOME", &paths.data).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().context("spawn daemon")?;
    pv_daemon::state::write_private(&paths.pid(), child.id().to_string().as_bytes())?;
    for _ in 0..600 {
        if api.healthy().await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!("daemon did not become healthy; see {}", paths.log().display())
}

fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(opener).arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn parse_action(s: &str) -> Result<Value> {
    let a = s.to_ascii_uppercase();
    let ok = ["ALLOW", "TOKENIZE", "REDACT", "GENERALIZE", "PRIVATE_ROUTE", "REQUIRE_APPROVAL", "BLOCK", "FALLBACK"];
    if !ok.contains(&a.as_str()) {
        bail!("action must be one of {}", ok.join(", "));
    }
    Ok(Value::String(a))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let paths = Paths::resolve();
    let port = api::port(&paths, cli.port);
    let j = cli.json;
    match cli.cmd {
        Cmd::Daemon => {
            tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("PV_LOG").unwrap_or_else(|_| "info".into())).with_writer(std::io::stderr).init();
            let st = pv_daemon::state::bootstrap(paths.clone(), None).await?;
            let l = pv_daemon::bind(cli.port.unwrap_or(pv_daemon::state::DEFAULT_PORT)).await?;
            tracing::info!("Prompt VPN {} listening on http://{}", pv_daemon::state::VERSION, l.local_addr()?);
            pv_daemon::serve(st, l).await?;
        }
        Cmd::Init(args) => wizard::run(&paths, port, args).await?,
        Cmd::Start => {
            start_daemon(&paths, port).await?;
            println!("Prompt VPN running at http://127.0.0.1:{port} (OpenAI base URL http://127.0.0.1:{port}/v1)");
        }
        Cmd::Stop => {
            let api = Api::new(&paths, port)?;
            if api.healthy().await {
                let _ = api.post("/shutdown", json!({})).await;
                for _ in 0..50 {
                    if !api.healthy().await {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            let _ = std::fs::remove_file(paths.pid());
            println!("stopped");
        }
        Cmd::Status => {
            let api = Api::new(&paths, port)?;
            if !api.healthy().await {
                print(j, &json!({"running": false}), |_| println!("Prompt VPN is not running (data: {})", paths.data.display()));
                return Ok(());
            }
            let h = api.get("/health").await?;
            print(j, &h, |h| {
                println!("Prompt VPN {}: {}  ({})", h["version"].as_str().unwrap_or(""), h["status"].as_str().unwrap_or(""), h["base_url"].as_str().unwrap_or(""));
                println!("  default destination: {}", h["default_destination"]);
                println!("  NEAR: {}", h["near"]["status"].as_str().unwrap_or(""));
                println!("  detection model: {} loaded={}", h["span"]["model"].as_str().unwrap_or(""), h["span"]["loaded"]);
                println!("  pending approvals: {}", h["pending_approvals"]);
                for w in h["warnings"].as_array().into_iter().flatten() {
                    println!("  ! {}", w.as_str().unwrap_or(""));
                }
            });
        }
        Cmd::Dashboard { no_open } => {
            start_daemon(&paths, port).await?;
            let api = Api::new(&paths, port)?;
            let t = api.post("/session/ticket", json!({})).await?;
            let url = t["url"].as_str().unwrap_or_default().to_string();
            println!("Dashboard login link (single use, expires in 2 minutes):\n  {url}");
            if !no_open {
                open_url(&url);
            }
        }
        Cmd::Doctor => {
            let mut report = json!({"data_dir": paths.data.display().to_string(), "os": std::env::consts::OS, "arch": std::env::consts::ARCH});
            let api = Api::new(&paths, port);
            match api {
                Ok(api) if api.healthy().await => report["health"] = api.get("/health").await?,
                _ => report["health"] = json!("daemon not running"),
            }
            let ol = pv_detectors::ollama::Ollama::new("http://127.0.0.1:11434")?;
            report["ollama"] = match ol.version().await {
                Ok(v) => json!({"installed": true, "version": v}),
                Err(_) => json!({"installed": false, "install": pv_detectors::ollama::install_instructions(pv_daemon::models::os_name())}),
            };
            report["clients"] = json!(client::detect(&home()));
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::Model { cmd } => {
            let api = ensure_running(&paths, port).await?;
            match cmd {
                ModelCmd::List => {
                    let v = api.get("/models").await?;
                    print(j, &v, |v| {
                        println!("Detection model: {}  loaded={}  {}", v["span"]["model"].as_str().unwrap_or(""), v["span"]["loaded"], v["span"]["error"].as_str().unwrap_or(""));
                        for o in v["options"].as_array().into_iter().flatten() {
                            println!("  {:<18} {:>5} MB  installed={}  {}", o["model"].as_str().unwrap_or(""), o["size_bytes"].as_u64().unwrap_or(0) / 1_000_000, o["installed"], if o["default"] == true { "(default)" } else { "" });
                        }
                    });
                }
                ModelCmd::Install { size } => {
                    println!("Downloading pinned GLiNER files from Hugging Face and verifying sha256...");
                    let v = api.post("/models/install", json!({"model": size})).await?;
                    print(j, &v, |v| println!("{} installed; loaded={}", v["model"].as_str().unwrap_or(""), v["loaded"]));
                }
                ModelCmd::Select { size } => {
                    let v = api.post("/models/select", json!({"model": size})).await?;
                    print(j, &v, |v| println!("{} active; loaded={}", v["model"].as_str().unwrap_or(""), v["loaded"]));
                }
                ModelCmd::Test { text } => {
                    let v = api.post("/models/test", json!({"text": text})).await?;
                    print(j, &v, |v| {
                        println!("{} ms (model {} ms)", v["latency_ms"], v["span_latency_ms"]);
                        for f in v["findings"].as_array().into_iter().flatten() {
                            println!("  {:<20} {:.2}  {:?}  [{}]", f["category"].as_str().unwrap_or(""), f["confidence"].as_f64().unwrap_or(0.0), f["text"].as_str().unwrap_or(""), f["recognizer"].as_str().unwrap_or(""));
                        }
                    });
                }
            }
        }
        Cmd::Ollama { cmd } => {
            let ol = pv_detectors::ollama::Ollama::new("http://127.0.0.1:11434")?;
            let os = pv_daemon::models::os_name();
            match (cmd, ol.version().await) {
                (_, Ok(v)) => println!("Ollama {v} is running at http://127.0.0.1:11434"),
                (OllamaCmd::Status, Err(_)) => println!("Ollama is not running or not installed.\n{}", pv_detectors::ollama::install_instructions(os)),
                (OllamaCmd::Setup, Err(_)) => {
                    println!("Prompt VPN does not install system software for you.\n{}", pv_detectors::ollama::install_instructions(os));
                    if confirm("Open the official download page now?") {
                        open_url("https://ollama.com/download");
                    }
                    println!("Waiting for Ollama (Ctrl-C to stop)...");
                    loop {
                        if let Ok(v) = ol.version().await {
                            println!("Ollama {v} detected.");
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    }
                }
            }
        }
        Cmd::Near { cmd } => near(&paths, port, j, cmd).await?,
        Cmd::Policy { cmd } => {
            let api = ensure_running(&paths, port).await?;
            match cmd {
                PolicyCmd::List => {
                    let v = api.get("/policies").await?;
                    print(j, &v, |v| {
                        println!("effective policy {}", v["effective_policy_hash"].as_str().unwrap_or(""));
                        for r in v["rules"].as_array().into_iter().flatten() {
                            println!("  {:<38} v{:<3} {:<17} {:<10} {}", r["id"].as_str().unwrap_or(""), r["version"], r["action"].as_str().unwrap_or(""), r["override"].as_str().unwrap_or(""), r["selector"]["categories"]);
                        }
                    });
                }
                PolicyCmd::Validate { file } => {
                    let v: Value = serde_json::from_str(&std::fs::read_to_string(file)?)?;
                    let r = api.post("/policies/validate", v).await?;
                    println!("{}", serde_json::to_string_pretty(&r)?);
                    if r["valid"] != true {
                        std::process::exit(2);
                    }
                }
                PolicyCmd::Test { text, tier, models } => {
                    let r = api.post("/policies/test", json!({"text": text, "destination_tier": tier.to_ascii_uppercase(), "use_models": models})).await?;
                    print(j, &r, |r| {
                        println!("disposition: {}", r["decision"]["disposition"].as_str().unwrap_or(""));
                        for f in r["findings"].as_array().into_iter().flatten() {
                            println!("  {:<18} {:<12} conf {:.2}  {:?}", f["finding"]["category"].as_str().unwrap_or(""), f["action"].as_str().unwrap_or(""), f["finding"]["confidence"].as_f64().unwrap_or(0.0), f["text"].as_str().unwrap_or(""));
                        }
                    });
                }
                PolicyCmd::Import { file } => {
                    let v: Value = serde_json::from_str(&std::fs::read_to_string(file)?)?;
                    println!("{}", api.post("/policies/import", v).await?);
                }
                PolicyCmd::Export { out } => {
                    let v = api.get("/policies/export").await?;
                    let s = serde_json::to_string_pretty(&v)?;
                    match out {
                        Some(o) => std::fs::write(o, s)?,
                        None => println!("{s}"),
                    }
                }
                PolicyCmd::Category { category, action, confirm } => {
                    let r = api.put(&format!("/categories/{}", category.to_ascii_uppercase()), json!({"action": parse_action(&action)?, "confirm": confirm})).await?;
                    println!("{} → {} (v{})", r["id"].as_str().unwrap_or(""), r["action"].as_str().unwrap_or(""), r["version"]);
                }
            }
        }
        Cmd::Secret { cmd } => {
            let api = ensure_running(&paths, port).await?;
            match cmd {
                SecretCmd::Add { label, action, kind, file, separators, allow } => {
                    let value = match (&kind[..], file) {
                        ("file_fingerprint", Some(f)) => pv_detectors::exact::fingerprint_text(&std::fs::read_to_string(f)?),
                        ("file_fingerprint", None) => bail!("--file is required for file_fingerprint"),
                        _ => read_secret("Value (input hidden): ")?,
                    };
                    if value.is_empty() {
                        bail!("empty value");
                    }
                    let r = api.post("/secrets", json!({"label": label, "value": value, "kind": kind, "action": parse_action(&action)?, "allow": allow, "norm": {"unicode": true, "casefold": true, "whitespace": true, "separators": separators}})).await?;
                    println!("registered {} (the value is stored encrypted; it is never shown again)", r["secret_id"].as_str().unwrap_or(""));
                }
                SecretCmd::List => {
                    let v = api.get("/secrets").await?;
                    print(j, &v, |v| {
                        for s in v["secrets"].as_array().into_iter().flatten() {
                            println!("  {}  {:<16} {:<8} allow={} {}", s["secret_id"].as_str().unwrap_or(""), s["kind"].as_str().unwrap_or(""), s["action"].as_str().unwrap_or(""), s["allow"], s["label"].as_str().unwrap_or(""));
                        }
                    });
                }
                SecretCmd::Remove { id } => {
                    api.delete(&format!("/secrets/{id}")).await?;
                    println!("removed {id}");
                }
            }
        }
        Cmd::Entity { cmd } => {
            let api = ensure_running(&paths, port).await?;
            match cmd {
                EntityCmd::Add { label, description, action } => {
                    let r = api.post("/entities", json!({"label": label, "description": description, "action": parse_action(&action)?})).await?;
                    println!("added entity type {}", r["entity_id"].as_str().unwrap_or(""));
                    if r["span_model_loaded"] != true {
                        println!("note: the detection model is not loaded, so requests will be held for review until you run `promptvpn model install`");
                    }
                }
                EntityCmd::List => {
                    let v = api.get("/entities").await?;
                    print(j, &v, |v| {
                        for e in v["entities"].as_array().into_iter().flatten() {
                            println!("  {}  {:<16} {}", e["entity"]["id"].as_str().unwrap_or(""), e["action"].as_str().unwrap_or(""), e["entity"]["label"].as_str().unwrap_or(""));
                        }
                    });
                }
                EntityCmd::Remove { id } => {
                    api.delete(&format!("/entities/{id}")).await?;
                    println!("removed {id}");
                }
            }
        }
        Cmd::Events { cmd } => {
            let api = ensure_running(&paths, port).await?;
            match cmd {
                EventsCmd::List { action, severity, limit } => {
                    let mut q = format!("/events?limit={limit}");
                    if let Some(a) = action {
                        q += &format!("&action={a}");
                    }
                    if let Some(s) = severity {
                        q += &format!("&severity={s}");
                    }
                    let v = api.get(&q).await?;
                    print(j, &v, |v| {
                        for e in v["events"].as_array().into_iter().flatten() {
                            println!("{}  {:<17} {:<9} {}", e["timestamp"].as_str().unwrap_or("").get(..19).unwrap_or(""), e["action"].as_str().unwrap_or(""), e["severity"].as_str().unwrap_or(""), e["summary"].as_str().unwrap_or(""));
                        }
                    });
                }
                EventsCmd::Export { out } => {
                    let v = api.get("/events/export").await?;
                    let s = v.as_str().unwrap_or_default().to_string();
                    match out {
                        Some(o) => {
                            std::fs::write(&o, s)?;
                            println!("wrote {o}");
                        }
                        None => print!("{s}"),
                    }
                }
                EventsCmd::Verify => {
                    let v = api.post("/events/verify", json!({})).await?;
                    println!("{}", serde_json::to_string_pretty(&v)?);
                    if v["ok"] != true {
                        std::process::exit(2);
                    }
                }
                EventsCmd::Delete { yes } => {
                    if !yes && !confirm("Delete ALL audit events?") {
                        bail!("aborted");
                    }
                    println!("{}", api.delete("/events").await?);
                }
            }
        }
        Cmd::Approvals { cmd } => approvals(&paths, port, j, cmd).await?,
        Cmd::Connect { cmd } => connect(&paths, port, cmd).await?,
        Cmd::Client { cmd } => clients(&paths, port, cmd).await?,
        Cmd::Destination { cmd } => {
            let api = ensure_running(&paths, port).await?;
            let base = |id: &str, kind: &str, url: &str, model: &str, tier: &str, fo: bool| json!({"schema_version": 1, "id": id, "kind": kind, "display_name": id, "base_url": url, "default_model": model, "declared_tier": tier, "enabled": true, "allow_failover": fo, "supports_responses": false, "timeout_secs": 300});
            match cmd {
                DestCmd::List => {
                    let v = api.get("/providers").await?;
                    let s = api.get("/settings").await?;
                    print(j, &v, |v| {
                        println!("default: {}  failover: {}", s["engine"]["default_destination"], s["engine"]["failover"]);
                        for p in v["providers"].as_array().into_iter().flatten() {
                            let c = &p["config"];
                            println!("  {:<10} {:<18} {:<20} {:<40} model={} failover={} credential={}", c["id"].as_str().unwrap_or(""), c["kind"].as_str().unwrap_or(""), c["declared_tier"].as_str().unwrap_or(""), c["base_url"].as_str().unwrap_or(""), c["default_model"].as_str().unwrap_or(""), c["allow_failover"], p["has_credential"]);
                        }
                    });
                }
                DestCmd::AddOllama { model, url } => {
                    api.post("/providers", base("ollama", "OLLAMA", &url, &model, "LOCAL", true)).await?;
                    println!("added local destination 'ollama' ({model})");
                }
                DestCmd::AddCustom { id, url, model, with_key, allow_failover, ack_external_unverified } => {
                    if !ack_external_unverified {
                        bail!("custom endpoints are EXTERNAL_UNVERIFIED (no verified confidentiality); pass --ack-external-unverified");
                    }
                    let mut b = base(&id, "OPENAI_COMPATIBLE", &url, &model, "EXTERNAL_UNVERIFIED", allow_failover);
                    if with_key {
                        b["api_key"] = json!(read_secret("Endpoint API key (hidden): ")?);
                    }
                    api.post("/providers", b).await?;
                    println!("added external destination '{id}'");
                }
                DestCmd::Remove { id } => {
                    api.delete(&format!("/providers/{id}")).await?;
                    println!("removed {id}");
                }
                DestCmd::Default { id } => {
                    api.put("/settings", json!({"engine": {"default_destination": id}})).await?;
                    println!("default destination: {id}");
                }
                DestCmd::Test { id } => println!("{}", serde_json::to_string_pretty(&api.post(&format!("/providers/{id}/test"), json!({})).await?)?),
            }
        }
        Cmd::Vault { cmd } => match cmd {
            VaultCmd::Rotate => {
                let api = ensure_running(&paths, port).await?;
                println!("active key: {}", api.post("/vault/rotate", json!({})).await?["active_key"]);
            }
            VaultCmd::RecoveryExport { out } => {
                let api = ensure_running(&paths, port).await?;
                let p1 = read_secret("Recovery passphrase (min 12 chars): ")?;
                if std::io::stdin().is_terminal() && read_secret("Repeat: ")? != p1 {
                    bail!("passphrases differ");
                }
                let v = api.post("/vault/recovery", json!({"passphrase": p1})).await?;
                pv_daemon::state::write_private(std::path::Path::new(&out), serde_json::to_string_pretty(&v["bundle"])?.as_bytes())?;
                println!("wrote recovery bundle {out}; store it offline. It decrypts only with your passphrase.");
            }
            VaultCmd::Recover { bundle } => {
                if let Ok(a) = Api::new(&paths, port) {
                    if a.healthy().await {
                        bail!("stop the daemon first (`promptvpn stop`)");
                    }
                }
                let b = std::fs::read_to_string(bundle)?;
                let pass = read_secret("Recovery passphrase: ")?;
                let backend = pv_daemon::state::key_backend(&paths)?;
                pv_vault::Vault::recover(backend.as_ref(), &b, &pass).map_err(|e| anyhow!("{e}"))?;
                println!("data key restored into {}", backend.name());
            }
        },
        Cmd::Service { cmd } => match cmd {
            ServiceCmd::Install { yes } => {
                if !yes && !confirm(&format!("Install a user-level autostart service at {}?", service::unit_path().display())) {
                    bail!("aborted; nothing installed");
                }
                let exe = std::env::current_exe()?.display().to_string();
                println!("installed {}", service::install(&exe, &paths.data.display().to_string(), port)?);
            }
            ServiceCmd::Uninstall => println!("{}", if service::uninstall()? { "service removed" } else { "no service installed" }),
        },
        Cmd::EmergencyStop { off } => {
            let api = ensure_running(&paths, port).await?;
            let v = api.post("/emergency-stop", json!({"engaged": !off})).await?;
            println!("{}", if v["emergency_stop"] == true { "EMERGENCY STOP ENGAGED: all model traffic is blocked locally" } else { "emergency stop released" });
        }
        Cmd::MockUpstream { listen, record } => {
            let rec = std::sync::Arc::new(pv_daemon::mock::Recorded { file: record.map(Into::into), ..Default::default() });
            let (url, h) = pv_daemon::mock::serve(listen, rec).await?;
            println!("mock upstream at {url} (records exact request bytes)");
            h.await?;
        }
        Cmd::Eval { corpus, configs, out, models_dir } => eval_cmd(&paths, &corpus, &configs, out, models_dir).await?,
        Cmd::Schema { out } => {
            std::fs::create_dir_all(&out)?;
            for (n, s) in pv_core::schema::all_schemas() {
                std::fs::write(std::path::Path::new(&out).join(n), s + "\n")?;
            }
            println!("wrote schemas to {out}");
        }
        Cmd::Uninstall { purge, yes } => {
            let api = Api::new(&paths, port);
            if let Ok(api) = api {
                if api.healthy().await {
                    let _ = api.post("/shutdown", json!({})).await;
                }
            }
            service::uninstall()?;
            if purge {
                println!("This permanently deletes {} (policies, vault, audit log) and the keychain entry.", paths.data.display());
                let ok = yes || {
                    print!("Type DELETE to confirm: ");
                    let _ = std::io::stdout().flush();
                    let mut s = String::new();
                    let _ = std::io::stdin().read_line(&mut s);
                    s.trim() == "DELETE"
                };
                if !ok {
                    bail!("aborted; data preserved");
                }
                if let Ok(b) = pv_daemon::state::key_backend(&paths) {
                    let _ = b.delete();
                }
                std::fs::remove_dir_all(&paths.data).ok();
                println!("removed all Prompt VPN data");
            } else {
                println!("Service removed and daemon stopped. Data preserved at {} (use --purge to delete).", paths.data.display());
            }
            println!("Client configs are untouched; restore them with `promptvpn client rollback <client>` before uninstalling if you edited any.");
        }
    }
    Ok(())
}

pub fn home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

async fn near(paths: &Paths, port: u16, j: bool, cmd: NearCmd) -> Result<()> {
    match cmd {
        NearCmd::Models => {
            let api = Api::new(paths, port).ok();
            let up = match &api { Some(a) => a.healthy().await, None => false };
            let v = if up {
                api.unwrap().get("/near/models").await?
            } else {
                use pv_provider_shim::list;
                list().await?
            };
            print(j, &v, |v| {
                println!("{:<52} {:<15} TEE-eligible candidate", "model", "privacy tier");
                for m in v["models"].as_array().into_iter().flatten() {
                    let cand = m["privacy_tier"] == "PRIVATE_TEE";
                    println!("{:<52} {:<15} {}", m["id"].as_str().unwrap_or(""), m["privacy_tier"].as_str().unwrap_or(""), if cand { "yes (verify before use)" } else { "no" });
                }
                println!("\nNote: the spec's preferred `deepseek-ai/DeepSeek-V4-Flash` is not a catalog model on the live gateway (it is an alias). Use `promptvpn near verify-attestation <model>`.");
            });
        }
        NearCmd::VerifyAttestation { model } => {
            let model = model.unwrap_or_else(|| pv_provider::near::DEFAULT_NEAR_MODEL.into());
            let api = Api::new(paths, port).ok();
            let up = match &api { Some(a) => a.healthy().await, None => false };
            let v = if up {
                api.unwrap().post("/near/verify", json!({"model": model})).await?
            } else {
                pv_provider_shim::verify(&model).await?
            };
            print(j, &v, |v| {
                if v["eligible"] == true {
                    println!("VERIFIED: {} is eligible for PRIVATE_ROUTE", v["model"].as_str().unwrap_or(""));
                    println!("  gateway signer {} (TCB {}), TLS SPKI {}", v["gateway_signer"].as_str().unwrap_or(""), v["gateway_tcb"].as_str().unwrap_or(""), v["gateway_tls_spki"].as_str().unwrap_or(""));
                    let join = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                    println!("  model signer {} (TCB {}), GPU evidence verified, E2EE key present", join("model_signers"), join("model_tcb"));
                    println!("  verifier: {}", v["verifier"].as_str().unwrap_or(""));
                } else {
                    println!("NOT ELIGIBLE: {}: {}", v["model"].as_str().unwrap_or(""), v["error"].as_str().unwrap_or(""));
                }
            });
            if v["eligible"] != true {
                std::process::exit(2);
            }
        }
        NearCmd::Login { model } => {
            let api = ensure_running(paths, port).await?;
            let key = read_secret("NEAR AI Cloud API key (input hidden): ")?;
            if key.is_empty() {
                bail!("empty key");
            }
            let model = model.unwrap_or_else(|| pv_provider::near::DEFAULT_NEAR_MODEL.into());
            let ver = api.post("/near/verify", json!({"model": model})).await?;
            if ver["eligible"] != true {
                println!("warning: {model} is not TEE-eligible: {}", ver["error"].as_str().unwrap_or(""));
            }
            let cfg = json!({"schema_version": 1, "id": "near", "kind": "NEAR_AI", "display_name": "NEAR AI", "base_url": "https://cloud-api.near.ai/v1", "default_model": model, "declared_tier": "ATTESTED_TEE", "credential_ref": "provider:near", "enabled": true, "allow_failover": true, "supports_responses": true, "timeout_secs": 180, "api_key": key});
            api.post("/providers", cfg).await?;
            println!("NEAR AI configured (key stored in the OS credential store). Default model {model}.");
            println!("Make it the default destination: promptvpn init (or Settings in the dashboard).");
        }
        NearCmd::Use { model } => {
            let api = ensure_running(paths, port).await?;
            let v = api.post("/near/verify", json!({"model": model})).await?;
            if v["eligible"] != true {
                bail!("{model} is not TEE-eligible: {}", v["error"].as_str().unwrap_or(""));
            }
            let p = api.get("/providers").await?;
            let mut cfg = p["providers"].as_array().into_iter().flatten().find(|x| x["config"]["id"] == "near").map(|x| x["config"].clone()).context("NEAR is not configured; run `promptvpn near login` first")?;
            cfg["default_model"] = json!(model);
            api.post("/providers", cfg).await?;
            let s = api.get("/settings").await?;
            if s["engine"]["private_destination"] == "near" {
                api.put("/settings", json!({"engine": {"private_model": model}})).await?;
            }
            println!("NEAR model set to {model} (attestation verified)");
        }
        NearCmd::Test => {
            let api = ensure_running(paths, port).await?;
            let v = api.post("/providers/near/test", json!({})).await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            if v["ok"] != true {
                std::process::exit(2);
            }
        }
    }
    Ok(())
}

/// NEAR helpers that work without a running daemon (public endpoints only).
mod pv_provider_shim {
    use anyhow::{anyhow, Result};
    use serde_json::{json, Value};
    pub async fn list() -> Result<Value> {
        use pv_provider::near::NearBackend;
        let b = pv_provider::near::NearSdkBackend::new(pv_provider::near::DEFAULT_NEAR_BASE, None).map_err(|e| anyhow!("{e}"))?;
        Ok(json!({"models": b.list_models().await.map_err(|e| anyhow!("{e}"))?}))
    }
    pub async fn verify(model: &str) -> Result<Value> {
        use pv_provider::near::NearBackend;
        let b = pv_provider::near::NearSdkBackend::new(pv_provider::near::DEFAULT_NEAR_BASE, None).map_err(|e| anyhow!("{e}"))?;
        Ok(json!(b.verify(model).await))
    }
}

async fn approvals(paths: &Paths, port: u16, j: bool, cmd: ApprovalCmd) -> Result<()> {
    let api = ensure_running(paths, port).await?;
    let act = |id: &str, a: &str| format!("/approvals/{id}/{a}");
    let v = match cmd {
        ApprovalCmd::List { all } => {
            let v = api.get(&format!("/approvals?all={all}")).await?;
            print(j, &v, |v| {
                for a in v["approvals"].as_array().into_iter().flatten() {
                    println!("{}  {:<9} {:?}  expires {}\n    {}", a["approval_id"].as_str().unwrap_or(""), a["status"].as_str().unwrap_or(""), a["categories"], a["expires_at"].as_str().unwrap_or("").get(..19).unwrap_or(""), a["redacted_preview"].as_str().unwrap_or("").chars().take(160).collect::<String>().replace('\n', " "));
                }
            });
            return Ok(());
        }
        ApprovalCmd::Approve { id } => api.post(&act(&id, "approve"), json!({})).await?,
        ApprovalCmd::Deny { id } => api.post(&act(&id, "deny"), json!({})).await?,
        ApprovalCmd::Cancel { id } => api.post(&act(&id, "cancel"), json!({})).await?,
        ApprovalCmd::Whitelist { id } => api.post(&act(&id, "whitelist"), json!({})).await?,
        ApprovalCmd::Blacklist { id } => api.post(&act(&id, "blacklist"), json!({})).await?,
        ApprovalCmd::Reveal { id } => {
            if !confirm("Reveal the full held prompt on this terminal? (audited)") {
                bail!("aborted");
            }
            api.post(&act(&id, "reveal"), json!({"confirm": true})).await?
        }
        ApprovalCmd::Edit { id, file } => {
            let body: Value = serde_json::from_str(&std::fs::read_to_string(file)?)?;
            api.post(&act(&id, "resubmit"), json!({"body": body})).await?
        }
        ApprovalCmd::SaveRule { id, kind, category, action, name, text } => {
            let a = api.get(&format!("/approvals/{id}")).await?;
            match kind.as_str() {
                "category" => {
                    let c = category.or_else(|| a["approval"]["categories"][0].as_str().map(String::from)).context("--category required")?;
                    api.put(&format!("/categories/{c}"), json!({"action": parse_action(&action.unwrap_or_else(|| "BLOCK".into()))?})).await?
                }
                "exact" => {
                    let value = read_secret("Value to register (input hidden): ")?;
                    api.post("/secrets", json!({"label": name.unwrap_or_else(|| format!("from review {id}")), "value": value, "action": parse_action(&action.unwrap_or_else(|| "BLOCK".into()))?})).await?
                }
                "entity" => api.post("/entities", json!({"label": name.or(text).context("--name (the entity label) required")?, "action": parse_action(&action.unwrap_or_else(|| "TOKENIZE".into()))?})).await?,
                _ => bail!("--kind must be exact, category or entity"),
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

async fn connect(paths: &Paths, port: u16, cmd: ConnectCmd) -> Result<()> {
    match cmd {
        ConnectCmd::Show => {
            let key = std::fs::read_to_string(paths.app_key("default")).ok();
            println!("OpenAI-compatible base URL: http://127.0.0.1:{port}/v1");
            match key {
                Some(_) => println!("Local API key: stored at {} (pass it as the client's OpenAI API key)", paths.app_key("default").display()),
                None => println!("Local API key: none yet; run `promptvpn init` or `promptvpn connect add-app <name>`"),
            }
            println!("\nExamples:\n  export OPENAI_BASE_URL=http://127.0.0.1:{port}/v1\n  export OPENAI_API_KEY=\"$(cat '{}')\"\n  # Python: OpenAI(base_url=\"http://127.0.0.1:{port}/v1\", api_key=...)", paths.app_key("default").display());
            println!("\nCoverage: only requests sent to this API are inspected (including tool-call arguments and tool results inside them). Separately executed shell/HTTP/browser tool calls are not intercepted.");
        }
        ConnectCmd::AddApp { name, agent, destination } => {
            let api = ensure_running(paths, port).await?;
            let v = api.post("/apps", json!({"name": name, "agent_id": agent, "destination_id": destination})).await?;
            println!("app {}  key (shown once): {}", v["app"]["app_id"].as_str().unwrap_or(""), v["api_key"].as_str().unwrap_or(""));
        }
        ConnectCmd::Test => {
            let report = connect_test().await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report["passed"] != true {
                std::process::exit(2);
            }
        }
    }
    Ok(())
}

/// Self-contained compatibility + egress test: temporary data dir, in-memory
/// keys, local mock upstream; synthetic secrets only.
async fn connect_test() -> Result<Value> {
    std::env::set_var("PV_KEY_BACKEND", "memory");
    let dir = std::env::temp_dir().join(format!("promptvpn-connect-test-{}", uuid::Uuid::new_v4().simple()));
    let rec = std::sync::Arc::new(pv_daemon::mock::Recorded::default());
    let (mock, _) = pv_daemon::mock::serve(0, rec.clone()).await?;
    let st = pv_daemon::state::bootstrap(Paths { data: dir.clone() }, Some(std::sync::Arc::new(pv_proxy::notify::Recorder::default()))).await?;
    st.engine.detector.set_settings(pv_detectors::DetectorSettings { span_enabled: false, span_required: false, span_timeout_ms: 1000 });
    let cfg = pv_core::ProviderConfig { schema_version: 1, id: "mock".into(), kind: pv_core::ProviderKind::OpenaiCompatible, display_name: "mock".into(), base_url: mock.clone(), default_model: "mock-echo".into(), declared_tier: pv_core::DestinationTier::ExternalUnverified, credential_ref: None, enabled: true, allow_failover: false, supports_responses: true, timeout_secs: 10 };
    pv_daemon::admin::put_provider_inner(&st, cfg, None).map_err(|_| anyhow!("provider"))?;
    let mut s = st.engine.settings();
    s.default_destination = Some("mock".into());
    pv_daemon::state::save_settings(&st, &s, "test")?;
    let (_, key) = pv_daemon::state::create_app(&st, "connect-test", None, None)?;
    let l = pv_daemon::bind(0).await?;
    let base = format!("http://{}", l.local_addr()?);
    tokio::spawn(pv_daemon::serve(st.clone(), l));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let http = reqwest::Client::builder().no_proxy().build()?;
    let mut checks = vec![];
    let mut passed = true;
    let mut check = |name: &str, ok: bool, detail: String| {
        passed &= ok;
        checks.push(json!({"check": name, "ok": ok, "detail": detail}));
    };
    let synthetic_ssn = "123-45-6789";
    let synthetic_email = "synthetic.user@example.com";
    let synthetic_key = "sk-proj-SYNTHETICabcdefghijklmnop123456";
    let post = |path: &str, body: Value| http.post(format!("{base}{path}")).bearer_auth(&key).json(&body).send();
    let r = post("/v1/chat/completions", json!({"model": "x", "messages": [{"role": "user", "content": format!("ssn {synthetic_ssn}")}]})).await?;
    check("SSN blocked before egress", r.status() == 403 && !rec.all_text().contains(synthetic_ssn), format!("HTTP {}", r.status()));
    let r = post("/v1/chat/completions", json!({"model": "x", "messages": [{"role": "user", "content": format!("key {synthetic_key}")}]})).await?;
    check("API key blocked before egress", r.status() == 403 && !rec.all_text().contains(synthetic_key), format!("HTTP {}", r.status()));
    let r = post("/v1/chat/completions", json!({"model": "x", "messages": [{"role": "user", "content": format!("mail {synthetic_email}")}]})).await?;
    let v: Value = r.json().await?;
    check("email tokenized upstream", !rec.all_text().contains(synthetic_email) && rec.all_text().contains("[[PV_EMAIL_"), "chat completions".into());
    check("email restored for caller", v["choices"][0]["message"]["content"].as_str().unwrap_or("").contains(synthetic_email), "chat completions".into());
    let r = post("/v1/chat/completions", json!({"model": "x", "stream": true, "messages": [{"role": "user", "content": format!("mail {synthetic_email}")}]})).await?;
    let t = r.text().await?;
    check("streaming SSE works and restores", t.contains("[DONE]") && t.contains("synthetic.user"), "chat completions stream".into());
    let r = post("/v1/responses", json!({"model": "x", "input": format!("mail {synthetic_email}")})).await?;
    let v: Value = r.json().await?;
    check("Responses API works and restores", v["object"] == "response" && v.to_string().contains(synthetic_email), "responses".into());
    check("no plaintext synthetic secret reached the mock upstream", !rec.all_text().contains(synthetic_ssn) && !rec.all_text().contains(synthetic_key) && !rec.all_text().contains(synthetic_email), format!("{} upstream requests recorded", rec.count()));
    st.shutdown.notify_waiters();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(json!({"passed": passed, "checks": checks}))
}

async fn clients(paths: &Paths, port: u16, cmd: ClientCmd) -> Result<()> {
    let h = home();
    match cmd {
        ClientCmd::Detect => println!("{}", serde_json::to_string_pretty(&client::detect(&h))?),
        ClientCmd::Configure { client: c, yes } => {
            let c = client::Client::parse(&c).context("supported clients: codex, aider, opencode, env")?;
            let base = format!("http://127.0.0.1:{port}/v1");
            let key = std::fs::read_to_string(paths.app_key("default")).context("no local API key; run `promptvpn init` first")?;
            if c == client::Client::Env {
                println!("export OPENAI_BASE_URL={base}\nexport OPENAI_API_KEY={}", key.trim());
                return Ok(());
            }
            let path = c.config_path(&h).unwrap();
            println!("This will back up and edit {} so {} sends requests to Prompt VPN at {base}.", path.display(), c.name());
            if !yes && !confirm("Proceed?") {
                bail!("aborted; nothing changed");
            }
            let latest = paths.backups().join(format!("{}.latest.json", c.name()));
            let prior: Option<pv_core::ClientIntegration> = std::fs::read_to_string(&latest).ok().and_then(|s| serde_json::from_str(&s).ok()).filter(|r: &pv_core::ClientIntegration| r.rolled_back_at.is_none());
            let rec = client::configure(c, &h, &paths.backups(), &base, key.trim(), "default", prior.as_ref().and_then(|p| p.backup_path.as_deref()))?;
            if let Ok(api) = Api::new(paths, port) {
                if api.healthy().await {
                    let _ = api.post("/integrations", serde_json::to_value(&rec)?).await;
                }
            }
            std::fs::create_dir_all(paths.backups())?;
            std::fs::write(paths.backups().join(format!("{}.latest.json", c.name())), serde_json::to_string_pretty(&rec)?)?;
            println!("configured {} (backup {}).", c.name(), rec.backup_path.as_deref().unwrap_or(""));
            if c == client::Client::Codex {
                println!("Codex reads the key from PROMPTVPN_API_KEY:  export PROMPTVPN_API_KEY=\"$(cat '{}')\"", paths.app_key("default").display());
            }
            println!("Undo any time: promptvpn client rollback {}", c.name());
        }
        ClientCmd::Rollback { client: c } => {
            let c = client::Client::parse(&c).context("supported clients: codex, aider, opencode")?;
            let p = paths.backups().join(format!("{}.latest.json", c.name()));
            let mut rec: pv_core::ClientIntegration = serde_json::from_str(&std::fs::read_to_string(&p).context("no recorded integration for this client")?)?;
            client::rollback(&rec)?;
            rec.rolled_back_at = Some(chrono::Utc::now());
            std::fs::write(&p, serde_json::to_string_pretty(&rec)?)?;
            if let Ok(api) = Api::new(paths, port) {
                if api.healthy().await {
                    let _ = api.post("/integrations", serde_json::to_value(&rec)?).await;
                }
            }
            println!("restored {} from backup", rec.config_path);
        }
    }
    Ok(())
}

async fn eval_cmd(paths: &Paths, corpus: &str, configs: &str, out: Option<String>, models_dir: Option<String>) -> Result<()> {
    let (cases, meta) = eval::load(corpus)?;
    let list: Vec<&str> = if configs == "all" { vec!["deterministic", "gliner-edge", "gliner-small", "gliner-base"] } else { configs.split(',').collect() };
    let base = models_dir.map(std::path::PathBuf::from).unwrap_or_else(|| paths.models());
    let mut reports = vec![];
    for c in list {
        let span: Option<std::sync::Arc<dyn pv_detectors::span::SpanModel>> = match c {
            "deterministic" => None,
            x if x.starts_with("gliner-") => {
                let name = pv_daemon::models::resolve_name(x.trim_start_matches("gliner-")).ok_or_else(|| anyhow!("unknown config {x}"))?;
                Some(load_gliner(&base.join(&name).display().to_string(), &name)?)
            }
            other => bail!("unknown config {other}"),
        };
        eprintln!("evaluating {c} on {} cases...", cases.len());
        reports.push(eval::run(c, &cases, &meta, span).await);
    }
    let md = eval::markdown(&reports);
    println!("{md}");
    if let Some(o) = out {
        std::fs::write(&o, serde_json::to_string_pretty(&reports)?)?;
        std::fs::write(o.replace(".json", ".md"), md)?;
        eprintln!("wrote {o}");
    }
    Ok(())
}

#[cfg(feature = "gliner")]
fn load_gliner(dir: &str, name: &str) -> Result<std::sync::Arc<dyn pv_detectors::span::SpanModel>> {
    let l = pv_daemon::models::lock();
    let g = &l["span"][name];
    let thr = g["threshold"].as_f64().unwrap_or(0.5) as f32;
    let mode = g["span_mode"].as_str().unwrap_or("token_level");
    let onnx = if std::path::Path::new(&format!("{dir}/model.onnx")).exists() { format!("{dir}/model.onnx") } else { format!("{dir}/model_quint8.onnx") };
    Ok(std::sync::Arc::new(pv_detectors::span::gliner_impl::GlinerModel::load(&format!("{dir}/tokenizer.json"), &onnx, thr, name, mode).map_err(|e| anyhow!(e))?))
}

#[cfg(not(feature = "gliner"))]
fn load_gliner(_dir: &str, _name: &str) -> Result<std::sync::Arc<dyn pv_detectors::span::SpanModel>> {
    bail!("built without the gliner feature")
}
