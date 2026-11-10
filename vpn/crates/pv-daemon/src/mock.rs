//! Local OpenAI-compatible mock upstream that records the exact bytes it
//! receives. Used by integration tests, `promptvpn connect test` and the setup
//! skill's egress test. Behavior is selected by the upstream `model` name:
//!
//! * `mock-echo` (default): reply with the last user/input text
//! * `mock-fixed:<text>`: reply with fixed text
//! * `mock-leak`: reply contains a synthetic SSN and API key (output inspection)
//! * `mock-tool`: reply with a tool call whose arguments echo the input
//! * `mock-fail-<code>`: respond with that HTTP status
//! * `mock-badsse`: (streaming) an oversized, unterminated SSE event

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Recorded {
    pub bodies: Mutex<Vec<Vec<u8>>>,
    /// Authorization header of each request (None when absent).
    pub auth: Mutex<Vec<Option<String>>>,
    pub file: Option<std::path::PathBuf>,
}

impl Recorded {
    pub fn all_text(&self) -> String {
        self.bodies.lock().unwrap().iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect::<Vec<_>>().join("\n")
    }
    pub fn count(&self) -> usize {
        self.bodies.lock().unwrap().len()
    }
    pub fn last(&self) -> Option<Value> {
        self.bodies.lock().unwrap().last().and_then(|b| serde_json::from_slice(b).ok())
    }
}

fn last_text(v: &Value) -> String {
    if let Some(msgs) = v["messages"].as_array() {
        for m in msgs.iter().rev() {
            if m["role"] == "user" || m["role"] == "tool" {
                return match &m["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
                    _ => String::new(),
                };
            }
        }
    }
    match &v["input"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().rev().find_map(|it| match &it["content"] {
            Value::String(s) => Some(s.clone()),
            Value::Array(p) => Some(p.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join(" ")),
            _ => None,
        }).unwrap_or_default(),
        _ => String::new(),
    }
}

fn reply_text(model: &str, v: &Value) -> String {
    if let Some(t) = model.strip_prefix("mock-fixed:") {
        return t.to_string();
    }
    if model == "mock-leak" {
        return "Sure. The SSN on file is 123-45-6789 and the key is sk-proj-LEAKEDabcdefghijklmnopqrstu12345. Done.".into();
    }
    format!("You said: {}", last_text(v))
}

fn chunks(s: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(n).map(|c| c.iter().collect()).collect()
}

