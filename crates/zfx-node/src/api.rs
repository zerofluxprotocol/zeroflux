//! HTTP JSON API (v1). Request bodies are capped; every handler is
//! read-only except /v1/submit and /v1/tx.

use crate::node::{parse_address, Node};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server};
use zfx_verify::hash::from_hex32;
use zfx_verify::tx::Transaction;

const MAX_BODY: u64 = 64 * 1024;

fn reply(req: Request, code: u16, body: String, ctype: &str) {
    let mut r = Response::from_string(body).with_status_code(code);
    r.add_header(Header::from_bytes("Content-Type", ctype).unwrap());
    r.add_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap());
    r.add_header(Header::from_bytes("Access-Control-Allow-Headers", "content-type").unwrap());
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
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    Some((data, ct))
}

fn handle(node: &Node, explorer: &Option<String>, mut req: Request) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = req.method().clone();
    let ip = req.remote_addr().map(|a| a.ip());
    let mut body = String::new();
    if method == Method::Post {
        if req.as_reader().take(MAX_BODY + 1).read_to_string(&mut body).is_err() || body.len() as u64 > MAX_BODY {
            return json_reply(req, 413, json!({"error": "body too large"}));
        }
    }
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, parts.as_slice()) {
        (Method::Options, _) => json_reply(req, 204, json!({})),
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
            let nonce = v["nonce"].as_u64().or_else(|| v["nonce"].as_str().and_then(|s| s.parse().ok()));
            match nonce {
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
            let g = node.0.lock().unwrap();
            let v: Vec<Value> = g.mempool.ordered().iter().map(|t| json!({"txid": hex::encode(t.txid()), "nonce": t.nonce, "amount": t.amount, "fee": t.fee})).collect();
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

pub fn serve(node: Node, port: u16, explorer: Option<String>, workers: usize) -> Result<(), String> {
    let server = Arc::new(Server::http(("0.0.0.0", port)).map_err(|e| e.to_string())?);
    for _ in 0..workers.max(1) {
        let s = server.clone();
        let n = node.clone();
        let ex = explorer.clone();
        std::thread::spawn(move || loop {
            match s.recv() {
                Ok(r) => handle(&n, &ex, r),
                Err(_) => break,
            }
        });
    }
    Ok(())
}
