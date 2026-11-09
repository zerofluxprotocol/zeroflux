//! Relayer witness (one of three). Each instance has its own seed, its own
//! ZeroFlux node and its own NEAR RPC view, and signs only what it has
//! independently verified:
//!
//!   POST /attest/mint     -> ed25519 approval of a wZFX mint, if the
//!       deposit is final (>= K confirmations) on *its* node, the Merkle
//!       inclusion proof verifies against the header, and destination
//!       (lockbox), memo (NEAR recipient) and amount match exactly.
//!   POST /attest/release  -> SLH-DSA approval of a native release, if the
//!       NEAR burn record exists at finality "final" with the same
//!       withdrawal id, amount and native recipient, and it was not
//!       already released.
//!   POST /attest/pause    -> 1-of-3 pause approvals (native + NEAR),
//!       issued only if this relayer's own reconciliation fails.
//!   GET  /info
//!
//! A persistent signing log (data_dir/signed.jsonl) records every approval.

use crate::config::RelayerConfig;
use crate::digest::{self, MintAttestation, NearAdmin};
use crate::keys::RelayerKeys;
use crate::near::NearClient;
use crate::zfx::ZfxClient;
use ed25519_dalek::Signer;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Response, Server};
use zfx_verify::block::verify_merkle_proof;
use zfx_verify::bridge::{self, Release};
use zfx_verify::header::BlockHeader;
use zfx_verify::tx::{address_from_string, Transaction};

pub struct Relayer {
    pub cfg: RelayerConfig,
    pub keys: RelayerKeys,
    pub zfx: ZfxClient,
    pub near: NearClient,
    log: Mutex<std::fs::File>,
    /// Test hook: refuse everything (simulates a down/compromised-but-honest-refusing member).
    pub refuse: std::sync::atomic::AtomicBool,
}

fn now_ns() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
}