async fn chat(State(rec): State<Arc<Recorded>>, headers: axum::http::HeaderMap, body: Bytes) -> Response {
    record(&rec, &body);
    rec.auth.lock().unwrap().push(headers.get("authorization").and_then(|v| v.to_str().ok()).map(String::from));
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let model = v["model"].as_str().unwrap_or("mock-echo").to_string();
    if let Some(code) = model.strip_prefix("mock-fail-").and_then(|c| c.parse::<u16>().ok()) {
        return (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(json!({"error": {"message": "mock failure"}}))).into_response();
    }
    if model == "mock-badsse" && v["stream"].as_bool() == Some(true) {
        return ([("content-type", "text/event-stream")], format!("data: {{\"choices\": [{{\"delta\": {{\"content\": \"x\"}}}}]}}\n\ndata: {}\n", "y".repeat(2 << 20))).into_response();
    }
    let text = reply_text(&model, &v);
    let tool = model == "mock-tool";
    let args = json!({"to": last_text(&v)}).to_string();
    if v["stream"].as_bool() == Some(true) {
        let mut out = String::new();
        let base = |delta: Value, fin: Value| json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 1, "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": fin}]});
        out += &format!("data: {}\n\n", base(json!({"role": "assistant", "content": ""}), Value::Null));
        if tool {
            for (i, c) in chunks(&args, 7).into_iter().enumerate() {
                let d = if i == 0 { json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "send", "arguments": c}}]}) } else { json!({"tool_calls": [{"index": 0, "function": {"arguments": c}}]}) };
                out += &format!("data: {}\n\n", base(d, Value::Null));
            }
            out += &format!("data: {}\n\n", base(json!({}), json!("tool_calls")));
        } else {
            for c in chunks(&text, 5) {
                out += &format!("data: {}\n\n", base(json!({"content": c}), Value::Null));
            }
            out += &format!("data: {}\n\n", base(json!({}), json!("stop")));
        }
        out += "data: [DONE]\n\n";
        return ([("content-type", "text/event-stream")], out).into_response();
    }
    let message = if tool {
        json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "send", "arguments": args}}]})
    } else {
        json!({"role": "assistant", "content": text})
    };
    Json(json!({"id": "chatcmpl-mock", "object": "chat.completion", "created": 1, "model": model, "choices": [{"index": 0, "message": message, "finish_reason": if tool {"tool_calls"} else {"stop"}}], "usage": {"prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10}})).into_response()
}

async fn responses(State(rec): State<Arc<Recorded>>, headers: axum::http::HeaderMap, body: Bytes) -> Response {
    record(&rec, &body);
    rec.auth.lock().unwrap().push(headers.get("authorization").and_then(|v| v.to_str().ok()).map(String::from));
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let model = v["model"].as_str().unwrap_or("mock-echo").to_string();
    if let Some(code) = model.strip_prefix("mock-fail-").and_then(|c| c.parse::<u16>().ok()) {
        return (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(json!({"error": {"message": "mock failure"}}))).into_response();
    }
    let text = reply_text(&model, &v);
    let item = json!({"type": "message", "id": "msg_1", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": text, "annotations": []}]});
    let resp = json!({"id": "resp_mock", "object": "response", "created_at": 1, "status": "completed", "model": model, "output": [item], "usage": {"input_tokens": 5, "output_tokens": 5, "total_tokens": 10}});
    if v["stream"].as_bool() == Some(true) {
        let mut out = String::new();
        let mut seq = 0;
        let mut ev = |ty: &str, mut d: Value| {
            d["type"] = json!(ty);
            d["sequence_number"] = json!(seq);
            seq += 1;
            format!("event: {ty}\ndata: {d}\n\n")
        };
        out += &ev("response.created", json!({"response": {"id": "resp_mock", "object": "response", "status": "in_progress", "output": []}}));
        out += &ev("response.output_item.added", json!({"output_index": 0, "item": {"type": "message", "id": "msg_1", "status": "in_progress", "role": "assistant", "content": []}}));
        out += &ev("response.content_part.added", json!({"item_id": "msg_1", "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": ""}}));
        for c in chunks(&text, 5) {
            out += &ev("response.output_text.delta", json!({"item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": c}));
        }
        out += &ev("response.output_text.done", json!({"item_id": "msg_1", "output_index": 0, "content_index": 0, "text": text}));
        out += &ev("response.content_part.done", json!({"item_id": "msg_1", "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": text}}));
        out += &ev("response.output_item.done", json!({"output_index": 0, "item": item}));
        out += &ev("response.completed", json!({"response": resp}));
        return ([("content-type", "text/event-stream")], out).into_response();
    }
    Json(resp).into_response()
}

fn record(rec: &Recorded, body: &[u8]) {
    rec.bodies.lock().unwrap().push(body.to_vec());
    if let Some(f) = &rec.file {
        use std::io::Write;
        if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
            let _ = fh.write_all(body);
            let _ = fh.write_all(b"\n");
        }
    }
}

pub fn router(rec: Arc<Recorded>) -> Router {
    Router::new().route("/v1/chat/completions", post(chat)).route("/v1/responses", post(responses)).with_state(rec)
}

/// Bind on 127.0.0.1:`port` (0 = ephemeral). Returns the base URL ending in /v1.
pub async fn serve(port: u16, rec: Arc<Recorded>) -> std::io::Result<(String, tokio::task::JoinHandle<()>)> {
    let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = l.local_addr()?;
    let h = tokio::spawn(async move {
        let _ = axum::serve(l, router(rec)).await;
    });
    Ok((format!("http://{addr}/v1"), h))
}
