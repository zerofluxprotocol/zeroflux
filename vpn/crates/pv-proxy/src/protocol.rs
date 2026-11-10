//! Translation between the Responses API and Chat Completions, used when a
//! destination lacks `/v1/responses` or when a private route must use Chat
//! (NEAR field E2EE covers Chat only).

use serde_json::{json, Map, Value};

fn text_of_parts(content: &Value) -> Value {
    match content {
        Value::String(s) => Value::String(s.clone()),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for p in parts {
                let t = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match t {
                    "input_text" | "output_text" | "text" => out.push(json!({"type": "text", "text": p.get("text").cloned().unwrap_or(Value::String(String::new()))})),
                    "refusal" => out.push(json!({"type": "text", "text": p.get("refusal").cloned().unwrap_or_default()})),
                    _ => out.push(p.clone()), // media parts stay; inspection already handled them
                }
            }
            if out.iter().all(|p| p["type"] == "text") {
                Value::String(out.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"))
            } else {
                Value::Array(out)
            }
        }
        _ => Value::String(String::new()),
    }
}

/// Convert a Responses request into a Chat Completions request.
pub fn responses_to_chat(req: &Value) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(i) = req.get("instructions").and_then(|v| v.as_str()) {
        messages.push(json!({"role": "system", "content": i}));
    }
    match req.get("input") {
        Some(Value::String(s)) => messages.push(json!({"role": "user", "content": s})),
        Some(Value::Array(items)) => {
            for it in items {
                let t = it.get("type").and_then(|t| t.as_str()).unwrap_or("message");
                match t {
                    "message" => {
                        let role = it.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                        let role = if role == "developer" { "system" } else { role };
                        messages.push(json!({"role": role, "content": text_of_parts(it.get("content").unwrap_or(&Value::Null))}));
                    }
                    "function_call" => {
                        let call = json!({"id": it.get("call_id").cloned().unwrap_or_default(), "type": "function", "function": {"name": it.get("name").cloned().unwrap_or_default(), "arguments": it.get("arguments").cloned().unwrap_or(Value::String("{}".into()))}});
                        // Merge consecutive calls into one assistant message.
                        if let Some(last) = messages.last_mut() {
                            if last["role"] == "assistant" && last.get("tool_calls").is_some() {
                                last["tool_calls"].as_array_mut().unwrap().push(call);
                                continue;
                            }
                        }
                        messages.push(json!({"role": "assistant", "content": Value::Null, "tool_calls": [call]}));
                    }
                    "function_call_output" => {
                        let out = match it.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => v.to_string(),
                            None => String::new(),
                        };
                        messages.push(json!({"role": "tool", "tool_call_id": it.get("call_id").cloned().unwrap_or_default(), "content": out}));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let mut out = Map::new();
    out.insert("model".into(), req.get("model").cloned().unwrap_or_default());
    out.insert("messages".into(), Value::Array(messages));
    if let Some(tools) = req.get("tools").and_then(|t| t.as_array()) {
        let mapped: Vec<Value> = tools
            .iter()
            .filter(|t| t.get("type").and_then(|x| x.as_str()) == Some("function"))
            .map(|t| json!({"type": "function", "function": {"name": t.get("name").cloned().unwrap_or_default(), "description": t.get("description").cloned().unwrap_or_default(), "parameters": t.get("parameters").cloned().unwrap_or(json!({"type":"object"}))}}))
            .collect();
        if !mapped.is_empty() {
            out.insert("tools".into(), Value::Array(mapped));
        }
    }
    if let Some(tc) = req.get("tool_choice") {
        let v = match tc {
            Value::Object(o) if o.get("type").and_then(|x| x.as_str()) == Some("function") => json!({"type": "function", "function": {"name": o.get("name").cloned().unwrap_or_default()}}),
            other => other.clone(),
        };
        out.insert("tool_choice".into(), v);
    }
    for (from, to) in [("max_output_tokens", "max_tokens"), ("temperature", "temperature"), ("top_p", "top_p"), ("stream", "stream"), ("parallel_tool_calls", "parallel_tool_calls")] {
        if let Some(v) = req.get(from) {
            out.insert(to.into(), v.clone());
        }
    }
    if req.get("stream").and_then(|s| s.as_bool()) == Some(true) {
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }
    Value::Object(out)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Convert a Chat Completion into a Responses object.
pub fn chat_to_responses(chat: &Value, model_fallback: &str) -> Value {
    let id = format!("resp_{}", chat.get("id").and_then(|v| v.as_str()).unwrap_or("pv").trim_start_matches("chatcmpl-"));
    let msg = &chat["choices"][0]["message"];
    let mut output = Vec::new();
    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
    if !text.is_empty() || msg.get("tool_calls").is_none() {
        output.push(json!({"type": "message", "id": format!("msg_{}", &id[5..]), "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": text, "annotations": []}]}));
    }
    if let Some(calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for c in calls {
            output.push(json!({"type": "function_call", "id": format!("fc_{}", c["id"].as_str().unwrap_or("")), "call_id": c["id"], "name": c["function"]["name"], "arguments": c["function"]["arguments"], "status": "completed"}));
        }
    }
    let usage = &chat["usage"];
    json!({
        "id": id,
        "object": "response",
        "created_at": chat.get("created").cloned().unwrap_or(json!(now())),
        "status": "completed",
        "model": chat.get("model").cloned().unwrap_or(Value::String(model_fallback.into())),
        "output": output,
        "output_text": text,
        "usage": {
            "input_tokens": usage["prompt_tokens"].as_u64().unwrap_or(0),
            "output_tokens": usage["completion_tokens"].as_u64().unwrap_or(0),
            "total_tokens": usage["total_tokens"].as_u64().unwrap_or(0)
        }
    })
}

/// Incremental converter from processed Chat SSE chunks to Responses SSE events.
pub struct ChatToResponsesStream {
    id: String,
    model: String,
    seq: u64,
    started: bool,
    text_open: bool,
    text: String,
    calls: Vec<(String, String, String)>, // call_id, name, arguments
    usage: Value,
}

impl ChatToResponsesStream {
    pub fn new(model: &str) -> Self {
        ChatToResponsesStream { id: format!("resp_{}", uuid::Uuid::new_v4().simple()), model: model.into(), seq: 0, started: false, text_open: false, text: String::new(), calls: vec![], usage: json!({}) }
    }

    fn ev(&mut self, ty: &str, mut data: Value) -> String {
        data["type"] = Value::String(ty.into());
        data["sequence_number"] = json!(self.seq);
        self.seq += 1;
        format!("event: {ty}\ndata: {data}\n\n")
    }

    fn base(&self, status: &str) -> Value {
        json!({"id": self.id, "object": "response", "created_at": now(), "status": status, "model": self.model, "output": []})
    }

    fn msg_id(&self) -> String {
        format!("msg_{}", &self.id[5..])
    }

    /// Feed one processed chat chunk; returns SSE text to emit.
    pub fn push(&mut self, chunk: &Value) -> String {
        let mut out = String::new();
        if !self.started {
            self.started = true;
            let b = self.base("in_progress");
            out += &self.ev("response.created", json!({"response": b}));
            let b = self.base("in_progress");
            out += &self.ev("response.in_progress", json!({"response": b}));
        }
        if let Some(u) = chunk.get("usage") {
            if !u.is_null() {
                self.usage = u.clone();
            }
        }
        let delta = &chunk["choices"][0]["delta"];
        if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
            if !t.is_empty() {
                if !self.text_open {
                    self.text_open = true;
                    let mid = self.msg_id();
                    out += &self.ev("response.output_item.added", json!({"output_index": 0, "item": {"type": "message", "id": mid, "status": "in_progress", "role": "assistant", "content": []}}));
                    out += &self.ev("response.content_part.added", json!({"item_id": mid, "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}}));
                }
                self.text.push_str(t);
                let mid = self.msg_id();
                out += &self.ev("response.output_text.delta", json!({"item_id": mid, "output_index": 0, "content_index": 0, "delta": t}));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
            for c in calls {
                let idx = c["index"].as_u64().unwrap_or(0) as usize;
                while self.calls.len() <= idx {
                    self.calls.push((String::new(), String::new(), String::new()));
                }
                if let Some(id) = c["id"].as_str() {
                    self.calls[idx].0 = id.into();
                }
                if let Some(n) = c["function"]["name"].as_str() {
                    self.calls[idx].1.push_str(n);
                }
                if let Some(a) = c["function"]["arguments"].as_str() {
                    self.calls[idx].2.push_str(a);
                }
            }
        }
        out
    }

    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        if !self.started {
            out += &self.push(&json!({}));
        }
        let mut output = Vec::new();
        let mut oi = 0;
        if self.text_open {
            let mid = self.msg_id();
            let text = self.text.clone();
            out += &self.ev("response.output_text.done", json!({"item_id": mid, "output_index": 0, "content_index": 0, "text": text}));
            out += &self.ev("response.content_part.done", json!({"item_id": mid, "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": text, "annotations": []}}));
            let item = json!({"type": "message", "id": mid, "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": text, "annotations": []}]});
            out += &self.ev("response.output_item.done", json!({"output_index": 0, "item": item}));
            output.push(item);
            oi = 1;
        }
        for (i, (call_id, name, args)) in self.calls.clone().into_iter().enumerate() {
            let item = json!({"type": "function_call", "id": format!("fc_{call_id}"), "call_id": call_id, "name": name, "arguments": args, "status": "completed"});
            let idx = oi + i;
            out += &self.ev("response.output_item.added", json!({"output_index": idx, "item": {"type": "function_call", "id": format!("fc_{call_id}"), "call_id": call_id, "name": name, "arguments": "", "status": "in_progress"}}));
            out += &self.ev("response.function_call_arguments.delta", json!({"item_id": format!("fc_{call_id}"), "output_index": idx, "delta": args}));
            out += &self.ev("response.function_call_arguments.done", json!({"item_id": format!("fc_{call_id}"), "output_index": idx, "arguments": args}));
            out += &self.ev("response.output_item.done", json!({"output_index": idx, "item": item}));
            output.push(item);
        }
        let mut b = self.base("completed");
        b["output"] = Value::Array(output);
        b["usage"] = json!({"input_tokens": self.usage["prompt_tokens"].as_u64().unwrap_or(0), "output_tokens": self.usage["completion_tokens"].as_u64().unwrap_or(0), "total_tokens": self.usage["total_tokens"].as_u64().unwrap_or(0)});
        out += &self.ev("response.completed", json!({"response": b}));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_translation() {
        let r = json!({"model":"m","instructions":"sys","input":[
            {"role":"user","content":[{"type":"input_text","text":"hello"}]},
            {"type":"function_call","call_id":"c1","name":"f","arguments":"{}"},
            {"type":"function_call_output","call_id":"c1","output":"done"}],
            "tools":[{"type":"function","name":"f","description":"d","parameters":{"type":"object"}}],
            "max_output_tokens": 50, "stream": true});
        let c = responses_to_chat(&r);
        assert_eq!(c["messages"][0]["role"], "system");
        assert_eq!(c["messages"][1]["content"], "hello");
        assert_eq!(c["messages"][2]["tool_calls"][0]["function"]["name"], "f");
        assert_eq!(c["messages"][3]["role"], "tool");
        assert_eq!(c["tools"][0]["function"]["name"], "f");
        assert_eq!(c["max_tokens"], 50);
        assert_eq!(c["stream_options"]["include_usage"], true);
    }

    #[test]
    fn response_translation() {
        let chat = json!({"id":"chatcmpl-1","model":"m","choices":[{"message":{"role":"assistant","content":"hi","tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments":"{}"}}]}}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}});
        let r = chat_to_responses(&chat, "m");
        assert_eq!(r["output"][0]["content"][0]["text"], "hi");
        assert_eq!(r["output"][1]["type"], "function_call");
        assert_eq!(r["usage"]["total_tokens"], 4);
        assert_eq!(r["output_text"], "hi");
    }

    #[test]
    fn stream_translation() {
        let mut s = ChatToResponsesStream::new("m");
        let mut out = s.push(&json!({"choices":[{"delta":{"content":"He"}}]}));
        out += &s.push(&json!({"choices":[{"delta":{"content":"llo"}}]}));
        out += &s.finish();
        assert!(out.contains("response.created"));
        assert!(out.contains("\"delta\":\"He\""));
        assert!(out.contains("\"text\":\"Hello\""));
        assert!(out.trim_end().ends_with('}'));
        assert!(out.contains("event: response.completed"));
    }
}
