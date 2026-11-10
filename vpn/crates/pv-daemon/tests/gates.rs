//! Phase gates: synthetic SSN never reaches upstream; tokenized email is
//! restored; events omit values. Plus chat/responses protocol coverage.

mod common;
use common::*;
use serde_json::json;

#[tokio::test]
async fn ssn_never_reaches_upstream_and_events_omit_values() {
    let h = harness("mock-echo").await;
    let (s, v) = h.chat(user("Please update my file. SSN: 123-45-6789")).await;
    assert_eq!(s, 403, "{v}");
    assert_eq!(v["error"]["code"], "PV_POLICY_BLOCKED");
    assert_eq!(v["error"]["type"], "promptvpn_policy_block");
    assert!(!v.to_string().contains("123-45-6789"));
    assert!(v["error"]["event_id"].is_string());
    assert_eq!(h.rec.count(), 0, "nothing may reach the upstream");
    let ev = h.events_text().await;
    assert!(ev.contains("GOVERNMENT_ID"));
    assert!(!ev.contains("123-45-6789"));
    assert!(!h.disk_text().contains("123-45-6789"), "no plaintext SSN on disk");
    assert!(h.notes.sent.lock().unwrap().iter().any(|n| n.kind == "block" && !n.body.contains("123-45")));
}

#[tokio::test]
async fn email_tokenized_upstream_and_restored_for_caller() {
    let h = harness("mock-echo").await;
    let (s, v) = h.chat(user("Write to jane.doe@example.com about the invoice")).await;
    assert_eq!(s, 200, "{v}");
    let up = h.upstream_text();
    assert!(!up.contains("jane.doe@example.com"), "{up}");
    assert!(up.contains("[[PV_EMAIL_"));
    let content = v["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(content.contains("jane.doe@example.com"), "{content}");
    assert!(!content.contains("[[PV_"));
    let ev = h.events_text().await;
    assert!(ev.contains("TRANSFORM") && !ev.contains("jane.doe"));
    assert!(!h.disk_text().contains("jane.doe@example.com"));
}

#[tokio::test]
async fn same_value_same_token_within_conversation() {
    let h = harness("mock-echo").await;
    let body = json!({"model": "m", "messages": [{"role": "user", "content": "a@b.co then a@b.co"}]});
    let (s, _) = h.chat(body).await;
    assert_eq!(s, 200);
    let last = h.rec.last().unwrap();
    let t = last["messages"][0]["content"].as_str().unwrap();
    let toks: Vec<&str> = t.split_whitespace().filter(|w| w.starts_with("[[PV_EMAIL_")).collect();
    assert_eq!(toks.len(), 2);
    assert_eq!(toks[0], toks[1]);
}

#[tokio::test]
async fn streaming_chat_restores_and_never_leaks_upstream() {
    let h = harness("mock-echo").await;
    let mut b = user("contact me at stream.user@example.org please");
    b["stream"] = json!(true);
    let (s, sse) = h.stream("/v1/chat/completions", b).await;
    assert_eq!(s, 200);
    assert!(sse.trim_end().ends_with("data: [DONE]"), "{sse}");
    let text = sse_content(&sse);
    assert!(text.contains("stream.user@example.org"), "{text}");
    assert!(!sse.contains("[[PV_"), "no partial placeholders in stream: {sse}");
    assert!(!h.upstream_text().contains("stream.user@example.org"));
}

#[tokio::test]
async fn responses_api_nonstream_and_stream() {
    let h = harness("mock-echo").await;
    let (s, v) = h.responses(json!({"model": "m", "input": "my email is resp@example.com"})).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["output"][0]["content"][0]["text"].as_str().unwrap().contains("resp@example.com"));
    assert!(!h.upstream_text().contains("resp@example.com"));
    let (s, sse) = h.stream("/v1/responses", json!({"model": "m", "stream": true, "input": [{"role": "user", "content": [{"type": "input_text", "text": "mail rs@example.com"}]}]})).await;
    assert_eq!(s, 200);
    assert!(sse_resp_text(&sse).contains("rs@example.com"), "{sse}");
    assert!(sse.contains("response.completed"));
    assert!(!sse.contains("[[PV_"));
    let (s, v) = h.responses(json!({"model": "m", "input": "token sk-proj-AbCdEfGhIjKlMnOpQrStUvWx123456"})).await;
    assert_eq!(s, 403, "{v}");
}

