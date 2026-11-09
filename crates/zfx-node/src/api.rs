//! HTTP API: REST v1 (explorer/wallet) and JSON-RPC 2.0 at POST /rpc
//! (docs/CONTRACT_API.md). Request bodies are capped. Admin methods require
//! `Authorization: Bearer <token>` (token file in the data dir, 0600).

use crate::node::{parse_address, Node};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server};
use zfx_verify::bridge::BridgeOp;
use zfx_verify::hash::from_hex32;
use zfx_verify::tx::Transaction;

const MAX_BODY: u64 = 256 * 1024;

pub mod codes {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const TX_REJECTED: i64 = 1001;
    pub const STALE_WORK: i64 = 1002;
    pub const NOT_FOUND: i64 = 1003;
    pub const UNAUTHORIZED: i64 = 1004;
    pub const RATE_LIMITED: i64 = 1005;
    pub const BRIDGE_REJECTED: i64 = 1006;
    pub const FAUCET: i64 = 1007;
    pub const INTERNAL: i64 = 1099;
}

fn reply(req: Request, code: u16, body: String, ctype: &str) {
    let mut r = Response::from_string(body).with_status_code(code);
    r.add_header(Header::from_bytes("Content-Type", ctype).unwrap());
    r.add_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap());
    r.add_header(Header::from_bytes("Access-Control-Allow-Headers", "content-type, authorization").unwrap());
    let _ = req.respond(r);
}

fn json_reply(req: Request, code: u16, v: Value) {
    reply(req, code, v.to_string(), "application/json")
}

fn query(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn static_file(dir: &str, path: &str) -> Option<(Vec<u8>, &'static str)> {
    let rel = if path == "/" || path.is_empty() { "index.html" } else { path.trim_start_matches('/') };
    if rel.contains("..") {
        return None;
    }
    let p = std::path::Path::new(dir).join(rel);
    let data = std::fs::read(&p).ok()?;
    let ct = match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    Some((data, ct))
}

/// Constant-time comparison for the admin token.
fn token_ok(given: Option<&str>, want: &Option<String>) -> bool {
    match (given, want) {
        (Some(g), Some(w)) if g.len() == w.len() => g.bytes().zip(w.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0,
        _ => false,
    }
}

fn rpc_err(id: &Value, code: i64, msg: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg.into()}})
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn param<'a>(p: &'a Value, k: &str) -> Option<&'a Value> {
    p.get(k).or_else(|| p.get(0).and_then(|o| o.get(k)))
}

fn hash_param(p: &Value, k: &str) -> Result<[u8; 32], String> {
    param(p, k).and_then(|v| v.as_str()).and_then(from_hex32).ok_or_else(|| format!("param `{}` must be 32-byte hex", k))
}

