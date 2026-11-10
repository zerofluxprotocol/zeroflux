//! Settlement relayer for clearing mode: the only bridge between a node and the clearing contract.
//!
//! Each step:
//! 1. forwards contract events after the node's chain cursor, in order;
//! 2. submits the node's unconfirmed batches in id order, skipping ids the contract already has
//!    (a restart or a lost response never double-submits, and the contract refuses a wrong id);
//! 3. prunes consumed events now and then.
//!
//! Stateless: it holds the operator key and nothing else. Batches are borsh bytes cut by the
//! engine; the relayer never edits them, and the contract verifies every op in them.

use crate::Actor;
use crate::clearing::Clearing;
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepReport {
    pub events_forwarded: usize,
    pub batches_submitted: Vec<(u64, String, f64)>,
}

pub struct ClearingAdapter {
    pub clearing: Clearing,
    pub operator: Actor,
    pub node: String,
    pub admin_token: String,
    pub http: reqwest::Client,
    pub poll: Duration,
    pub keep_events: u64,
    pub event_page: u32,
    /// Optional file to append (batch id, tx hash, Tgas, ops) lines to.
    pub log_path: Option<std::path::PathBuf>,
}

impl ClearingAdapter {
    pub fn new(clearing: Clearing, operator: Actor, node: &str, admin_token: &str) -> Self {
        ClearingAdapter {
            clearing,
            operator,
            node: node.trim_end_matches('/').into(),
            admin_token: admin_token.into(),
            http: reqwest::Client::new(),
            poll: Duration::from_millis(500),
            keep_events: 10_000,
            event_page: 100,
            log_path: None,
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let r = self
            .http
            .get(format!("{}{path}", self.node))
            .bearer_auth(&self.admin_token)
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        Ok(r.error_for_status()?.json().await?)
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let r = self
            .http
            .post(format!("{}{path}", self.node))
            .bearer_auth(&self.admin_token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("POST {path}: {status} {v}");
        }
        Ok(v)
    }

    pub async fn step(&self) -> Result<StepReport> {
        let mut rep = StepReport::default();
        loop {
            let cursor = self.get("/internal/status").await?["chain_cursor"].as_u64().unwrap_or(0);
            let evs = self.clearing.events(cursor + 1, self.event_page).await?;
            if evs.is_empty() {
                let st = self.clearing.state().await?;
                let first: u64 = st["first_event"].as_str().unwrap_or("1").parse()?;
                if first > cursor + 1 {
                    bail!("contract pruned events past the node cursor ({first} > {})", cursor + 1);
                }
                break;
            }
            let n = evs.len();
            let body: Vec<Value> = evs.iter().map(|(s, e)| json!([s.to_string(), e])).collect();
            let r = self.post("/internal/clearing/events", json!({"events": body})).await?;
            for x in r["results"].as_array().into_iter().flatten() {
                if x["result"]["error"] == "chain_event_gap" {
                    bail!("node reported a chain event gap at {}", x["seq"]);
                }
                if let Some(err) = x["result"]["error"].as_str() {
                    tracing::warn!(seq = %x["seq"], err, "clearing event not applied by the engine");
                }
            }
            rep.events_forwarded += n;
            if n < self.event_page as usize {
                break;
            }
        }
        let pending = self.get("/internal/clearing/batches").await?;
        if let Some(d) = pending["diverged"].as_str() {
            bail!("engine diverged from the contract: {d}; not submitting");
        }
        let batches = pending["batches"].as_array().cloned().unwrap_or_default();
        if !batches.is_empty() {
            let mut last: u64 = self.clearing.state().await?["last_batch_id"].as_str().unwrap_or("0").parse()?;
            for b in &batches {
                let id: u64 = b["id"].as_str().unwrap_or("0").parse()?;
                if id <= last {
                    continue;
                }
                if id != last + 1 {
                    bail!("batch {id} is not next after on-chain {last}");
                }
                let bytes = base64::engine::general_purpose::STANDARD.decode(b["borsh_b64"].as_str().unwrap_or(""))?;
                let out = self
                    .clearing
                    .near
                    .call_raw(&self.operator, &self.clearing.contract, "settle_batch", bytes, 300)
                    .await?;
                let tgas = out.gas_burnt as f64 / 1e12;
                tracing::info!(batch = id, tgas, ops = %b["ops"], tx = out.tx_hash, "settled");
                if let Some(p) = &self.log_path {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                        let _ = writeln!(f, "{}", json!({"batch": id, "tx": out.tx_hash, "tgas": tgas, "ops": b["ops"], "orders": b["orders"]}));
                    }
                }
                rep.batches_submitted.push((id, out.tx_hash, tgas));
                last = id;
            }
        }
        let cursor = self.get("/internal/status").await?["chain_cursor"].as_u64().unwrap_or(0);
        if cursor > 2 * self.keep_events {
            let st = self.clearing.state().await?;
            let first: u64 = st["first_event"].as_str().unwrap_or("1").parse()?;
            let upto = cursor - self.keep_events;
            if upto >= first + self.keep_events {
                self.clearing.prune_events(&self.operator, upto).await?;
            }
        }
        Ok(rep)
    }

    pub async fn run(&self) {
        let mut backoff = self.poll;
        loop {
            match self.step().await {
                Ok(_) => backoff = self.poll,
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "clearing adapter step failed; retrying");
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
            }
            tokio::time::sleep(backoff).await;
        }
    }
}