impl Relayer {
    pub fn new(cfg: RelayerConfig) -> Result<Relayer, String> {
        let seed = crate::keys::read_seed(&cfg.seed_file)?;
        std::fs::create_dir_all(&cfg.data_dir).map_err(|e| e.to_string())?;
        let log = std::fs::OpenOptions::new().create(true).append(true).open(format!("{}/signed.jsonl", cfg.data_dir)).map_err(|e| e.to_string())?;
        Ok(Relayer {
            zfx: ZfxClient::new(&cfg.zfx_node),
            near: NearClient::new(&cfg.near_rpc),
            keys: RelayerKeys::from_seed(&seed),
            cfg,
            log: Mutex::new(log),
            refuse: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn record(&self, kind: &str, id: &str, digest: &[u8], detail: Value) {
        let line = json!({"t": now_ns(), "kind": kind, "id": id, "digest": hex::encode(digest), "detail": detail});
        let mut f = self.log.lock().unwrap();
        let _ = writeln!(f, "{}", line);
        let _ = f.sync_data();
    }

    /// Independent verification of a native deposit, then sign the mint.
    pub fn attest_mint(&self, a: &MintAttestation) -> Result<Value, String> {
        let st = self.zfx.call("zfx_bridge_deposit_status", json!({"deposit_id": a.deposit_id}))?;
        if st["state"] != "final" {
            return Err(format!("deposit not final on my node: state {} confirmations {}", st["state"], st["confirmations"]));
        }
        let conf = st["confirmations"].as_u64().unwrap_or(0);
        if conf < self.cfg.confirmations {
            return Err(format!("only {} confirmations, my policy requires {}", conf, self.cfg.confirmations));
        }
        if st["chain_id"].as_u64() != Some(self.cfg.native_chain_id as u64) || st["native_asset_id"] != self.cfg.native_asset_id {
            return Err("deposit is for a different chain/asset".into());
        }
        // Verify the inclusion proof myself instead of trusting the node's summary.
        let inc = &st["inclusion"];
        let tx: Transaction = serde_json::from_value(inc["tx"].clone()).map_err(|e| e.to_string())?;
        tx.verify_authorization().map_err(|e| format!("deposit tx authorization: {}", e))?;
        if hex::encode(tx.txid()) != a.deposit_id {
            return Err("txid mismatch".into());
        }
        if tx.to != bridge::lockbox_address(self.cfg.native_chain_id) {
            return Err("deposit does not pay the lockbox".into());
        }
        if tx.memo.as_deref() != Some(a.recipient.as_str()) {
            return Err("deposit memo does not name this NEAR recipient".into());
        }
        if a.amount != tx.amount.to_string() {
            return Err("amount mismatch".into());
        }
        let header = BlockHeader::decode(&hex::decode(inc["header_hex"].as_str().unwrap_or("")).map_err(|e| e.to_string())?)?;
        if header.chain_id != self.cfg.native_chain_id || header.height.to_string() != a.native_height || header.height != st["height"].as_u64().unwrap_or(u64::MAX) {
            return Err("header chain/height mismatch".into());
        }
        let leaf = tx.wtxid();
        if hex::encode(leaf) != inc["leaf"].as_str().unwrap_or("") {
            return Err("proof leaf is not this transaction's wtxid".into());
        }
        let proof: Vec<([u8; 32], bool)> = inc["merkle_proof"]
            .as_array()
            .ok_or("no proof")?
            .iter()
            .map(|p| (hex::decode(p["hash"].as_str().unwrap_or("")).ok().and_then(|v| v.try_into().ok()).unwrap_or([0; 32]), p["right"].as_bool().unwrap_or(false)))
            .collect();
        if !verify_merkle_proof(&header.tx_root, inc["leaf_count"].as_u64().unwrap_or(0) as u32, &leaf, &proof) {
            return Err("Merkle inclusion proof does not verify against the header tx_root".into());
        }
        // The block must be on my node's best chain (confirmations > 0 already implies it).
        let blk = self.zfx.call("zfx_node_get_block", json!({"height": header.height}))?;
        if blk["hash"].as_str() != Some(&hex::encode(header.hash())) {
            return Err("deposit block is not on my node's best chain".into());
        }
        // NEAR side: epoch must match, and never sign an already-minted deposit.
        let bs = self.near.view(&self.cfg.wzfx_contract, "bridge_state", json!({}), "final")?;
        if bs["epoch"].as_u64() != Some(a.epoch as u64) {
            return Err(format!("attestation epoch {} != contract epoch {}", a.epoch, bs["epoch"]));
        }
        if bs["paused"] == true {
            return Err("NEAR bridge is paused".into());
        }
        if self.near.view(&self.cfg.wzfx_contract, "is_minted", json!({"deposit_id": a.deposit_id}), "final")? == true {
            return Err("already minted".into());
        }
        let exp: u64 = a.expiry_ns.parse().map_err(|_| "bad expiry")?;
        if exp > now_ns() + 3_600_000_000_000 {
            return Err("expiry too far in the future (max 1 h)".into());
        }
        let d = digest::mint_digest(&self.cfg.wzfx_contract, self.cfg.native_chain_id, &self.cfg.native_asset_id, a);
        let sig = self.keys.ed25519.sign(&d);
        self.record("mint", &a.deposit_id, &d, json!({"recipient": a.recipient, "amount": a.amount, "confirmations": conf}));
        Ok(json!({"member": self.cfg.member, "signature": hex::encode(sig.to_bytes())}))
    }

    /// Independent verification of a finalized NEAR burn, then sign the
    /// native release with SLH-DSA.
    pub fn attest_release(&self, r: &Release, burn_nonce: u64) -> Result<Value, String> {
        let burn = self.near.view(&self.cfg.wzfx_contract, "get_burn", json!({"nonce": burn_nonce.to_string()}), "final")?;
        if burn.is_null() {
            return Err("burn not found at NEAR finality=final".into());
        }
        let wid = burn["withdrawal_id"].as_str().unwrap_or("");
        if wid != hex::encode(r.withdrawal_id) || wid != digest::withdrawal_id(&self.cfg.wzfx_contract, burn_nonce) {
            return Err("withdrawal id mismatch".into());
        }
        if burn["amount"].as_str() != Some(&r.amount.to_string()) {
            return Err("amount mismatch".into());
        }
        let to = address_from_string(burn["native_recipient"].as_str().unwrap_or("")).map_err(|e| format!("burn recipient: {}", e))?;
        if to != r.recipient {
            return Err("recipient mismatch".into());
        }
        let want_ref = format!("{}:{}:{}", self.cfg.wzfx_contract, burn_nonce, burn["block_height"].as_str().unwrap_or(""));
        if r.near_burn_ref != want_ref {
            return Err(format!("near_burn_ref must be {}", want_ref));
        }
        let info = self.zfx.call("zfx_bridge_info", json!({}))?;
        if info["epoch"].as_u64() != Some(r.epoch as u64) {
            return Err("release epoch != native committee epoch".into());
        }
        if info["paused"] == true {
            return Err("native bridge is paused".into());
        }
        let tip = info["height"].as_u64().unwrap_or(0);
        if r.expiry_height <= tip || r.expiry_height > tip + 500 {
            return Err("expiry height outside (tip, tip+500]".into());
        }
        let rs = self.zfx.call("zfx_bridge_redeem_status", json!({"withdrawal_id": wid}))?;
        if rs["state"] != "unknown" && rs["state"] != "mempool" {
            return Err(format!("already released ({})", rs["state"]));
        }
        let d = bridge::release_digest(self.cfg.native_chain_id, r);
        let sig = bridge::sign_digest(&self.keys.slh, &d);
        self.record("release", wid, &d, json!({"amount": r.amount, "burn_nonce": burn_nonce}));
        Ok(json!({"member": self.cfg.member, "sig": hex::encode(sig)}))
    }

    /// 1-of-3 pause approvals for both chains, only if my own check fails.
    pub fn attest_pause(&self, near_admin_nonce: u64, near_epoch: u32, native_op_nonce: u64, native_epoch: u32) -> Result<Value, String> {
        let rep = crate::reconcile::reconcile(&self.zfx, &self.near, &self.cfg.wzfx_contract, self.cfg.native_chain_id)?;
        if rep["invariant_ok"] == true && rep["orphans"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
            return Err("my reconciliation passes; refusing to pause".into());
        }
        let d = digest::near_admin_digest(&self.cfg.wzfx_contract, near_epoch, near_admin_nonce, NearAdmin::Pause);
        let near_sig = self.keys.ed25519.sign(&d);
        let mut op = bridge::AdminOp { epoch: native_epoch, op_nonce: native_op_nonce, action: bridge::AdminAction::Pause, approvals: vec![] };
        let nd = bridge::admin_digest(self.cfg.native_chain_id, &op);
        op.approvals = vec![bridge::Approval { member: self.cfg.member, sig: bridge::sign_digest(&self.keys.slh, &nd) }];
        self.record("pause", "invariant", &d, rep.clone());
        Ok(json!({"near_approval": {"member": self.cfg.member, "signature": hex::encode(near_sig.to_bytes())}, "native_op": op, "report": rep}))
    }

    pub fn info(&self) -> Value {
        json!({"member": self.cfg.member, "ed25519": self.keys.ed25519_hex(), "slh": self.keys.slh_hex(), "zfx_node": self.cfg.zfx_node,
               "near_rpc": self.cfg.near_rpc, "contract": self.cfg.wzfx_contract, "confirmations": self.cfg.confirmations})
    }
}

pub fn serve(r: Arc<Relayer>) -> Result<(), String> {
    let server = Server::http(&r.cfg.listen).map_err(|e| e.to_string())?;
    eprintln!("[relayer {}] listening on {} ed25519 {} slh {}", r.cfg.member, r.cfg.listen, &r.keys.ed25519_hex()[..16], &r.keys.slh_hex()[..16]);
    let server = Arc::new(server);
    let mut hs = Vec::new();
    for _ in 0..4 {
        let (s, r) = (server.clone(), r.clone());
        hs.push(std::thread::spawn(move || {
            while let Ok(mut req) = s.recv() {
                let mut body = String::new();
                let _ = req.as_reader().take(1 << 20).read_to_string(&mut body);
                let path = req.url().to_string();
                let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let refuse = r.refuse.load(std::sync::atomic::Ordering::Relaxed);
                let out: Result<Value, String> = match (req.method(), path.as_str()) {
                    (Method::Get, "/info") => Ok(r.info()),
                    (Method::Post, "/admin/refuse") => {
                        r.refuse.store(v["refuse"] == true, std::sync::atomic::Ordering::Relaxed);
                        Ok(json!({"refuse": v["refuse"] == true}))
                    }
                    _ if refuse => Err("relayer refusing (test hook)".into()),
                    (Method::Post, "/attest/mint") => serde_json::from_value::<MintAttestation>(v.clone()).map_err(|e| e.to_string()).and_then(|a| r.attest_mint(&a)),
                    (Method::Post, "/attest/release") => {
                        let nonce = v["burn_nonce"].as_u64().unwrap_or(u64::MAX);
                        serde_json::from_value::<Release>(v["release"].clone()).map_err(|e| e.to_string()).and_then(|rel| r.attest_release(&rel, nonce))
                    }
                    (Method::Post, "/attest/pause") => r.attest_pause(
                        v["near_admin_nonce"].as_u64().unwrap_or(0),
                        v["near_epoch"].as_u64().unwrap_or(0) as u32,
                        v["native_op_nonce"].as_u64().unwrap_or(0),
                        v["native_epoch"].as_u64().unwrap_or(0) as u32,
                    ),
                    _ => Err("not found".into()),
                };
                let (code, body) = match out {
                    Ok(v) => (200, json!({"ok": true, "result": v})),
                    Err(e) => (409, json!({"ok": false, "error": e})),
                };
                let mut resp = Response::from_string(body.to_string()).with_status_code(code);
                resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = req.respond(resp);
            }
        }));
    }
    for h in hs {
        let _ = h.join();
    }
    Ok(())
}