/// JSON-RPC 2.0 dispatcher. Returns the response object.
pub fn rpc(node: &Node, ip: Option<std::net::IpAddr>, auth: Option<&str>, req: &Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    if req.get("jsonrpc") != Some(&json!("2.0")) {
        return rpc_err(&id, codes::INVALID_REQUEST, "jsonrpc must be \"2.0\"");
    }
    let Some(method) = req.get("method").and_then(|m| m.as_str()) else {
        return rpc_err(&id, codes::INVALID_REQUEST, "missing method");
    };
    let p = req.get("params").cloned().unwrap_or(json!({}));
    let admin_token = node.lock().opts.admin_token.clone();
    if method.starts_with("zfx_admin_") && !token_ok(auth, &admin_token) {
        return rpc_err(&id, codes::UNAUTHORIZED, "admin method requires a valid bearer token");
    }
    let r: Result<Value, (i64, String)> = (|| {
        let bad = |e: String| (codes::INVALID_PARAMS, e);
        match method {
            "zfx_node_get_tip" => {
                let s = node.status();
                Ok(json!({"height": s["height"], "hash": s["best_hash"], "state_root": s["state_root"], "timestamp": s["tip_timestamp"],
                          "target": s["target"], "cum_work_log2": s["cum_work_log2"], "chain_id": s["chain_id"], "consensus_version": s["consensus_version"]}))
            }
            "zfx_node_get_status" => Ok(node.status()),
            "zfx_node_get_block" => {
                let key = param(&p, "hash")
                    .and_then(|v| v.as_str().map(String::from))
                    .or_else(|| param(&p, "height").and_then(|v| v.as_u64()).map(|h| h.to_string()));
                let key = key.ok_or_else(|| bad("need `hash` or `height`".into()))?;
                node.block_json(&key).ok_or((codes::NOT_FOUND, "block not found".into()))
            }
            "zfx_node_get_transaction" => {
                let h = hash_param(&p, "txid").map_err(bad)?;
                node.tx_json(&h).ok_or((codes::NOT_FOUND, "transaction not found".into()))
            }
            "zfx_get_account" => {
                let a = param(&p, "address").and_then(|v| v.as_str()).ok_or_else(|| bad("need `address`".into()))?;
                Ok(node.account_json(&parse_address(a).map_err(bad)?))
            }
            "zfx_get_address_history" => {
                let a = param(&p, "address").and_then(|v| v.as_str()).ok_or_else(|| bad("need `address`".into()))?;
                let limit = param(&p, "limit").and_then(|v| v.as_u64()).unwrap_or(50).min(500) as usize;
                Ok(node.history_json(&parse_address(a).map_err(bad)?, limit))
            }
            "zfx_estimate_fee" => Ok(node.fee_estimate()),
            "zfx_submit_transaction" | "zfx_bridge_lock" => {
                let tx: Transaction = serde_json::from_value(param(&p, "tx").cloned().unwrap_or(Value::Null)).map_err(|e| bad(e.to_string()))?;
                if method == "zfx_bridge_lock" {
                    let cid = node.lock().chain.cfg.chain_id;
                    if tx.to != zfx_verify::bridge::lockbox_address(cid) {
                        return Err((codes::BRIDGE_REJECTED, "zfx_bridge_lock requires a transfer to the lockbox address".into()));
                    }
                }
                let txid = tx.txid();
                match node.submit_tx(tx, None) {
                    Ok(id) => {
                        Ok(json!({"txid": hex::encode(id), "deposit_id": if method == "zfx_bridge_lock" { json!(hex::encode(id)) } else { Value::Null }}))
                    }
                    // Idempotent resubmission of an identical tx.
                    Err(e) if e.contains("already in mempool") => Ok(json!({"txid": hex::encode(txid), "duplicate": true})),
                    Err(e) => Err((if method == "zfx_bridge_lock" { codes::BRIDGE_REJECTED } else { codes::TX_REJECTED }, e)),
                }
            }
            "zfx_get_mining_work" => {
                let a = param(&p, "address").and_then(|v| v.as_str()).map(parse_address).transpose().map_err(bad)?;
                node.get_work(a).map_err(bad)
            }
            "zfx_submit_mining_result" => {
                let wid = param(&p, "work_id").and_then(|v| v.as_str()).ok_or_else(|| bad("need `work_id`".into()))?;
                let nonce = param(&p, "nonce")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                    .ok_or_else(|| bad("need `nonce` (u64)".into()))?;
                let r = node.submit_work(ip, wid, nonce);
                if r["accepted"] == true {
                    Ok(r)
                } else {
                    Err((r["code"].as_i64().unwrap_or(codes::TX_REJECTED), r["error"].as_str().unwrap_or("rejected").into()))
                }
            }
            "zfx_bridge_info" => Ok(node.bridge_info()),
            "zfx_bridge_list_deposits" => {
                let from = param(&p, "from_height").and_then(|v| v.as_u64()).unwrap_or(0);
                let limit = param(&p, "limit").and_then(|v| v.as_u64()).unwrap_or(500).min(1000) as usize;
                Ok(node.list_deposits(from, limit))
            }
            "zfx_bridge_list_releases" => Ok(node.list_releases()),
            "zfx_bridge_deposit_status" => Ok(node.deposit_status(&hash_param(&p, "deposit_id").map_err(bad)?)),
            "zfx_bridge_redeem_status" => Ok(node.redeem_status(&hash_param(&p, "withdrawal_id").map_err(bad)?)),
            "zfx_bridge_submit_op" => {
                let op: BridgeOp = serde_json::from_value(param(&p, "op").cloned().unwrap_or(Value::Null)).map_err(|e| bad(e.to_string()))?;
                node.submit_bridge_op(op, None).map(|id| json!({"op_id": hex::encode(id)})).map_err(|e| (codes::BRIDGE_REJECTED, e))
            }
            "zfx_faucet" => {
                let a = param(&p, "address").and_then(|v| v.as_str()).ok_or_else(|| bad("need `address`".into()))?;
                node.faucet(parse_address(a).map_err(bad)?).map_err(|e| (codes::FAUCET, e))
            }
            "zfx_admin_add_peer" => {
                let a = param(&p, "addr").and_then(|v| v.as_str()).ok_or_else(|| bad("need `addr`".into()))?;
                node.maintain_peer(a.to_string());
                Ok(json!({"ok": true}))
            }
            "zfx_admin_snapshot_export" => {
                let path = param(&p, "path").and_then(|v| v.as_str()).ok_or_else(|| bad("need `path`".into()))?;
                node.snapshot_export(path).map_err(|e| (codes::INTERNAL, e))
            }
            "zfx_admin_snapshot_verify" => {
                let path = param(&p, "path").and_then(|v| v.as_str()).ok_or_else(|| bad("need `path`".into()))?;
                node.snapshot_verify(path).map_err(|e| (codes::INTERNAL, e))
            }
            "zfx_admin_shutdown" => {
                node.clean_shutdown();
                let n = node.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    drop(n);
                    std::process::exit(0);
                });
                Ok(json!({"ok": true}))
            }
            _ => Err((codes::METHOD_NOT_FOUND, format!("unknown method {}", method))),
        }
    })();
    match r {
        Ok(v) => rpc_ok(&id, v),
        Err((c, m)) => rpc_err(&id, c, m),
    }
}

