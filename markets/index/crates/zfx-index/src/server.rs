//! Read-only REST + WebSocket server over the published static tree and the
//! revision ledger. GET only; no write endpoints exist.
//!
//! ```text
//! GET /v1/markets/models
//! GET /v1/indices/or-model-spend/{permaslug}?from=YYYY-MM-DD&to=YYYY-MM-DD
//! GET /v1/indices/or-model-spend/{permaslug}/latest
//! GET /v1/indices/or-model-spend/{permaslug}/provenance/{revision_id}
//! GET /v1/indices/source-health
//! GET /v1/indices/prints/latest
//! GET /v1/oracle/reporter
//! GET /v1/indices/or-model-spend-7d/{permaslug}[?from&to|/latest|/provenance/{revision_id}]
//! WS  /v1/indices/stream[?replay=N]   new daily and 7D revisions (FINAL prints and CORRECTED revisions)
//! ```

use crate::time::UtcDate;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn slug_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.contains("..")
        && !s.starts_with('/')
        && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-._/:~".contains(&c))
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: &'static str,
}

fn json_resp(status: u16, v: &Value) -> Response {
    Response { status, body: serde_json::to_vec(v).unwrap(), content_type: "application/json" }
}
fn err(status: u16, msg: &str) -> Response {
    json_resp(status, &json!({"error": msg}))
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn query_param<'a>(q: &'a str, k: &str) -> Option<&'a str> {
    q.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
}

fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Routes one GET request against the static tree rooted at `root` (the directory containing `v1/`).
pub fn route(root: &Path, method: &str, target: &str) -> Response {
    if method != "GET" && method != "HEAD" {
        return err(405, "read-only API: GET only");
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let Some(path) = pct_decode(path) else { return err(400, "bad path encoding") };
    let v1 = root.join("v1");
    match path.as_str() {
        "/v1/markets/models" => return file(&v1.join("markets/models.json")),
        "/v1/indices/source-health" => return file(&v1.join("indices/source-health.json")),
        "/v1/indices/prints/latest" => return file(&v1.join("indices/prints/latest.json")),
        "/v1/oracle/reporter" => return file(&v1.join("oracle/reporter.json")),
        "/" | "/v1" => {
            return json_resp(200, &json!({"endpoints": ["/v1/markets/models", "/v1/indices/or-model-spend/{permaslug}?from=&to=", "/v1/indices/or-model-spend/{permaslug}/latest", "/v1/indices/or-model-spend/{permaslug}/provenance/{revision_id}", "/v1/indices/source-health", "/v1/indices/prints/latest", "/v1/oracle/reporter", "/v1/indices/or-model-spend-7d/{permaslug}?from=&to=", "/v1/indices/or-model-spend-7d/{permaslug}/latest", "/v1/indices/or-model-spend-7d/{permaslug}/provenance/{revision_id}", "/v1/indices/or-model-spend-lic/{permaslug}?from=&to=", "/v1/indices/or-model-spend-lic-7d/{permaslug}?from=&to=", "WS /v1/indices/stream"]}))
        }
        _ => {}
    }
    for dir in ["or-model-spend", "or-model-spend-7d", "or-model-spend-lic", "or-model-spend-lic-7d"] {
        if let Some(rest) = path.strip_prefix(&format!("/v1/indices/{dir}/")) {
            return index_route(&v1.join("indices").join(dir), rest, query);
        }
    }
    err(404, "not found")
}

/// `{permaslug}[?from&to]`, `{permaslug}/latest`, `{permaslug}/provenance/{revision_id}` under one index dir.
fn index_route(base: &Path, rest: &str, query: &str) -> Response {
    let rest = rest.trim_end_matches('/');
    if let Some((slug, rid)) = rest.split_once("/provenance/") {
        if !slug_ok(slug) || !slug_ok(rid) || rid.contains('/') {
            return err(400, "bad slug or revision id");
        }
        return file(&base.join(slug).join("provenance").join(format!("{rid}.json")));
    }
    if let Some(slug) = rest.strip_suffix("/latest") {
        if !slug_ok(slug) {
            return err(400, "bad slug");
        }
        return file(&base.join(slug).join("latest.json"));
    }
    if !slug_ok(rest) {
        return err(400, "bad slug");
    }
    let Some(mut series) = read_json(&base.join(rest).join("series.json")) else {
        return err(404, "unknown market");
    };
    let from = query_param(query, "from");
    let to = query_param(query, "to");
    let (f, t) = match (from.map(UtcDate::parse), to.map(UtcDate::parse)) {
        (Some(None), _) | (_, Some(None)) => return err(400, "from/to must be YYYY-MM-DD"),
        (a, b) => (a.flatten(), b.flatten()),
    };
    if let (Some(f), Some(t)) = (f, t) {
        if f > t {
            return err(400, "from is after to");
        }
    }
    if let Some(points) = series["points"].as_array_mut() {
        points.retain(|p| {
            let d = p["utc_date"].as_str().and_then(UtcDate::parse);
            match d {
                Some(d) => f.map(|f| d >= f).unwrap_or(true) && t.map(|t| d <= t).unwrap_or(true),
                None => false,
            }
        });
    }
    series["from"] = json!(f.map(|d| d.to_string()));
    series["to"] = json!(t.map(|d| d.to_string()));
    json_resp(200, &series)
}

fn file(p: &Path) -> Response {
    match std::fs::read(p) {
        Ok(b) => Response { status: 200, body: b, content_type: "application/json" },
        Err(_) => err(404, "not found"),
    }
}

fn reason(s: u16) -> &'static str {
    match s {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    }
}

