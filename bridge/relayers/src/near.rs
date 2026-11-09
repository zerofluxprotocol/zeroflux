//! Minimal NEAR JSON-RPC client with local transaction signing.
//!
//! Borsh layouts (nearcore protocol, Transaction V0):
//!   Transaction = signer_id:String | public_key:PublicKey | nonce:u64 |
//!                 receiver_id:String | block_hash:[u8;32] | actions:Vec<Action>
//!   PublicKey   = 0u8 (ED25519) | [u8;32];  Signature = 0u8 | [u8;64]
//!   Action      = 0 CreateAccount | 1 DeployContract{code} | 2 FunctionCall{
//!                 method_name, args, gas:u64, deposit:u128} | 3 Transfer{deposit:u128}
//!                 | 5 AddKey{public_key, AccessKey{nonce:u64, permission: 1 FullAccess}}
//! tx hash = SHA256(borsh(Transaction)); submitted with `send_tx`, waiting
//! until FINAL, so returned outcomes are final (not optimistic).

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const TGAS: u64 = 1_000_000_000_000;

#[derive(Clone)]
pub struct NearSigner {
    pub account_id: String,
    pub key: SigningKey,
}

pub fn parse_secret_key(s: &str) -> Result<SigningKey, String> {
    let body = s.strip_prefix("ed25519:").ok_or("secret key must start with ed25519:")?;
    let raw = bs58::decode(body).into_vec().map_err(|e| e.to_string())?;
    if raw.len() != 64 && raw.len() != 32 {
        return Err("ed25519 secret must be 32 or 64 bytes".into());
    }
    Ok(SigningKey::from_bytes(raw[..32].try_into().unwrap()))
}

pub fn secret_key_string(k: &SigningKey) -> String {
    let mut b = k.to_bytes().to_vec();
    b.extend_from_slice(&k.verifying_key().to_bytes());
    format!("ed25519:{}", bs58::encode(b).into_string())
}

pub fn public_key_string(k: &SigningKey) -> String {
    format!("ed25519:{}", bs58::encode(k.verifying_key().to_bytes()).into_string())
}

impl NearSigner {
    pub fn new(account_id: &str, secret: &str) -> Result<NearSigner, String> {
        Ok(NearSigner { account_id: account_id.into(), key: parse_secret_key(secret)? })
    }
    pub fn public_key(&self) -> String {
        public_key_string(&self.key)
    }
}

pub enum Action {
    CreateAccount,
    DeployContract(Vec<u8>),
    FunctionCall { method: String, args: Vec<u8>, gas: u64, deposit: u128 },
    Transfer(u128),
    AddFullAccessKey([u8; 32]),
}

fn b_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

fn b_bytes(b: &mut Vec<u8>, v: &[u8]) {
    b.extend_from_slice(&(v.len() as u32).to_le_bytes());
    b.extend_from_slice(v);
}

pub fn encode_tx(signer: &str, pk: &[u8; 32], nonce: u64, receiver: &str, block_hash: &[u8; 32], actions: &[Action]) -> Vec<u8> {
    let mut b = Vec::new();
    b_str(&mut b, signer);
    b.push(0);
    b.extend_from_slice(pk);
    b.extend_from_slice(&nonce.to_le_bytes());
    b_str(&mut b, receiver);
    b.extend_from_slice(block_hash);
    b.extend_from_slice(&(actions.len() as u32).to_le_bytes());
    for a in actions {
        match a {
            Action::CreateAccount => b.push(0),
            Action::DeployContract(code) => {
                b.push(1);
                b_bytes(&mut b, code);
            }
            Action::FunctionCall { method, args, gas, deposit } => {
                b.push(2);
                b_str(&mut b, method);
                b_bytes(&mut b, args);
                b.extend_from_slice(&gas.to_le_bytes());
                b.extend_from_slice(&deposit.to_le_bytes());
            }
            Action::Transfer(d) => {
                b.push(3);
                b.extend_from_slice(&d.to_le_bytes());
            }
            Action::AddFullAccessKey(pk) => {
                b.push(5);
                b.push(0);
                b.extend_from_slice(pk);
                b.extend_from_slice(&0u64.to_le_bytes());
                b.push(1); // FullAccess
            }
        }
    }
    b
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub tx_hash: String,
    pub success: bool,
    pub value: Option<Value>,
    pub failure: Option<String>,
    pub logs: Vec<String>,
    pub raw: Value,
}

#[derive(Clone)]
pub struct NearClient {
    pub rpc: String,
}

impl NearClient {
    pub fn new(rpc: &str) -> NearClient {
        NearClient { rpc: rpc.trim_end_matches('/').to_string() }
    }

