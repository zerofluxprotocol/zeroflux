//! Bridge coordinator: observes both chains, drives each transfer through a
//! persistent job state machine, gathers 2-of-3 committee approvals from the
//! three independent relayers, submits, and confirms against on-chain
//! receipts. It holds no committee key: it cannot forge approvals, only
//! relay them. A NEAR account is used only to pay gas for `bridge_mint`.
//!
//! Mint:    OBSERVED -> AWAITING_FINALITY -> ATTESTING -> READY_TO_SUBMIT -> SUBMITTED -> FINALIZED
//!          (REORGED if the deposit leaves the best chain before minting; it
//!           is re-tracked if it is mined again)
//! Release: OBSERVED -> ATTESTING -> READY_TO_SUBMIT -> SUBMITTED -> FINALIZED
//! Any job: PAUSED while either side is paused; FAILED_NEEDS_REVIEW after
//! bounded retries. A reconciliation failure that persists across 3
//! consecutive fresh reads halts the coordinator, writes an incident file and
//! requests 1-of-3 pause approvals for both chains. Nothing is ever minted or
//! released to "fix" a discrepancy.

use crate::config::BridgeEnv;
use crate::digest::{self, MintAttestation};
use crate::jobs::{now_ms, Job, JobKind, JobState, JobStore};
use crate::near::{NearClient, NearSigner};
use crate::reconcile::reconcile;
use crate::zfx::ZfxClient;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use zfx_verify::bridge::{self, Approval, Release};
use zfx_verify::tx::address_from_string;

pub struct Coordinator {
    pub env: BridgeEnv,
    pub zfx: ZfxClient,
    pub near: NearClient,
    pub signer: NearSigner,
    pub store: Mutex<JobStore>,
    pub incident_dir: String,
    pub auto_register: bool,
    pub max_attempts: u32,
    bad_reconciles: Mutex<u32>,
    pub last_report: Mutex<Value>,
    /// Fault injection for tests: probability [0,1) of simulated RPC failure.
    pub fault_rate: f64,
}