fn handle_http(mut stream: TcpStream, root: &Path) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut r = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    let resp = route(root, method, target);
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: public, max-age=60\r\nX-Index-Notice: paper-trading index, not settlement-grade\r\nConnection: close\r\n\r\n",
        resp.status,
        reason(resp.status),
        resp.content_type,
        resp.body.len()
    );
    stream.write_all(head.as_bytes())?;
    if method != "HEAD" {
        stream.write_all(&resp.body)?;
    }
    stream.flush()
}

/// Streams revisions appended to `<ledger>/revisions.jsonl` after connect.
fn handle_ws(stream: TcpStream, ledger: &Path, replay: usize) {
    let Ok(mut ws) = tungstenite::accept(stream) else { return };
    let files = [ledger.join("revisions.jsonl"), ledger.join("revisions_7d.jsonl"), ledger.join("revisions_lic.jsonl"), ledger.join("revisions_lic_7d.jsonl")];
    let read_lines = |p: &PathBuf| -> Vec<String> {
        std::fs::File::open(p).map(|f| BufReader::new(f).lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).collect()).unwrap_or_default()
    };
    let mut sent: Vec<usize> = files.iter().map(|f| read_lines(f).len()).collect();
    let hello = json!({"type": "hello", "revisions_in_ledger": {"daily": sent[0], "7d": sent[1], "lic": sent[2], "lic_7d": sent[3]}, "note": "Events are new daily and 7D revisions. This stream never carries intraday usage."});
    if ws.send(tungstenite::Message::text(hello.to_string())).is_err() {
        return;
    }
    for (i, f) in files.iter().enumerate() {
        let lines = read_lines(f);
        for l in lines.iter().skip(sent[i].saturating_sub(replay)) {
            if send_rev(&mut ws, l).is_err() {
                return;
            }
        }
    }
    let _ = ws.get_mut().set_read_timeout(Some(Duration::from_millis(500)));
    loop {
        for (i, f) in files.iter().enumerate() {
            let lines = read_lines(f);
            for l in lines.iter().skip(sent[i]) {
                if send_rev(&mut ws, l).is_err() {
                    return;
                }
            }
            sent[i] = sent[i].max(lines.len());
        }
        match ws.read() {
            Ok(tungstenite::Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return,
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
}

fn send_rev(ws: &mut tungstenite::WebSocket<TcpStream>, line: &str) -> Result<(), tungstenite::Error> {
    let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
    let kind = if v["finality"] == "CORRECTED" { "correction" } else { "print" };
    let ev = json!({"type": kind, "index": v["index"].as_str().unwrap_or("DAILY"), "index_id": v["index_id"], "market": v["market"], "utc_date": v["utc_date"], "revision_id": v["revision_id"], "data_status": v["data_status"], "value_usd": v["value_usd"], "blocked": v["blocked"], "clamped": v["clamped"], "print": v["print"]});
    ws.send(tungstenite::Message::text(ev.to_string()))
}

pub fn serve(addr: &str, root: PathBuf, ledger: PathBuf) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    eprintln!("[serve] listening on http://{}", listener.local_addr()?);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (root, ledger) = (root.clone(), ledger.clone());
        std::thread::spawn(move || {
            let mut peek = [0u8; 1024];
            let n = stream.peek(&mut peek).unwrap_or(0);
            let head = String::from_utf8_lossy(&peek[..n]).to_string();
            let first = head.lines().next().unwrap_or("");
            let target = first.split_whitespace().nth(1).unwrap_or("");
            let is_ws = target.starts_with("/v1/indices/stream") && head.to_ascii_lowercase().contains("upgrade: websocket");
            if is_ws {
                let replay = target.split_once('?').and_then(|(_, q)| query_param(q, "replay")).and_then(|x| x.parse().ok()).unwrap_or(0usize).min(1000);
                handle_ws(stream, &ledger, replay);
            } else if target.starts_with("/v1/indices/stream") {
                let mut s = stream;
                let _ = s.read(&mut [0u8; 1024]);
                let _ = s.write_all(b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            } else {
                let _ = handle_http(stream, &root);
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert!(slug_ok("anthropic/claude-opus-5.5-20260921"));
        assert!(slug_ok("openai/gpt-oss-120b:free"));
        assert!(!slug_ok("../etc/passwd"));
        assert!(!slug_ok("/abs"));
        assert!(!slug_ok("a/b c"));
        assert!(!slug_ok("A/B"));
    }

    #[test]
    fn routes_and_ranges() {
        let root = std::env::temp_dir().join(format!("zfx-srv-{}", std::process::id()));
        let d = root.join("v1/indices/or-model-spend/a/m-1");
        std::fs::create_dir_all(d.join("provenance")).unwrap();
        std::fs::write(d.join("series.json"), r#"{"points":[{"utc_date":"2026-11-01"},{"utc_date":"2026-11-02"},{"utc_date":"2026-11-03"}]}"#).unwrap();
        std::fs::write(d.join("latest.json"), r#"{"utc_date":"2026-11-03"}"#).unwrap();
        std::fs::write(d.join("provenance/2026-11-02.r1.json"), r#"{"revision_id":"2026-11-02.r1"}"#).unwrap();
        let r = route(&root, "GET", "/v1/indices/or-model-spend/a/m-1?from=2026-11-02&to=2026-11-02");
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["points"].as_array().unwrap().len(), 1);
        let r = route(&root, "GET", "/v1/indices/or-model-spend/a%2Fm-1/latest");
        assert_eq!(r.status, 200);
        assert_eq!(route(&root, "GET", "/v1/indices/or-model-spend/a/m-1/provenance/2026-11-02.r1").status, 200);
        assert_eq!(route(&root, "GET", "/v1/indices/or-model-spend/a/m-1?from=2026-13-01").status, 400);
        assert_eq!(route(&root, "GET", "/v1/indices/or-model-spend/a/m-1?from=2026-11-03&to=2026-11-01").status, 400);
        assert_eq!(route(&root, "GET", "/v1/indices/or-model-spend/../../x").status, 400);
        assert_eq!(route(&root, "POST", "/v1/markets/models").status, 405);
        assert_eq!(route(&root, "GET", "/v1/indices/or-model-spend/nope/x").status, 404);
        let d7 = root.join("v1/indices/or-model-spend-7d/a/m-1");
        std::fs::create_dir_all(&d7).unwrap();
        std::fs::write(d7.join("series.json"), r#"{"points":[{"utc_date":"2026-11-07"},{"utc_date":"2026-11-08"}]}"#).unwrap();
        std::fs::write(d7.join("latest.json"), r#"{"utc_date":"2026-11-08","index":"7D"}"#).unwrap();
        let r = route(&root, "GET", "/v1/indices/or-model-spend-7d/a/m-1?from=2026-11-08");
        assert_eq!(serde_json::from_slice::<Value>(&r.body).unwrap()["points"].as_array().unwrap().len(), 1);
        assert_eq!(route(&root, "GET", "/v1/indices/or-model-spend-7d/a/m-1/latest").status, 200);
        assert_eq!(route(&root, "GET", "/v1/indices/or-model-spend-7d/../x").status, 400);
        let _ = std::fs::remove_dir_all(&root);
    }
}
