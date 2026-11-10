//! Server-sent-event parsing and per-protocol stream processors.

use crate::output::{OutCtx, OutStats, OutputBlocked, TextGuard};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const MAX_EVENT_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental SSE decoder with a bounded buffer.
#[derive(Default)]
pub struct SseParser {
    buf: String,
    raw: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SseError {
    #[error("SSE event exceeds size limit")]
    TooLarge,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        self.raw.extend_from_slice(bytes);
        // Only decode complete UTF-8 prefixes.
        let valid = match std::str::from_utf8(&self.raw) {
            Ok(_) => self.raw.len(),
            Err(e) => e.valid_up_to(),
        };
        let s = String::from_utf8_lossy(&self.raw[..valid]).into_owned();
        self.raw.drain(..valid);
        self.buf.push_str(&s.replace("\r\n", "\n"));
        if self.buf.len() > MAX_EVENT_BYTES && !self.buf.contains("\n\n") {
            return Err(SseError::TooLarge);
        }
        let mut out = Vec::new();
        while let Some(i) = self.buf.find("\n\n") {
            let block: String = self.buf[..i].to_string();
            self.buf.drain(..i + 2);
            if let Some(ev) = parse_block(&block) {
                out.push(ev);
            }
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Vec<SseEvent> {
        let rest = std::mem::take(&mut self.buf);
        parse_block(rest.trim_end()).into_iter().collect()
    }
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut data: Vec<&str> = Vec::new();
    for line in block.lines() {
        if let Some(v) = line.strip_prefix("event:") {
            event = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("data:") {
            data.push(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    if data.is_empty() && event.is_none() {
        return None;
    }
    Some(SseEvent { event, data: data.join("\n") })
}

pub fn data_line(v: &Value) -> String {
    format!("data: {v}\n\n")
}

// ------------------------------------------------------------------ Chat Completions

struct ToolBuf {
    id: Option<String>,
    name: String,
    args: String,
    typ: Option<String>,
}

/// Processes upstream Chat Completions chunks into inspected chunks.
pub struct ChatStream {
    ctx: Arc<OutCtx>,
    max_bytes: usize,
    content: BTreeMap<u64, TextGuard>,
    reasoning: BTreeMap<u64, TextGuard>,
    tools: BTreeMap<(u64, u64), ToolBuf>,
    meta: Value,
    finish: BTreeMap<u64, Value>,
    pub completion_id: Option<String>,
}

impl ChatStream {
    pub fn new(ctx: Arc<OutCtx>, max_bytes: usize) -> Self {
        ChatStream { ctx, max_bytes, content: BTreeMap::new(), reasoning: BTreeMap::new(), tools: BTreeMap::new(), meta: json!({}), finish: BTreeMap::new(), completion_id: None }
    }

    fn chunk(&self, index: u64, delta: Value, finish: Value) -> Value {
        let mut c = self.meta.clone();
        c["choices"] = json!([{ "index": index, "delta": delta, "finish_reason": finish }]);
        c
    }

    /// Feed one upstream chunk; returns processed chunks to emit.
    pub fn on_chunk(&mut self, v: &Value) -> Result<Vec<Value>, OutputBlocked> {
        let mut out = Vec::new();
        for k in ["id", "object", "created", "model", "system_fingerprint", "service_tier"] {
            if let Some(x) = v.get(k) {
                self.meta[k] = x.clone();
            }
        }
        if self.completion_id.is_none() {
            self.completion_id = v.get("id").and_then(|i| i.as_str()).map(String::from);
        }
        let choices = v.get("choices").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        if choices.is_empty() {
            // usage-only or keep-alive chunk: no model text, pass metadata through.
            if v.get("usage").is_some_and(|u| !u.is_null()) {
                let mut c = self.meta.clone();
                c["choices"] = json!([]);
                c["usage"] = v["usage"].clone();
                out.push(c);
            }
            return Ok(out);
        }
        for ch in choices {
            let idx = ch["index"].as_u64().unwrap_or(0);
            let delta = &ch["delta"];
            let mut emit = serde_json::Map::new();
            if let Some(r) = delta.get("role") {
                emit.insert("role".into(), r.clone());
            }
            if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
                let max = self.max_bytes;
                let g = self.content.entry(idx).or_insert_with(|| TextGuard::new(self.ctx.clone(), max));
                let rel = g.push(t)?;
                if !rel.is_empty() {
                    emit.insert("content".into(), Value::String(rel));
                }
            }
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = delta.get(key).and_then(|c| c.as_str()) {
                    let max = self.max_bytes;
                    let g = self.reasoning.entry(idx).or_insert_with(|| TextGuard::new(self.ctx.clone(), max));
                    let rel = g.push(t)?;
                    if !rel.is_empty() {
                        emit.insert(key.into(), Value::String(rel));
                    }
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                for c in calls {
                    let ti = c["index"].as_u64().unwrap_or(0);
                    let b = self.tools.entry((idx, ti)).or_insert(ToolBuf { id: None, name: String::new(), args: String::new(), typ: None });
                    if let Some(id) = c["id"].as_str() {
                        b.id = Some(id.into());
                    }
                    if let Some(t) = c["type"].as_str() {
                        b.typ = Some(t.into());
                    }
                    if let Some(n) = c["function"]["name"].as_str() {
                        b.name.push_str(n);
                    }
                    if let Some(a) = c["function"]["arguments"].as_str() {
                        b.args.push_str(a);
                        if b.args.len() > self.max_bytes {
                            return Err(OutputBlocked { categories: vec!["OUTPUT_TOO_LARGE".into()] });
                        }
                    }
                }
            }
            if !ch["finish_reason"].is_null() {
                self.finish.insert(idx, ch["finish_reason"].clone());
            }
            if !emit.is_empty() {
                out.push(self.chunk(idx, Value::Object(emit), Value::Null));
            }
        }
        Ok(out)
    }

    /// Flush held text and buffered tool calls, then finish reasons.
    pub async fn finish(&mut self) -> Result<Vec<Value>, OutputBlocked> {
        let mut out = Vec::new();
        let idxs: Vec<u64> = self.content.keys().chain(self.reasoning.keys()).chain(self.tools.keys().map(|(i, _)| i)).chain(self.finish.keys()).copied().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        for idx in idxs {
            if let Some(g) = self.reasoning.get_mut(&idx) {
                let t = g.finish().await?;
                if !t.is_empty() {
                    out.push(self.chunk(idx, json!({"reasoning_content": t}), Value::Null));
                }
            }
            if let Some(g) = self.content.get_mut(&idx) {
                let t = g.finish().await?;
                if !t.is_empty() {
                    out.push(self.chunk(idx, json!({"content": t}), Value::Null));
                }
            }
            let keys: Vec<(u64, u64)> = self.tools.keys().filter(|(i, _)| *i == idx).copied().collect();
            let mut calls = Vec::new();
            for k in keys {
                let b = self.tools.remove(&k).unwrap();
                let mut st = OutStats::default();
                let args = self.ctx.process_full(&[b.args], &mut st).await?.pop().unwrap_or_default();
                calls.push(json!({"index": k.1, "id": b.id, "type": b.typ.unwrap_or_else(|| "function".into()), "function": {"name": b.name, "arguments": args}}));
            }
            if !calls.is_empty() {
                out.push(self.chunk(idx, json!({"tool_calls": calls}), Value::Null));
            }
            let fr = self.finish.get(&idx).cloned().unwrap_or(Value::String("stop".into()));
            out.push(self.chunk(idx, json!({}), fr));
        }
        Ok(out)
    }

    pub fn stats(&self) -> OutStats {
        let mut s = OutStats::default();
        for g in self.content.values().chain(self.reasoning.values()) {
            s.findings += g.stats.findings;
            s.redacted += g.stats.redacted;
            s.restored += g.stats.restored;
            for c in &g.stats.categories {
                if !s.categories.contains(c) {
                    s.categories.push(c.clone());
                }
            }
        }
        s
    }
}

// ------------------------------------------------------------------ Responses API

/// Processes upstream Responses events into inspected events.
pub struct ResponsesStream {
    ctx: Arc<OutCtx>,
    max_bytes: usize,
    texts: BTreeMap<String, TextGuard>,
    released: BTreeMap<String, String>,
    args: BTreeMap<String, String>,
    pub response_id: Option<String>,
    stats: OutStats,
}

fn text_key(v: &Value) -> String {
    format!("{}:{}", v["item_id"].as_str().unwrap_or(""), v["content_index"].as_u64().unwrap_or(0))
}

impl ResponsesStream {
    pub fn new(ctx: Arc<OutCtx>, max_bytes: usize) -> Self {
        ResponsesStream { ctx, max_bytes, texts: BTreeMap::new(), released: BTreeMap::new(), args: BTreeMap::new(), response_id: None, stats: OutStats::default() }
    }

    fn ev(ty: &str, v: &Value) -> String {
        format!("event: {ty}\ndata: {v}\n\n")
    }

    /// Generic full-object inspection for events not handled incrementally.
    async fn inspect_object(&mut self, v: &mut Value) -> Result<(), OutputBlocked> {
        let ex = crate::fields::extract(v, &["response", "item", "part", "output", "text", "delta", "arguments", "summary"]);
        if ex.fields.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = ex.fields.iter().map(|f| f.text.clone()).collect();
        let mut st = OutStats::default();
        let out = self.ctx.process_full(&texts, &mut st).await?;
        let mut fields = ex.fields;
        for (f, t) in fields.iter_mut().zip(out) {
            f.text = t;
        }
        crate::fields::write_back(v, &fields);
        self.stats.findings += st.findings;
        self.stats.redacted += st.redacted;
        Ok(())
    }

    pub async fn on_event(&mut self, ev: &SseEvent) -> Result<String, OutputBlocked> {
        let Ok(mut v) = serde_json::from_str::<Value>(&ev.data) else {
            // Non-JSON data cannot be inspected: drop it.
            return Ok(String::new());
        };
        let ty = ev.event.clone().or_else(|| v["type"].as_str().map(String::from)).unwrap_or_default();
        if self.response_id.is_none() {
            self.response_id = v["response"]["id"].as_str().map(String::from);
        }
        match ty.as_str() {
            "response.output_text.delta" | "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" | "response.refusal.delta" => {
                let key = format!("{ty}:{}", text_key(&v));
                let max = self.max_bytes;
                let g = self.texts.entry(key.clone()).or_insert_with(|| TextGuard::new(self.ctx.clone(), max));
                let rel = g.push(v["delta"].as_str().unwrap_or(""))?;
                if rel.is_empty() {
                    return Ok(String::new());
                }
                self.released.entry(key).or_default().push_str(&rel);
                v["delta"] = Value::String(rel);
                Ok(Self::ev(&ty, &v))
            }
            "response.output_text.done" | "response.reasoning_summary_text.done" | "response.reasoning_text.done" | "response.refusal.done" => {
                let delta_ty = ty.replace(".done", ".delta");
                let key = format!("{delta_ty}:{}", text_key(&v));
                let mut out = String::new();
                if let Some(mut g) = self.texts.remove(&key) {
                    let rel = g.finish().await?;
                    self.merge_stats(&g.stats);
                    if !rel.is_empty() {
                        self.released.entry(key.clone()).or_default().push_str(&rel);
                        let mut d = v.clone();
                        d["type"] = Value::String(delta_ty.clone());
                        d["delta"] = Value::String(rel);
                        for k in ["text", "refusal"] {
                            if let Some(o) = d.as_object_mut() {
                                o.remove(k);
                            }
                        }
                        out += &Self::ev(&delta_ty, &d);
                    }
                }
                let full = self.released.get(&key).cloned().unwrap_or_default();
                if v.get("text").is_some() {
                    v["text"] = Value::String(full);
                } else if v.get("refusal").is_some() {
                    v["refusal"] = Value::String(full);
                }
                out += &Self::ev(&ty, &v);
                Ok(out)
            }
            "response.function_call_arguments.delta" => {
                let key = v["item_id"].as_str().unwrap_or("").to_string();
                let e = self.args.entry(key).or_default();
                e.push_str(v["delta"].as_str().unwrap_or(""));
                if e.len() > self.max_bytes {
                    return Err(OutputBlocked { categories: vec!["OUTPUT_TOO_LARGE".into()] });
                }
                Ok(String::new())
            }
            "response.function_call_arguments.done" => {
                let key = v["item_id"].as_str().unwrap_or("").to_string();
                let raw = v["arguments"].as_str().map(String::from).unwrap_or_else(|| self.args.remove(&key).unwrap_or_default());
                self.args.remove(&key);
                let mut st = OutStats::default();
                let args = self.ctx.process_full(&[raw], &mut st).await?.pop().unwrap_or_default();
                let mut d = v.clone();
                d["type"] = Value::String("response.function_call_arguments.delta".into());
                d["delta"] = Value::String(args.clone());
                if let Some(o) = d.as_object_mut() {
                    o.remove("arguments");
                }
                v["arguments"] = Value::String(args);
                Ok(Self::ev("response.function_call_arguments.delta", &d) + &Self::ev(&ty, &v))
            }
            "response.created" | "response.in_progress" | "response.output_item.added" | "response.content_part.added" => {
                // No generated text yet beyond what later events carry; still inspect.
                self.inspect_object(&mut v).await?;
                Ok(Self::ev(&ty, &v))
            }
            _ => {
                self.inspect_object(&mut v).await?;
                Ok(Self::ev(&ty, &v))
            }
        }
    }

    fn merge_stats(&mut self, s: &OutStats) {
        self.stats.findings += s.findings;
        self.stats.redacted += s.redacted;
        self.stats.restored += s.restored;
        for c in &s.categories {
            if !self.stats.categories.contains(c) {
                self.stats.categories.push(c.clone());
            }
        }
    }

    pub async fn finish(&mut self) -> Result<String, OutputBlocked> {
        let mut out = String::new();
        let keys: Vec<String> = self.texts.keys().cloned().collect();
        for k in keys {
            let mut g = self.texts.remove(&k).unwrap();
            let rel = g.finish().await?;
            self.merge_stats(&g.stats);
            if !rel.is_empty() {
                let (ty, rest) = k.split_once(':').unwrap_or(("response.output_text.delta", ""));
                let (item, ci) = rest.rsplit_once(':').unwrap_or((rest, "0"));
                out += &Self::ev(ty, &json!({"type": ty, "item_id": item, "content_index": ci.parse::<u64>().unwrap_or(0), "delta": rel}));
            }
        }
        Ok(out)
    }

    pub fn stats(&self) -> OutStats {
        self.stats.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_parse_split_utf8_and_crlf() {
        let mut p = SseParser::default();
        let bytes = "event: x\r\ndata: {\"a\":\"é\"}\r\n\r\ndata: [DONE]\n\n".as_bytes();
        let (a, b) = bytes.split_at(20); // splits inside é
        let mut evs = p.push(a).unwrap();
        evs.extend(p.push(b).unwrap());
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event.as_deref(), Some("x"));
        assert_eq!(evs[0].data, "{\"a\":\"é\"}");
        assert_eq!(evs[1].data, "[DONE]");
    }

    #[test]
    fn sse_too_large() {
        let mut p = SseParser::default();
        let big = "data: ".to_string() + &"x".repeat(MAX_EVENT_BYTES + 10);
        assert_eq!(p.push(big.as_bytes()), Err(SseError::TooLarge));
    }
}