#[tokio::test]
async fn responses_translated_to_chat_when_upstream_lacks_it() {
    let h = harness("mock-echo").await;
    pv_daemon::admin::put_provider_inner(&h.st, provider("mock", &h.mock_base, "mock-echo", pv_core::DestinationTier::ExternalUnverified, false, false), None).map_err(|_| ()).unwrap();
    let (s, v) = h.responses(json!({"model": "m", "instructions": "be brief", "input": "hello t@example.com"})).await;
    assert_eq!(s, 200, "{v}");
    let up = h.rec.last().unwrap();
    assert!(up.get("messages").is_some(), "translated to chat: {up}");
    assert_eq!(v["object"], "response");
    assert!(v["output_text"].as_str().unwrap().contains("t@example.com"));
    let (s, sse) = h.stream("/v1/responses", json!({"model": "m", "stream": true, "input": "hi s@example.com"})).await;
    assert_eq!(s, 200);
    assert!(sse_resp_text(&sse).contains("s@example.com"), "{sse}");
    assert!(sse.contains("event: response.completed"));
}

#[tokio::test]
async fn tool_call_arguments_and_tool_results_inspected() {
    let h = harness("mock-echo").await;
    let b = json!({"model": "m", "messages": [
        {"role": "user", "content": "deploy"},
        {"role": "assistant", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "set_env", "arguments": "{\"OPENAI_API_KEY\":\"sk-proj-AbCdEfGhIjKlMnOpQrStUvWx123456\"}"}}]},
        {"role": "tool", "tool_call_id": "c1", "content": "ok"}]});
    let (s, _) = h.chat(b).await;
    assert_eq!(s, 403);
    let b = json!({"model": "m", "messages": [{"role": "user", "content": "lookup"}, {"role": "tool", "tool_call_id": "c1", "content": "record: ssn 123-45-6789"}]});
    let (s, _) = h.chat(b).await;
    assert_eq!(s, 403);
    let b = json!({"model": "m", "messages": [{"role": "system", "content": "internal db password=Sup3rS3cret!"}, {"role": "user", "content": "hi"}]});
    let (s, _) = h.chat(b).await;
    assert_eq!(s, 403, "secrets in system prompts are inspected");
    assert_eq!(h.rec.count(), 0);
}

#[tokio::test]
async fn model_output_critical_values_redacted_nonstream_and_stream() {
    let h = harness("mock-leak").await;
    let (s, v) = h.chat(user("tell me")).await;
    assert_eq!(s, 200, "{v}");
    let c = v["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(!c.contains("123-45-6789") && !c.contains("sk-proj-LEAKED"), "{c}");
    assert!(c.contains("[REDACTED_GOVERNMENT_ID]"));
    let mut b = user("tell me");
    b["stream"] = json!(true);
    let (_, sse) = h.stream("/v1/chat/completions", b).await;
    assert!(!sse.contains("123-45-6789") && !sse.contains("LEAKED"), "{sse}");
    assert!(sse_content(&sse).contains("[REDACTED_API_KEY]"));
}

#[tokio::test]
async fn output_block_policy_stops_stream() {
    let h = harness("mock-leak").await;
    // Owner policy: block critical data in output instead of redacting.
    let rule = json!({"id": "out-block", "version": 1, "scope": {"kind": "user", "id": "local-owner"}, "selector": {"categories": ["GOVERNMENT_ID"], "destinations": ["any"], "direction": "outbound"}, "action": "BLOCK", "severity": "critical", "override": "POLICY_OWNER_ONLY"});
    let (s, v) = h.admin_post("/policies", rule).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = h.chat(user("tell me")).await;
    assert_eq!(s, 403);
    assert_eq!(v["error"]["code"], "PV_OUTPUT_BLOCKED");
    let mut b = user("tell me");
    b["stream"] = json!(true);
    let (_, sse) = h.stream("/v1/chat/completions", b).await;
    assert!(sse.contains("PV_OUTPUT_BLOCKED"), "{sse}");
    assert!(!sse.contains("123-45-6789"));
}

#[tokio::test]
async fn tool_call_output_restored_in_stream() {
    let h = harness("mock-tool").await;
    let mut b = user("send to tool.user@example.com");
    b["stream"] = json!(true);
    let (_, sse) = h.stream("/v1/chat/completions", b).await;
    let mut args = String::new();
    for l in sse.lines() {
        if let Some(d) = l.strip_prefix("data: ") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(d) {
                if let Some(a) = v["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str() {
                    args.push_str(a);
                }
            }
        }
    }
    assert!(args.contains("tool.user@example.com"), "{sse}");
    assert!(!h.upstream_text().contains("tool.user@example.com"));
}

