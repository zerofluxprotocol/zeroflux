//! The settlement adapter: the only bridge between the node and NEAR.
//!
//! Each step:
//! 1. reads the node's chain cursor, fetches the next contract events and forwards them in order
//!    (deposits, key changes, forced withdrawals, reverted withdrawals, batch confirmations);
//! 2. submits the node's unconfirmed batches in id order, skipping any the contract already has
//!    (so a restart or a lost response never double-submits; the contract also rejects a wrong id);
//! 3. prunes consumed events from contract storage now and then.
//!
//! It holds the operator key and nothing else. Stateless: everything it needs is on the node or
//! on-chain, so it can be restarted at any time.

use crate::{Actor, Settlement};
use anyhow::{Result, bail};
use zfx_perps_client::Admin;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepReport {
    pub events_forwarded: usize,
    pub batches_submitted: Vec<u64>,
    pub gas_burnt: u64,
    pub pruned_upto: Option<u64>,
}

pub struct Adapter {
    pub settlement: Settlement,
    pub operator: Actor,
    pub node: Admin,
    pub poll: Duration,
    /// Keep this many consumed events on-chain before pruning (for debugging/auditing).
    pub keep_events: u64,
    pub event_page: u32,
}

impl Adapter {
    pub fn new(settlement: Settlement, operator: Actor, node: Admin) -> Self {
        Adapter {
            settlement,
            operator,
            node,
            poll: Duration::from_millis(500),
            keep_events: 10_000,
            event_page: 100,
        }
    }

    /// Contract event JSON -> node chain-event JSON (adds the sequence number).
    pub fn to_node_event(seq: u64, ev: &Value) -> Value {
        let mut e = ev.clone();
        e["seq"] = json!(seq.to_string());
        e
    }

    pub async fn step(&self) -> Result<StepReport> {
        let mut rep = StepReport::default();
        // 1. Events.
        loop {
            let cursor = self.node.status().await?["chain_cursor"].as_u64().unwrap_or(0);
            let evs = self.settlement.events(cursor + 1, self.event_page).await?;
            if evs.is_empty() {
                let st = self.settlement.state().await?;
                let first: u64 = st["first_event"].as_str().unwrap_or("1").parse()?;
                if first > cursor + 1 {
                    bail!(
                        "contract pruned events past the node cursor ({first} > {}); the node cannot catch up",
                        cursor + 1
                    );
                }
                break;
            }
            let n = evs.len();
            let body: Vec<Value> = evs.iter().map(|(s, e)| Self::to_node_event(*s, e)).collect();
            let r = self.node.chain_events(body).await?;
            for x in r["results"].as_array().into_iter().flatten() {
                if x["result"]["error"] == "chain_event_gap" {
                    bail!("node reported a chain event gap at {}", x["seq"]);
                }
                if let Some(err) = x["result"]["error"].as_str() {
                    tracing::warn!(seq = %x["seq"], err, "chain event not applied by the engine");
                }
            }
            rep.events_forwarded += n;
            if n < self.event_page as usize {
                break;
            }
        }
        // 2. Batches.
        let pending = self.node.batches().await?;
        let batches = pending["batches"].as_array().cloned().unwrap_or_default();
        if !batches.is_empty() {
            let mut last: u64 = self.settlement.state().await?["last_batch_id"].as_str().unwrap_or("0").parse()?;
            for b in &batches {
                let id: u64 = b["id"].as_str().unwrap_or("0").parse()?;
                if id <= last {
                    continue; // already on-chain; its batch_settled event confirms it to the node
                }
                if id != last + 1 {
                    bail!("batch {id} is not next after on-chain {last}");
                }
                let out = self.settlement.settle_batch(&self.operator, b).await?;
                tracing::info!(
                    batch = id,
                    gas_tgas = out.gas_burnt as f64 / 1e12,
                    entries = b["entries"].as_array().map_or(0, |v| v.len()),
                    "settled"
                );
                rep.gas_burnt += out.gas_burnt;
                rep.batches_submitted.push(id);
                last = id;
            }
        }
        // 3. Pruning.
        let cursor = self.node.status().await?["chain_cursor"].as_u64().unwrap_or(0);
        if cursor > 2 * self.keep_events {
            let st = self.settlement.state().await?;
            let first: u64 = st["first_event"].as_str().unwrap_or("1").parse()?;
            let upto = cursor - self.keep_events;
            if upto >= first + self.keep_events {
                self.settlement.prune_events(&self.operator, upto).await?;
                rep.pruned_upto = Some(upto);
            }
        }
        Ok(rep)
    }

    /// Run until the future is dropped. Errors are logged and retried after a backoff.
    pub async fn run(&self) {
        let mut backoff = self.poll;
        loop {
            match self.step().await {
                Ok(r) => {
                    backoff = self.poll;
                    if r.events_forwarded > 0 || !r.batches_submitted.is_empty() {
                        tracing::debug!(?r, "adapter step");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "adapter step failed; retrying");
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
            }
            tokio::time::sleep(backoff).await;
        }
    }
}
