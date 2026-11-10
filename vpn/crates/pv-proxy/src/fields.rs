//! Locate every content-bearing string in a request or response body as a JSON
//! pointer, so detection and transformation work on exact local buffers and the
//! result is written back without re-serializing anything else.

use pv_core::category::UNSUPPORTED_CONTENT;
use pv_core::{ClassifierType, Finding, Sensitivity};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub pointer: String,
    pub text: String,
}

/// Content part types that carry media the proxy cannot inspect.
const UNSUPPORTED_PARTS: &[&str] = &["image_url", "input_image", "input_audio", "input_file", "file", "image", "audio", "video", "document"];

/// Keys whose string values are protocol metadata, not content.
const SKIP_KEYS: &[&str] = &["role", "type", "id", "tool_call_id", "call_id", "detail", "format", "status", "object", "finish_reason", "index", "item_id", "response_id", "previous_response_id", "logprobs"];

fn escape(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

fn walk(v: &Value, ptr: &str, out: &mut Vec<Field>, unsupported: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(Field { pointer: ptr.to_string(), text: s.clone() }),
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                walk(x, &format!("{ptr}/{i}"), out, unsupported);
            }
        }
        Value::Object(o) => {
            if let Some(t) = o.get("type").and_then(|t| t.as_str()) {
                if UNSUPPORTED_PARTS.contains(&t) {
                    unsupported.push(ptr.to_string());
                    return;
                }
            }
            for (k, x) in o {
                if SKIP_KEYS.contains(&k.as_str()) {
                    continue;
                }
                walk(x, &format!("{ptr}/{}", escape(k)), out, unsupported);
            }
        }
        _ => {}
    }
}

pub struct Extracted {
    pub fields: Vec<Field>,
    /// Pointers of media parts that cannot be inspected.
    pub unsupported: Vec<String>,
}

/// Content subtrees by protocol.
pub const CHAT_REQUEST_ROOTS: &[&str] = &["messages", "tools", "functions", "metadata", "user", "prompt", "response_format"];
pub const RESPONSES_REQUEST_ROOTS: &[&str] = &["input", "instructions", "tools", "metadata", "user", "text", "prompt"];
pub const CHAT_RESPONSE_ROOTS: &[&str] = &["choices"];
pub const RESPONSES_RESPONSE_ROOTS: &[&str] = &["output", "output_text"];

pub fn extract(body: &Value, roots: &[&str]) -> Extracted {
    let mut fields = Vec::new();
    let mut unsupported = Vec::new();
    for r in roots {
        if let Some(v) = body.get(*r) {
            walk(v, &format!("/{}", escape(r)), &mut fields, &mut unsupported);
        }
    }
    // Drop empty strings: nothing to inspect.
    fields.retain(|f| !f.text.is_empty());
    Extracted { fields, unsupported }
}

/// Whole-field findings for each unsupported media part (index after text fields).
pub fn unsupported_findings(n_fields: usize, unsupported: &[String]) -> Vec<Finding> {
    unsupported
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let mut f = Finding::new(ClassifierType::Builtin, UNSUPPORTED_CONTENT, 0, 0, 1.0, "content-type-v1", "uninspectable media part", Sensitivity::High);
            f.field_index = n_fields + i;
            f.whole_field = true;
            f
        })
        .collect()
}

/// Write transformed texts back into the body.
pub fn write_back(body: &mut Value, fields: &[Field]) {
    for f in fields {
        if let Some(slot) = body.pointer_mut(&f.pointer) {
            *slot = Value::String(f.text.clone());
        }
    }
}

/// Remove media parts (used only when policy explicitly allows dropping them; not default).
pub fn texts(fields: &[Field]) -> Vec<String> {
    fields.iter().map(|f| f.text.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_fields_include_tool_args_and_skip_metadata_keys() {
        let body = json!({
            "model": "gpt-x",
            "messages": [
                {"role": "system", "content": "be nice"},
                {"role": "user", "content": [{"type": "text", "text": "hi jane@example.com"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}]},
                {"role": "assistant", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "send", "arguments": "{\"to\":\"bob@example.com\"}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "sent to 555-010-1234"}
            ],
            "tools": [{"type": "function", "function": {"name": "send", "description": "send mail", "parameters": {"type": "object"}}}]
        });
        let e = extract(&body, CHAT_REQUEST_ROOTS);
        let ptrs: Vec<&str> = e.fields.iter().map(|f| f.pointer.as_str()).collect();
        assert!(ptrs.contains(&"/messages/1/content/0/text"));
        assert!(ptrs.contains(&"/messages/2/tool_calls/0/function/arguments"));
        assert!(ptrs.contains(&"/messages/3/content"));
        assert!(ptrs.contains(&"/tools/0/function/description"));
        assert!(!ptrs.iter().any(|p| p.ends_with("/role") || p.ends_with("/tool_call_id")));
        assert_eq!(e.unsupported, vec!["/messages/1/content/1"]);
        let mut b2 = body.clone();
        let mut f = e.fields.clone();
        f[1].text = "hi [[PV_EMAIL_00000000]]".into();
        write_back(&mut b2, &f);
        assert_eq!(b2["messages"][1]["content"][0]["text"], "hi [[PV_EMAIL_00000000]]");
        assert_eq!(b2["model"], "gpt-x");
    }

    #[test]
    fn responses_fields() {
        let body = json!({"model":"m","instructions":"sys","input":[{"role":"user","content":[{"type":"input_text","text":"x"}]},{"type":"function_call_output","call_id":"c","output":"ok"}]});
        let e = extract(&body, RESPONSES_REQUEST_ROOTS);
        assert_eq!(e.fields.len(), 3);
        let s = json!({"input":"plain string"});
        assert_eq!(extract(&s, RESPONSES_REQUEST_ROOTS).fields[0].pointer, "/input");
    }
}