fn handle(node: &Node, explorer: &Option<String>, mut req: Request) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = req.method().clone();
    let ip = req.remote_addr().map(|a| a.ip());
    let auth = req.headers().iter().find(|h| h.field.equiv("Authorization")).and_then(|h| h.value.as_str().strip_prefix("Bearer ").map(String::from));
    let mut body = String::new();
    if method == Method::Post && (req.as_reader().take(MAX_BODY + 1).read_to_string(&mut body).is_err() || body.len() as u64 > MAX_BODY) {
        return json_reply(req, 413, json!({"error": "body too large"}));
    }
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, parts.as_slice()) {
        (Method::Options, _) => json_reply(req, 204, json!({})),
        (Method::Post, ["rpc"]) => {
            let v: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_reply(req, 200, rpc_err(&Value::Null, codes::PARSE, e.to_string())),
            };
            let out = if let Some(batch) = v.as_array() {
                if batch.len() > 50 {
                    rpc_err(&Value::Null, codes::INVALID_REQUEST, "batch too large")
                } else {
                    Value::Array(batch.iter().map(|r| rpc(node, ip, auth.as_deref(), r)).collect())
                }
            } else {
                rpc(node, ip, auth.as_deref(), &v)
            };
            json_reply(req, 200, out)
        }
        (Method::Get, ["health"]) => {
            let h = node.health();
            let code = if h["ok"] == true { 200 } else { 503 };
            json_reply(req, code, h)
        }
        (Method::Get, ["v1", "status"]) => json_reply(req, 200, node.status()),
        (Method::Get, ["v1", "work"]) => {
            let addr = match query(&url, "address").map(|a| parse_address(&a)) {
                Some(Ok(a)) => Some(a),
                Some(Err(e)) => return json_reply(req, 400, json!({"error": e})),
                None => None,
            };
            match node.get_work(addr) {
                Ok(v) => json_reply(req, 200, v),
                Err(e) => json_reply(req, 400, json!({"error": e})),
            }
        }
        (Method::Post, ["v1", "submit"]) => {
            let v: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_reply(req, 400, json!({"error": e.to_string()})),
            };
            let wid = v["work_id"].as_str().unwrap_or("").to_string();
            match v["nonce"].as_u64().or_else(|| v["nonce"].as_str().and_then(|s| s.parse().ok())) {
                Some(n) => json_reply(req, 200, node.submit_work(ip, &wid, n)),
                None => json_reply(req, 400, json!({"error": "nonce must be a u64 (number or decimal string)"})),
            }
        }
        (Method::Get, ["v1", "block", key]) => match node.block_json(key) {
            Some(v) => json_reply(req, 200, v),
            None => json_reply(req, 404, json!({"error": "not found"})),
        },
        (Method::Get, ["v1", "blocks"]) => {
            let from = query(&url, "from").and_then(|s| s.parse().ok());
            let count = query(&url, "count").and_then(|s| s.parse().ok()).unwrap_or(20);
            json_reply(req, 200, node.blocks_json(from, count))
        }
        (Method::Get, ["v1", "account", a]) => match parse_address(a) {
            Ok(a) => json_reply(req, 200, node.account_json(&a)),
            Err(e) => json_reply(req, 400, json!({"error": e})),
        },
        (Method::Get, ["v1", "history", a]) => match parse_address(a) {
            Ok(a) => json_reply(req, 200, node.history_json(&a, 100)),
            Err(e) => json_reply(req, 400, json!({"error": e})),
        },
        (Method::Get, ["v1", "fee"]) => json_reply(req, 200, node.fee_estimate()),
        (Method::Get, ["v1", "bridge"]) => json_reply(req, 200, node.bridge_info()),
        (Method::Get, ["v1", "tx", id]) => match from_hex32(id).and_then(|h| node.tx_json(&h)) {
            Some(v) => json_reply(req, 200, v),
            None => json_reply(req, 404, json!({"error": "not found"})),
        },
        (Method::Post, ["v1", "tx"]) => match serde_json::from_str::<Transaction>(&body) {
            Ok(tx) => match node.submit_tx(tx, None) {
                Ok(id) => json_reply(req, 200, json!({"accepted": true, "txid": hex::encode(id)})),
                Err(e) => json_reply(req, 400, json!({"accepted": false, "error": e})),
            },
            Err(e) => json_reply(req, 400, json!({"accepted": false, "error": e.to_string()})),
        },
        (Method::Get, ["v1", "mempool"]) => {
            let g = node.lock();
            let v: Vec<Value> =
                g.mempool.ordered().iter().map(|t| json!({"txid": hex::encode(t.txid()), "nonce": t.nonce, "amount": t.amount, "fee": t.fee})).collect();
            drop(g);
            json_reply(req, 200, json!(v))
        }
        (Method::Get, ["v1", "peers"]) => json_reply(req, 200, node.peers_json()),
        (Method::Get, ["metrics"]) => reply(req, 200, node.metrics(), "text/plain; version=0.0.4"),
        (Method::Get, _) => match explorer.as_ref().and_then(|d| static_file(d, &path)) {
            Some((data, ct)) => {
                let mut r = Response::from_data(data);
                r.add_header(Header::from_bytes("Content-Type", ct).unwrap());
                let _ = req.respond(r);
            }
            None => json_reply(req, 404, json!({"error": "not found"})),
        },
        _ => json_reply(req, 405, json!({"error": "method not allowed"})),
    }
}

pub fn serve(node: Node, bind: &str, port: u16, explorer: Option<String>, workers: usize) -> Result<(), String> {
    let server = Arc::new(Server::http((bind, port)).map_err(|e| e.to_string())?);
    for _ in 0..workers.max(1) {
        let s = server.clone();
        let n = node.clone();
        let ex = explorer.clone();
        std::thread::spawn(move || {
            while let Ok(r) = s.recv() {
                handle(&n, &ex, r)
            }
        });
    }
    Ok(())
}
