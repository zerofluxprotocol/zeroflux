//! `zfx-vpn-hosted serve`: run the hosted gateway.
//!
//! The public API and the operator API listen on separate addresses; keep the
//! operator listener private (loopback or an internal network).

use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(name = "zfx-vpn-hosted", version, about = "ZFX VPN hosted gateway (multi-tenant). The operator can see raw prompts in this mode; see /v1/zfx/privacy-policy")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Serve {
        /// Public OpenAI-compatible API.
        #[arg(long, default_value = "127.0.0.1:8800")]
        listen: SocketAddr,
        /// Operator API (tenants, keys, credentials, usage). Keep private.
        #[arg(long, default_value = "127.0.0.1:8801")]
        admin_listen: SocketAddr,
        /// Data directory (control.db, per-tenant metadata). Omit for memory only.
        #[arg(long)]
        data: Option<PathBuf>,
        /// Directory containing GLiNER models (as installed by `zfx-vpn model install`).
        #[arg(long)]
        models_dir: Option<PathBuf>,
        /// GLiNER model to load (gliner-pii-edge, gliner-pii-small, gliner-pii-base).
        #[arg(long, default_value = "gliner-pii-base")]
        model: String,
        /// Keep completed non-streaming responses in memory for idempotent replay (seconds; 0 = never).
        #[arg(long, default_value_t = 0)]
        idempotency_response_ttl: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("ZFX_LOG").unwrap_or_else(|_| "info".into())).with_writer(std::io::stderr).init();
    let Cmd::Serve { listen, admin_listen, data, models_dir, model, idempotency_response_ttl } = Cli::parse().cmd;
    let admin_token = match std::env::var("ZFX_HOSTED_ADMIN_TOKEN") {
        Ok(t) if t.len() >= 32 => Zeroizing::new(t),
        _ => {
            let mut b = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
            let t = format!("zfxop_{}", hex::encode(b));
            if let Some(d) = &data {
                pv_daemon::state::write_private(&d.join("operator.token"), t.as_bytes())?;
                eprintln!("operator token written to {}", d.join("operator.token").display());
            } else {
                eprintln!("operator token (memory mode, shown once): {t}");
            }
            Zeroizing::new(t)
        }
    };
    let span = match models_dir {
        Some(dir) => {
            let det = Arc::new(pv_detectors::Detector::default());
            let parent = dir.parent().map(PathBuf::from).unwrap_or_default();
            let mm = pv_daemon::models::ModelManager::new(pv_daemon::state::Paths::at(parent), det.clone());
            let ok = mm.load(&model);
            let mut s = det.settings();
            s.span_required = true; // installed model: fail closed if it stops working
            det.set_settings(s);
            eprintln!("GLiNER {model}: {}", if ok { "loaded" } else { "FAILED to load (requests will fail closed)" });
            Some(det)
        }
        None => {
            eprintln!("no --models-dir: deterministic detectors only (no GLiNER)");
            None
        }
    };
    let cfg = zfx_vpn_hosted::Config { data_dir: data, admin_token, idempotency_response_ttl_secs: idempotency_response_ttl, ..Default::default() };
    let h = zfx_vpn_hosted::Hosted::new(cfg, span)?;
    let hk = h.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            t.tick().await;
            hk.housekeeping();
        }
    });
    let public = tokio::net::TcpListener::bind(listen).await?;
    let admin = tokio::net::TcpListener::bind(admin_listen).await?;
    eprintln!("zfx-vpn-hosted public API http://{} operator API http://{}", public.local_addr()?, admin.local_addr()?);
    let ar = zfx_vpn_hosted::admin_router(h.clone());
    let a = tokio::spawn(async move {
        let _ = axum::serve(admin, ar).await;
    });
    axum::serve(public, zfx_vpn_hosted::router(h)).await?;
    a.abort();
    Ok(())
}