    pub fn call_rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": "zfx", "method": method, "params": params});
        let r = ureq::post(&self.rpc).timeout(std::time::Duration::from_secs(60)).send_json(body).map_err(|e| format!("NEAR RPC {}: {}", method, e))?;
        let v: Value = r.into_json().map_err(|e| e.to_string())?;
        if let Some(e) = v.get("error") {
            return Err(format!("NEAR RPC {} error: {}", method, e));
        }
        Ok(v["result"].clone())
    }

    /// View call at the given finality ("final" for authoritative state).
    pub fn view(&self, contract: &str, method: &str, args: Value, finality: &str) -> Result<Value, String> {
        let r = self.call_rpc(
            "query",
            json!({"request_type": "call_function", "finality": finality, "account_id": contract, "method_name": method,
                   "args_base64": base64::engine::general_purpose::STANDARD.encode(args.to_string())}),
        )?;
        if let Some(e) = r.get("error") {
            return Err(format!("view {} failed: {}", method, e));
        }
        let bytes: Vec<u8> = r["result"].as_array().ok_or("no result bytes")?.iter().map(|x| x.as_u64().unwrap_or(0) as u8).collect();
        serde_json::from_slice(&bytes).map_err(|e| format!("view {} returned non-JSON: {}", method, e))
    }

    pub fn final_block(&self) -> Result<(u64, String), String> {
        let r = self.call_rpc("block", json!({"finality": "final"}))?;
        Ok((r["header"]["height"].as_u64().unwrap_or(0), r["header"]["hash"].as_str().unwrap_or("").to_string()))
    }

    pub fn account_exists(&self, account: &str) -> bool {
        self.call_rpc("query", json!({"request_type": "view_account", "finality": "final", "account_id": account})).is_ok()
    }

    pub fn view_account(&self, account: &str) -> Result<Value, String> {
        self.call_rpc("query", json!({"request_type": "view_account", "finality": "final", "account_id": account}))
    }

    fn nonce_and_block(&self, s: &NearSigner) -> Result<(u64, [u8; 32]), String> {
        let r = self.call_rpc("query", json!({"request_type": "view_access_key", "finality": "final", "account_id": s.account_id, "public_key": s.public_key()}))?;
        let nonce = r["nonce"].as_u64().ok_or("no access key nonce")?;
        let bh = bs58::decode(r["block_hash"].as_str().ok_or("no block hash")?).into_vec().map_err(|e| e.to_string())?;
        Ok((nonce, bh.try_into().map_err(|_| "bad block hash")?))
    }

    /// Sign and submit; waits for FINAL execution. Retries once on an
    /// invalid-nonce race.
    pub fn send(&self, s: &NearSigner, receiver: &str, actions: Vec<Action>) -> Result<Outcome, String> {
        let mut last_err = String::new();
        for _ in 0..3 {
            let (nonce, bh) = self.nonce_and_block(s)?;
            let pk = s.key.verifying_key().to_bytes();
            let tx = encode_tx(&s.account_id, &pk, nonce + 1, receiver, &bh, &actions);
            let hash: [u8; 32] = Sha256::digest(&tx).into();
            let sig = s.key.sign(&hash);
            let mut signed = tx.clone();
            signed.push(0);
            signed.extend_from_slice(&sig.to_bytes());
            let tx_hash = bs58::encode(hash).into_string();
            match self.call_rpc("send_tx", json!({"signed_tx_base64": base64::engine::general_purpose::STANDARD.encode(&signed), "wait_until": "FINAL"})) {
                Ok(r) => return Ok(parse_outcome(tx_hash, r)),
                Err(e) if e.contains("InvalidNonce") || e.contains("Expired") => {
                    last_err = e;
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err)
    }

    pub fn call(&self, s: &NearSigner, contract: &str, method: &str, args: Value, gas_tgas: u64, deposit: u128) -> Result<Outcome, String> {
        self.send(s, contract, vec![Action::FunctionCall { method: method.into(), args: args.to_string().into_bytes(), gas: gas_tgas * TGAS, deposit }])
    }

    pub fn call_raw(&self, s: &NearSigner, contract: &str, method: &str, args: Vec<u8>, gas_tgas: u64, deposit: u128) -> Result<Outcome, String> {
        self.send(s, contract, vec![Action::FunctionCall { method: method.into(), args, gas: gas_tgas * TGAS, deposit }])
    }

    /// Final status of a known transaction.
    pub fn tx_status(&self, tx_hash: &str, sender: &str) -> Result<Outcome, String> {
        let r = self.call_rpc("tx", json!({"tx_hash": tx_hash, "sender_account_id": sender, "wait_until": "FINAL"}))?;
        Ok(parse_outcome(tx_hash.into(), r))
    }
}

fn parse_outcome(tx_hash: String, r: Value) -> Outcome {
    let status = &r["status"];
    let mut logs = Vec::new();
    for ro in r["receipts_outcome"].as_array().cloned().unwrap_or_default() {
        for l in ro["outcome"]["logs"].as_array().cloned().unwrap_or_default() {
            if let Some(s) = l.as_str() {
                logs.push(s.to_string());
            }
        }
    }
    // A tx is successful only if no receipt failed.
    let mut failure = status.get("Failure").map(|f| f.to_string());
    if failure.is_none() {
        for ro in r["receipts_outcome"].as_array().cloned().unwrap_or_default() {
            if let Some(f) = ro["outcome"]["status"].get("Failure") {
                failure = Some(f.to_string());
                break;
            }
        }
    }
    let value = status.get("SuccessValue").and_then(|v| v.as_str()).and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()).and_then(|b| serde_json::from_slice(&b).ok());
    Outcome { tx_hash, success: failure.is_none() && status.get("SuccessValue").is_some(), value, failure, logs, raw: r }
}

/// NEP-413 off-chain message signing (used by NEAR Intents):
/// hash = SHA256(borsh(u32 tag = 2^31 + 413) | borsh(Payload{message:String,
/// nonce:[u8;32], recipient:String, callback_url:Option<String>})).
pub fn nep413_hash(message: &str, nonce: &[u8; 32], recipient: &str, callback_url: Option<&str>) -> [u8; 32] {
    let mut b = (2_147_484_061u32).to_le_bytes().to_vec(); // 2^31 + 413
    b_str(&mut b, message);
    b.extend_from_slice(nonce);
    b_str(&mut b, recipient);
    match callback_url {
        None => b.push(0),
        Some(u) => {
            b.push(1);
            b_str(&mut b, u);
        }
    }
    Sha256::digest(&b).into()
}

pub fn nep413_sign(key: &SigningKey, message: &str, nonce: &[u8; 32], recipient: &str) -> String {
    let h = nep413_hash(message, nonce, recipient, None);
    format!("ed25519:{}", bs58::encode(key.sign(&h).to_bytes()).into_string())
}