fn post(url: &str, body: &Value, timeout_s: u64) -> Result<Value, String> {
    match ureq::post(url).timeout(std::time::Duration::from_secs(timeout_s)).send_json(body.clone()) {
        Ok(r) => {
            let v: Value = r.into_json().map_err(|e| e.to_string())?;
            Ok(v["result"].clone())
        }
        Err(ureq::Error::Status(_, r)) => {
            let v: Value = r.into_json().unwrap_or(Value::Null);
            Err(v["error"].as_str().unwrap_or("relayer error").to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

impl Coordinator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(env: BridgeEnv, zfx_url: &str, signer: NearSigner, store_path: &str, incident_dir: &str, auto_register: bool, fault_rate: f64) -> Result<Coordinator, String> {
        std::fs::create_dir_all(incident_dir).map_err(|e| e.to_string())?;
        Ok(Coordinator {
            near: NearClient::new(&env.near_rpc),
            zfx: ZfxClient::new(zfx_url),
            env,
            signer,
            store: Mutex::new(JobStore::open(store_path)?),
            incident_dir: incident_dir.to_string(),
            auto_register,
            max_attempts: 40,
            bad_reconciles: Mutex::new(0),
            last_report: Mutex::new(Value::Null),
            fault_rate,
        })
    }

    fn fault(&self, what: &str) -> Result<(), String> {
        if self.fault_rate > 0.0 && rand::random::<f64>() < self.fault_rate {
            return Err(format!("injected fault: {} RPC timeout", what));
        }
        Ok(())
    }

    /// Ask all relayers in parallel; returns (approvals, errors).
    fn gather(&self, path: &str, body: Value) -> (Vec<Value>, Vec<String>) {
        let res = Arc::new(Mutex::new((Vec::new(), Vec::new())));
        std::thread::scope(|s| {
            for r in &self.env.relayers {
                let res = res.clone();
                let body = body.clone();
                s.spawn(move || match post(&format!("{}{}", r.url, path), &body, 60) {
                    Ok(v) => res.lock().unwrap().0.push(v),
                    Err(e) => res.lock().unwrap().1.push(format!("member {}: {}", r.member, e)),
                });
            }
        });
        let g = res.lock().unwrap();
        (g.0.clone(), g.1.clone())
    }

    fn observe(&self) -> Result<(), String> {
        self.fault("observe")?;
        let (cursor, burn_cursor) = {
            let s = self.store.lock().unwrap();
            (s.native_cursor, s.near_burn_cursor)
        };
        // Native deposits (look back so reorged-and-remined deposits are seen).
        let from = cursor.saturating_sub(self.env.confirmations + 50);
        let deps = self.zfx.call("zfx_bridge_list_deposits", json!({"from_height": from, "limit": 1000}))?;
        let mut max_h = cursor;
        let mut s = self.store.lock().unwrap();
        for d in deps.as_array().cloned().unwrap_or_default() {
            let id = d["deposit_id"].as_str().unwrap_or("").to_string();
            max_h = max_h.max(d["height"].as_u64().unwrap_or(0));
            s.observe(&id, JobKind::Mint, d);
        }
        s.native_cursor = max_h;
        drop(s);
        // Finalized NEAR burns.
        let burns = self.near.view(&self.env.wzfx_contract, "burns_since", json!({"from_nonce": burn_cursor.to_string(), "limit": 100}), "final")?;
        let mut s = self.store.lock().unwrap();
        for b in burns.as_array().cloned().unwrap_or_default() {
            let id = b["withdrawal_id"].as_str().unwrap_or("").to_string();
            s.observe(&id, JobKind::Release, b);
            s.near_burn_cursor += 1;
        }
        s.save()
    }

    fn near_state(&self) -> Result<Value, String> {
        self.near.view(&self.env.wzfx_contract, "bridge_state", json!({}), "final")
    }

    fn step_mint(&self, j: &mut Job) -> Result<(), String> {
        let id = j.id.clone();
        match j.state {
            JobState::Observed | JobState::AwaitingFinality | JobState::Reorged | JobState::Paused => {
                self.fault("deposit_status")?;
                let st = self.zfx.call("zfx_bridge_deposit_status", json!({"deposit_id": id}))?;
                match st["state"].as_str() {
                    Some("final") => {
                        j.data["block"] = st["block"].clone();
                        j.data["height"] = st["height"].clone();
                        j.to(JobState::Attesting, &format!("final with {} confirmations", st["confirmations"]));
                    }
                    Some("awaiting_confirmations") => j.to(JobState::AwaitingFinality, ""),
                    Some("mempool") => j.to(JobState::AwaitingFinality, "back in mempool"),
                    _ => {
                        if j.state != JobState::Observed {
                            j.to(JobState::Reorged, "deposit not on the best chain");
                        }
                        j.backoff("deposit unknown");
                    }
                }
                Ok(())
            }
            JobState::Attesting => {
                if self.near.view(&self.env.wzfx_contract, "is_minted", json!({"deposit_id": id}), "final")? == true {
                    j.to(JobState::Finalized, "already minted");
                    return Ok(());
                }
                let ns = self.near_state()?;
                let expiry = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64) + self.env.mint_validity_secs * 1_000_000_000;
                let a = MintAttestation {
                    epoch: ns["epoch"].as_u64().unwrap_or(0) as u32,
                    deposit_id: id.clone(),
                    recipient: j.data["near_recipient"].as_str().unwrap_or("").into(),
                    amount: j.data["amount"].as_u64().unwrap_or(0).to_string(),
                    native_height: j.data["height"].as_u64().unwrap_or(0).to_string(),
                    expiry_ns: expiry.to_string(),
                };
                let (oks, errs) = self.gather("/attest/mint", serde_json::to_value(&a).unwrap());
                let committee: Vec<String> = ns["committee"].as_array().map(|v| v.iter().filter_map(|k| k.as_str().map(String::from)).collect()).unwrap_or_default();
                let d = digest::mint_digest(&self.env.wzfx_contract, self.env.native_chain_id, &self.env.native_asset_id, &a);
                let valid: Vec<Value> = oks.into_iter().filter(|ap| verify_ed25519(&committee, ap, &d)).collect();
                let need = ns["threshold"].as_u64().unwrap_or(2) as usize;
                if valid.len() >= need {
                    j.data["attestation"] = serde_json::to_value(&a).unwrap();
                    j.approvals = valid.into_iter().take(need).collect();
                    j.to(JobState::ReadyToSubmit, &format!("{} approvals", need));
                } else {
                    j.backoff(&format!("{} valid approvals of {} needed: {:?}", valid.len(), need, errs));
                }
                Ok(())
            }
            JobState::ReadyToSubmit | JobState::Submitted => {
                if self.near.view(&self.env.wzfx_contract, "is_minted", json!({"deposit_id": id}), "final")? == true {
                    j.to(JobState::Finalized, "is_minted at final");
                    return Ok(());
                }
                // Reorg guard: still final, same block.
                self.fault("pre-submit check")?;
                let st = self.zfx.call("zfx_bridge_deposit_status", json!({"deposit_id": id}))?;
                if st["state"] != "final" || st["block"] != j.data["block"] {
                    j.approvals.clear();
                    j.to(JobState::AwaitingFinality, "evidence changed before mint (reorg); approvals discarded");
                    return Ok(());
                }
                let a: MintAttestation = serde_json::from_value(j.data["attestation"].clone()).map_err(|e| e.to_string())?;
                let now_ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
                if now_ns + 5_000_000_000 > a.expiry_ns.parse::<u64>().unwrap_or(0) {
                    j.approvals.clear();
                    j.to(JobState::Attesting, "attestation near expiry; re-attesting");
                    return Ok(());
                }
                let reg = self.near.view(&self.env.wzfx_contract, "storage_balance_of", json!({"account_id": a.recipient}), "final")?;
                if reg.is_null() {
                    if !self.auto_register {
                        j.backoff("recipient not registered on wZFX and auto-register disabled");
                        return Ok(());
                    }
                    let o = self.near.call(&self.signer, &self.env.wzfx_contract, "storage_deposit", json!({"account_id": a.recipient, "registration_only": true}), 30, 1_250_000_000_000_000_000_000)?;
                    if !o.success {
                        j.backoff(&format!("storage_deposit failed: {:?}", o.failure));
                        return Ok(());
                    }
                }
                self.fault("bridge_mint submit")?;
                let o = self.near.call(&self.signer, &self.env.wzfx_contract, "bridge_mint", json!({"attestation": a, "approvals": j.approvals}), 50, 0)?;
                j.submission = Some(json!({"near_tx": o.tx_hash, "success": o.success, "failure": o.failure}));
                if o.success || o.failure.as_deref().unwrap_or("").contains("already minted") {
                    j.to(JobState::Submitted, &format!("bridge_mint tx {}", o.tx_hash));
                    if self.near.view(&self.env.wzfx_contract, "is_minted", json!({"deposit_id": id}), "final")? == true {
                        j.to(JobState::Finalized, "receipt final, is_minted=true");
                    }
                } else if o.failure.as_deref().unwrap_or("").contains("expired") {
                    j.approvals.clear();
                    j.to(JobState::Attesting, "expired on submit");
                } else {
                    j.backoff(&format!("bridge_mint failed: {:?}", o.failure));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn step_release(&self, j: &mut Job) -> Result<(), String> {
        let wid = j.id.clone();
        let nonce: u64 = j.data["nonce"].as_str().and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
        match j.state {
            JobState::Observed | JobState::Paused | JobState::AwaitingFinality => {
                let b = self.near.view(&self.env.wzfx_contract, "get_burn", json!({"nonce": nonce.to_string()}), "final")?;
                if b.is_null() || b["withdrawal_id"].as_str() != Some(&wid) {
                    j.backoff("burn not visible at finality=final");
                } else {
                    j.to(JobState::Attesting, "burn final on NEAR");
                }
                Ok(())
            }
            JobState::Attesting => {
                let rs = self.zfx.call("zfx_bridge_redeem_status", json!({"withdrawal_id": wid}))?;
                if rs["state"] == "final" {
                    j.to(JobState::Finalized, "already released");
                    return Ok(());
                }
                let info = self.zfx.call("zfx_bridge_info", json!({}))?;
                let tip = info["height"].as_u64().unwrap_or(0);
                let r = Release {
                    epoch: info["epoch"].as_u64().unwrap_or(0) as u32,
                    withdrawal_id: hex::decode(&wid).ok().and_then(|v| v.try_into().ok()).ok_or("bad withdrawal id")?,
                    near_burn_ref: format!("{}:{}:{}", self.env.wzfx_contract, nonce, j.data["block_height"].as_str().unwrap_or("")),
                    recipient: address_from_string(j.data["native_recipient"].as_str().unwrap_or(""))?,
                    amount: j.data["amount"].as_str().and_then(|s| s.parse().ok()).ok_or("bad amount")?,
                    expiry_height: tip + self.env.release_validity_blocks,
                    approvals: vec![],
                };
                let (oks, errs) = self.gather("/attest/release", json!({"release": r, "burn_nonce": nonce}));
                let members: Vec<Vec<u8>> = info["members"].as_array().map(|v| v.iter().filter_map(|k| k.as_str().and_then(|h| hex::decode(h).ok())).collect()).unwrap_or_default();
                let d = bridge::release_digest(self.env.native_chain_id, &r);
                let mut approvals: Vec<Approval> = Vec::new();
                for v in oks {
                    if let Ok(ap) = serde_json::from_value::<Approval>(v) {
                        if !approvals.iter().any(|x| x.member == ap.member) && bridge::count_approvals(&members, &d, std::slice::from_ref(&ap)).is_ok() {
                            approvals.push(ap);
                        }
                    }
                }
                if approvals.len() >= 2 {
                    let mut r = r;
                    approvals.truncate(2);
                    r.approvals = approvals;
                    j.data["release"] = serde_json::to_value(&r).unwrap();
                    j.to(JobState::ReadyToSubmit, "2 SLH-DSA approvals");
                } else {
                    j.backoff(&format!("{} valid release approvals: {:?}", approvals.len(), errs));
                }
                Ok(())
            }
            JobState::ReadyToSubmit | JobState::Submitted => {
                let rs = self.zfx.call("zfx_bridge_redeem_status", json!({"withdrawal_id": wid}))?;
                match rs["state"].as_str() {
                    Some("final") => {
                        j.to(JobState::Finalized, &format!("released in block {} ({} confirmations)", rs["record"]["height"], rs["confirmations"]));
                        return Ok(());
                    }
                    Some("released_awaiting_confirmations") | Some("mempool") => {
                        j.to(JobState::Submitted, "");
                        j.next_attempt_ms = now_ms() + 1000;
                        return Ok(());
                    }
                    _ => {}
                }
                let r: Release = serde_json::from_value(j.data["release"].clone()).map_err(|e| e.to_string())?;
                let tip = self.zfx.call("zfx_bridge_info", json!({}))?["height"].as_u64().unwrap_or(0);
                if tip + 2 >= r.expiry_height {
                    j.to(JobState::Attesting, "release approvals expired before inclusion; re-attesting");
                    return Ok(());
                }
                self.fault("submit release")?;
                match self.zfx.call("zfx_bridge_submit_op", json!({"op": bridge::BridgeOp::Release(r)})) {
                    Ok(v) => {
                        j.submission = Some(json!({"native_op": v["op_id"]}));
                        j.to(JobState::Submitted, &format!("op {}", v["op_id"]));
                    }
                    Err(e) => j.backoff(&format!("submit op: {}", e)),
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn paused(&self) -> Result<(bool, bool), String> {
        let n = self.zfx.call("zfx_bridge_info", json!({}))?["paused"] == true;
        let w = self.near_state()?["paused"] == true;
        Ok((n, w))
    }

    /// One coordinator iteration. Returns the number of jobs advanced.
    pub fn tick(&self) -> Result<usize, String> {
        if let Some(h) = &self.store.lock().unwrap().halted {
            return Err(format!("halted: {}", h));
        }
        self.observe()?;
        let (np, wp) = self.paused()?;
        let ids: Vec<String> = {
            let s = self.store.lock().unwrap();
            s.jobs.values().filter(|j| !j.terminal() && j.next_attempt_ms <= now_ms() && j.lease_until_ms <= now_ms()).map(|j| j.id.clone()).collect()
        };
        let mut advanced = 0;
        for id in ids {
            let mut j = {
                let mut s = self.store.lock().unwrap();
                let j = s.jobs.get_mut(&id).unwrap();
                j.lease_until_ms = now_ms() + 60_000;
                j.clone()
            };
            if np || wp {
                if j.state != JobState::Paused {
                    j.data["resume_state"] = json!(format!("{:?}", j.state));
                    j.to(JobState::Paused, if np { "native bridge paused" } else { "NEAR bridge paused" });
                }
            } else {
                let before = j.state;
                let r = match j.kind {
                    JobKind::Mint => self.step_mint(&mut j),
                    JobKind::Release => self.step_release(&mut j),
                };
                if let Err(e) = r {
                    j.backoff(&e);
                }
                if j.attempts > self.max_attempts && !j.terminal() {
                    j.to(JobState::FailedNeedsReview, "retry budget exhausted");
                }
                if j.state != before {
                    advanced += 1;
                }
            }
            j.lease_until_ms = 0;
            let mut s = self.store.lock().unwrap();
            s.jobs.insert(id, j);
            s.save()?;
        }
        Ok(advanced)
    }

    /// Reconcile; on a violation that persists across 3 fresh reads, halt and
    /// request emergency pauses on both chains.
    pub fn reconcile_and_guard(&self) -> Result<Value, String> {
        let rep = reconcile(&self.zfx, &self.near, &self.env.wzfx_contract, self.env.native_chain_id)?;
        *self.last_report.lock().unwrap() = rep.clone();
        let violation = rep["invariant_ok"] != true || !rep["orphans"].as_array().map(|a| a.is_empty()).unwrap_or(true) || rep["native_custody_ok"] != true || rep["wrapped_supply_ok"] != true;
        let mut bad = self.bad_reconciles.lock().unwrap();
        if !violation {
            *bad = 0;
            return Ok(rep);
        }
        *bad += 1;
        if *bad < 3 {
            return Ok(rep);
        }
        self.halt(&rep)?;
        Ok(rep)
    }

    pub fn halt(&self, rep: &Value) -> Result<(), String> {
        let reason = format!("reconciliation violation: {}", rep["orphans"]);
        let path = format!("{}/incident-{}.json", self.incident_dir, now_ms());
        std::fs::write(&path, serde_json::to_string_pretty(&json!({"reason": reason, "report": rep, "action": "bridge halted; pauses requested; manual review required"})).unwrap()).map_err(|e| e.to_string())?;
        {
            let mut s = self.store.lock().unwrap();
            s.halted = Some(reason.clone());
            s.save()?;
        }
        eprintln!("[coordinator] HALT: {} (incident {})", reason, path);
        // Ask relayers for independent 1-of-3 pause approvals.
        let ns = self.near_state()?;
        let info = self.zfx.call("zfx_bridge_info", json!({}))?;
        let (oks, _errs) = self.gather(
            "/attest/pause",
            json!({"near_admin_nonce": ns["admin_nonce"].as_str().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0), "near_epoch": ns["epoch"],
                   "native_op_nonce": info["admin_nonce"], "native_epoch": info["epoch"]}),
        );
        if let Some(p) = oks.first() {
            let _ = self.near.call(&self.signer, &self.env.wzfx_contract, "bridge_pause", json!({"approvals": [p["near_approval"]]}), 30, 0);
            let _ = self.zfx.call("zfx_bridge_submit_op", json!({"op": {"op": "admin", "epoch": p["native_op"]["epoch"], "op_nonce": p["native_op"]["op_nonce"],
                "action": "pause", "approvals": p["native_op"]["approvals"]}}));
        }
        Ok(())
    }

    pub fn status(&self) -> Value {
        let s = self.store.lock().unwrap();
        json!({"counts": s.counts(), "halted": s.halted, "native_cursor": s.native_cursor, "near_burn_cursor": s.near_burn_cursor,
               "last_reconcile": *self.last_report.lock().unwrap()})
    }
}

fn verify_ed25519(committee: &[String], ap: &Value, digest: &[u8; 32]) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let Some(i) = ap["member"].as_u64() else { return false };
    let Some(k) = committee.get(i as usize).and_then(|h| hex::decode(h).ok()).and_then(|v| <[u8; 32]>::try_from(v).ok()) else { return false };
    let Some(sig) = ap["signature"].as_str().and_then(|h| hex::decode(h).ok()).and_then(|v| <[u8; 64]>::try_from(v).ok()) else { return false };
    VerifyingKey::from_bytes(&k).map(|vk| vk.verify(digest, &Signature::from_bytes(&sig)).is_ok()).unwrap_or(false)
}
