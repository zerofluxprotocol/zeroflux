//! obkit operator node.

pub mod api;
pub mod auth;
pub mod config;
pub mod dto;
pub mod oracle;
pub mod sequencer;
pub mod state;

use std::sync::Arc;

pub struct Running {
    pub app: api::App,
    pub addr: std::net::SocketAddr,
    pub server: tokio::task::JoinHandle<()>,
    pub recovered_records: usize,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    /// Stop serving, flush the journal and write a final snapshot.
    pub async fn shutdown(self) {
        self.server.abort();
        for t in &self.tasks {
            t.abort();
        }
        let h = self.app.seq.clone();
        let _ = tokio::task::spawn_blocking(move || h.shutdown()).await;
    }
}

/// Start the sequencer, publisher, batch timer and HTTP server. Returns once listening.
pub async fn run(cfg: config::Config) -> Result<Running, String> {
    let cfg = Arc::new(cfg);
    let started = sequencer::start(&cfg)?;
    let shared = state::Shared::from_engine(&started.initial);
    {
        let shared = shared.clone();
        let rx = started.events;
        std::thread::Builder::new()
            .name("obkit-publisher".into())
            .spawn(move || state::run_publisher(shared, rx))
            .map_err(|e| e.to_string())?;
    }
    let auth = Arc::new(auth::Auth::new(cfg.rate_limit_per_sec));
    let app = api::App {
        cfg: cfg.clone(),
        seq: started.handle.clone(),
        shared: shared.clone(),
        auth: auth.clone(),
    };
    // Batch timer.
    let mut tasks = vec![];
    {
        let h = started.handle.clone();
        let every = std::time::Duration::from_millis(cfg.batch_interval_ms.max(10));
        tasks.push(tokio::spawn(async move {
            let mut t = tokio::time::interval(every);
            t.tick().await;
            loop {
                t.tick().await;
                match h.cut() {
                    Ok(()) => {}
                    Err(sequencer::SubmitError::Overloaded) => tracing::warn!("batch cut skipped: sequencer queue full"),
                    Err(sequencer::SubmitError::Stopped) => return,
                }
            }
        }));
    }
    // Funding ticks for every perp market.
    {
        let perps: Vec<u16> = started
            .initial
            .markets
            .iter()
            .filter(|m| m.spec.perp.is_some())
            .map(|m| m.spec.id)
            .collect();
        if !perps.is_empty() {
            let h = started.handle.clone();
            let every = std::time::Duration::from_millis(cfg.funding_interval_ms.max(1_000));
            tasks.push(tokio::spawn(async move {
                let mut t = tokio::time::interval(every);
                t.tick().await;
                loop {
                    t.tick().await;
                    for m in &perps {
                        let c = obkit_engine::Command::FundingTick {
                            market: *m,
                            ts_ns: auth::now_ns(),
                        };
                        match h.submit(c).await {
                            Err(sequencer::SubmitError::Stopped) => return,
                            Ok(Err(r)) => tracing::warn!(market = m, ?r, "funding tick rejected"),
                            _ => {}
                        }
                    }
                }
            }));
        }
    }
    if let Some(o) = cfg.oracle.clone() {
        tasks.push(tokio::spawn(oracle::run(o, started.handle.clone(), shared.clone())));
    }
    tasks.push(tokio::spawn(async move {
        let mut t = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            t.tick().await;
            auth.purge();
        }
    }));
    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .map_err(|e| format!("bind {}: {e}", cfg.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let router = api::router(app.clone());
    let server = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(%e, "server stopped");
        }
    });
    tracing::info!(%addr, "obkit-node listening");
    Ok(Running {
        app,
        addr,
        server,
        recovered_records: started.recovered_records,
        tasks,
    })
}