#[tokio::test]
async fn unsupported_media_blocked_for_remote() {
    let h = harness("mock-echo").await;
    let b = json!({"model": "m", "messages": [{"role": "user", "content": [{"type": "text", "text": "what is this"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}}]}]});
    let (s, v) = h.chat(b).await;
    assert_eq!(s, 403, "{v}");
    assert!(v["error"]["categories"].to_string().contains("UNSUPPORTED_CONTENT"));
}

#[tokio::test]
async fn bad_requests() {
    let h = harness("mock-echo").await;
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).header("content-type", "application/json").body("{nope").send().await.unwrap();
    assert_eq!(r.status(), 400);
    let big = "a".repeat(5 << 20);
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).json(&user(&big)).send().await.unwrap();
    assert_eq!(r.status(), 413);
    let r = h.http.post(format!("{}/v1/embeddings", h.base)).bearer_auth(&h.key).json(&json!({})).send().await.unwrap();
    assert_eq!(r.status(), 415);
}

#[tokio::test]
async fn health_reports_near_not_configured_and_no_secrets() {
    let h = harness("mock-echo").await;
    let (s, v) = h.admin_get("/health").await;
    assert_eq!(s, 200);
    assert_eq!(v["near"]["status"], "NEAR_NOT_CONFIGURED");
    assert!(v["coverage"].as_str().unwrap().contains("does not intercept"));
    let r = h.http.get(format!("{}/healthz", h.base)).send().await.unwrap();
    let pub_health: serde_json::Value = r.json().await.unwrap();
    assert_eq!(pub_health.as_object().unwrap().len(), 3);
    assert!(!pub_health.to_string().contains(&h.admin));
}

#[tokio::test]
async fn custom_entity_types_tokenized_and_restored() {
    let h = harness("mock-echo").await;
    // Deterministic stand-in for GLiNER: finds "<label>: <value>".
    h.st.engine.detector.set_span(Some(std::sync::Arc::new(pv_detectors::span::KeywordSpans { builtin: vec![], fail: false })));
    h.st.engine.detector.set_settings(pv_detectors::DetectorSettings { span_enabled: true, span_required: true, span_timeout_ms: 2000 });
    let (s, v) = h.admin_post("/entities", json!({"label": "Policy Number"})).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = h.admin_post("/entities", json!({"label": "email address"})).await;
    assert_eq!(s, 400, "built-in labels rejected: {v}");
    let (s, v) = h.chat(user("renew policy number: PX-4471 before friday")).await;
    assert_eq!(s, 200, "{v}");
    assert!(!h.upstream_text().contains("PX-4471"));
    assert!(h.upstream_text().contains("[[PV_CUSTOM_"));
    assert!(v["choices"][0]["message"]["content"].as_str().unwrap().contains("PX-4471"));
    // Per-entity action: block.
    let (_, l) = h.admin_get("/entities").await;
    let id = l["entities"][0]["entity"]["id"].as_str().unwrap().to_string();
    h.http.delete(format!("{}/api/admin/v1/entities/{id}", h.base)).bearer_auth(&h.admin).send().await.unwrap();
    h.admin_post("/entities", json!({"label": "employee id", "action": "BLOCK"})).await;
    let (s, _) = h.chat(user("employee id: E12345 needs access")).await;
    assert_eq!(s, 403);
    assert!(!h.disk_text().contains("PX-4471"));
}

#[tokio::test]
async fn entities_without_model_fail_closed() {
    let h = harness("mock-echo").await;
    h.st.engine.detector.set_settings(pv_detectors::DetectorSettings { span_enabled: true, span_required: false, span_timeout_ms: 2000 });
    h.admin_post("/entities", json!({"label": "policy number"})).await;
    let (s, v) = h.chat(user("policy number: PX-1")).await;
    assert_eq!(s, 428, "{v}");
    assert_eq!(h.rec.count(), 0);
}

#[tokio::test]
async fn onboarding_rejects_bad_input_before_writing() {
    let h = harness("mock-echo").await;
    let before = h.st.engine.policy_hash();
    let (s, _) = h.admin_post("/onboarding/complete", json!({
        "categories": {"EMAIL": "REDACT"}, "fallback_mode": "BLOCK_AND_REVIEW", "near": {"skip": true},
        "entities": [{"label": "employee id"}, {"label": "email address"}]
    })).await;
    assert_eq!(s, 400);
    assert_eq!(h.st.engine.policy_hash(), before, "nothing applied");
    let (_, e) = h.admin_get("/entities").await;
    assert_eq!(e["entities"].as_array().unwrap().len(), 0);
    assert!(e["builtin_labels"].as_array().unwrap().iter().any(|l| l == "email address"));
}
