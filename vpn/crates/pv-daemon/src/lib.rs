//! Prompt VPN daemon: loopback-only HTTP server for the OpenAI-compatible
//! proxy, the admin API and the embedded dashboard.

pub mod admin;
pub mod assets;
pub mod auth;
pub mod mock;
pub mod models;
pub mod proxy_routes;
pub mod state;

use axum::extract::DefaultBodyLimit;
use axum::routing::{any, get, post};
use axum::Router;
use state::AppState;
use std::net::SocketAddr;
use std::sync::Arc;

pub fn router(st: Arc<AppState>) -> Router {
    let limit = st.engine.settings().max_request_bytes + 1024;
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/chat/completions", post(proxy_routes::chat))
        .route("/v1/responses", post(proxy_routes::responses))
        .route("/v1/models", get(proxy_routes::models))
        .route("/v1/{*rest}", any(proxy_routes::unsupported))
        .nest("/api/admin/v1", admin::router(st.clone()))
        .route("/dashboard/login", get(assets::login))
        .route("/dashboard", get(|| async { axum::response::Redirect::to("/dashboard/") }))
        .route("/dashboard/", get(assets::index))
        .route("/dashboard/{*path}", get(assets::file))
        .route("/", get(|| async { axum::response::Redirect::to("/dashboard/") }))
        .layer(DefaultBodyLimit::max(limit))
        .layer(axum::middleware::from_fn_with_state(st.clone(), auth::host_guard))
        .with_state(st)
}

/// Public health: no secrets, no configuration details.
async fn healthz(axum::extract::State(st): axum::extract::State<Arc<AppState>>) -> axum::Json<serde_json::Value> {
    let s = st.engine.settings();
    axum::Json(serde_json::json!({
        "status": "ok",
        "version": state::VERSION,
        "protected": !s.emergency_stop,
    }))
}

/// Bind strictly to loopback. Never falls back to another interface or port.
pub async fn bind(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            std::io::Error::new(e.kind(), format!("127.0.0.1:{port} is already in use; stop the other process or choose another port with --port (Prompt VPN never binds to non-loopback addresses)"))
        } else {
            e
        }
    })
}

pub async fn serve(st: Arc<AppState>, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let port = listener.local_addr()?.port();
    st.port.store(port, std::sync::atomic::Ordering::Relaxed);
    let _ = state::write_private(&st.paths.port_file(), port.to_string().as_bytes());
    // Housekeeping: expire held payloads (15 min) and mappings; audit retention.
    let hk = st.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            t.tick().await;
            hk.engine.housekeeping();
        }
    });
    let app = router(st.clone());
    let s2 = st.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            s2.shutdown.notified().await;
        })
        .await?;
    let _ = std::fs::remove_file(st.paths.port_file());
    Ok(())
}
