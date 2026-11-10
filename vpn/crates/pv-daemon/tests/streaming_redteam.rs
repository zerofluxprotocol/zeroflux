//! Gates 2, 3 and 6 (spec 3.7) and the streaming red-team list (spec 3.4):
//! exact egress bytes never contain synthetic critical values; placeholders
//! restore faithfully in non-stream, SSE split at arbitrary byte boundaries
//! (including inside UTF-8 characters and JSON objects) and streamed tool-call
//! arguments; conversations and apps are isolated; fake placeholders, prefix
//! collisions, malformed chunks, mid-stream upstream errors, stalls and client
//! cancellation are handled without leaks or silent replays.

mod common;
use common::*;
use pv_core::*;
use serde_json::{json, Value};

const CRITICAL: &[&str] = &[
    "123-45-6789",
    "4111 1111 1111 1111",
    "AKIAIOSFODNN7EXAMPLE",
    "sk-proj-SyntheticKey0123456789abcdefABCDEF",
    "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789",
    "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----",
];

fn set(h: &H, f: impl FnOnce(&mut pv_proxy::Settings)) {
    let mut s = h.st.engine.settings();
    f(&mut s);
    pv_daemon::state::save_settings(&h.st, &s, "test").unwrap();
}

async fn model(h: &H, m: &str) {
    pv_daemon::admin::put_provider_inner(&h.st, provider("mock", &h.mock_base, m, DestinationTier::ExternalUnverified, false, true), None).map_err(|_| ()).unwrap();
}

fn tool_body(arg_value: &str) -> Value {
    json!({"model": "m", "messages": [
        {"role": "system", "content": "You are a deploy bot."},
        {"role": "user", "content": "deploy please"},
        {"role": "assistant", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "set_env", "arguments": json!({"VALUE": arg_value}).to_string()}}]},
        {"role": "tool", "tool_call_id": "c1", "content": format!("env now has {arg_value}")}
    ]})
}

#[tokio::test]
async fn exact_egress_bytes_never_contain_critical_values() {
    let h = harness("mock-echo").await;
    let mut sent = 0;
    for v in CRITICAL {
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, v);
        let bodies = vec![
            ("/v1/chat/completions", user(&format!("here it is: {v}"))),
            ("/v1/chat/completions", json!({"model": "m", "messages": [{"role": "system", "content": format!("secret context {v}")}, {"role": "user", "content": "hi"}]})),
            ("/v1/chat/completions", tool_body(v)),
            ("/v1/chat/completions", user(&format!("decode this {encoded}"))),
            ("/v1/responses", json!({"model": "m", "input": format!("value {v}")})),
            ("/v1/responses", json!({"model": "m", "input": [{"role": "user", "content": [{"type": "input_text", "text": format!("x {v}")}]}]})),
        ];
        for (path, mut b) in bodies {
            for stream in [false, true] {
                b["stream"] = json!(stream);
                let r = h.http.post(format!("{}{}", h.base, path)).bearer_auth(&h.key).json(&b).send().await.unwrap();
                let s = r.status().as_u16();
                let _ = r.text().await;
                if s == 200 {
                    sent += 1;
                    eprintln!("FORWARDED: {path} stream={stream} value={v:?} body={}", b.to_string().chars().take(200).collect::<String>());
                }
            }
        }
    }
    let egress = h.rec.all_raw();
    for v in CRITICAL {
        assert!(!egress.contains(v), "critical value reached the upstream: {v}");
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, v);
        assert!(!egress.contains(encoded.trim_end_matches('=')), "base64 of a critical value reached the upstream: {v}");
        for line in v.lines() {
            assert!(line.len() < 12 || !egress.contains(line), "part of a critical value reached the upstream: {line}");
        }
    }
    assert!(!egress.contains(&h.key), "client key reached the upstream");
    // Every one of these requests is blocked: nothing at all was sent.
    assert_eq!(sent, 0, "a request with a critical value was forwarded");
    assert_eq!(h.rec.count(), 0);
}

#[tokio::test]
async fn restoration_across_arbitrary_sse_byte_splits() {
    let h = harness("mock-echo").await;
    let text = "Write to zoë.ünïcode@example.org (é中😀) and phone 415-555-0172 today 用户@例子.广告";
    for n in [1usize, 2, 3, 5, 7, 64] {
        model(&h, &format!("mock-echo+split{n}")).await;
        let mut b = user(text);
        b["stream"] = json!(true);
        let (s, sse) = h.stream("/v1/chat/completions", b).await;
        assert_eq!(s, 200);
        assert_eq!(sse_content(&sse), format!("You said: {text}"), "split {n}");
        assert!(!sse.contains("[[ZFX_") && !sse.contains("[[PV_"), "no placeholder fragments (split {n})");
        assert!(sse.trim_end().ends_with("data: [DONE]"));
        let (s, sse) = h.stream("/v1/responses", json!({"model": "m", "stream": true, "input": text})).await;
        assert_eq!(s, 200);
        assert_eq!(sse_resp_text(&sse), format!("You said: {text}"), "responses split {n}");
        assert!(!sse.contains("[[ZFX_"));
    }
    let up = h.upstream_text();
    assert!(!up.contains("zoë.ünïcode@example.org") && !up.contains("415-555-0172") && !up.contains("用户@例子.广告"), "{up}");
}

#[tokio::test]
async fn tool_call_arguments_restored_stream_and_nonstream_with_splits() {
    let h = harness("mock-tool").await;
    let (s, v) = h.chat(user("mail tool.user@example.com now")).await;
    assert_eq!(s, 200, "{v}");
    let args = v["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"].as_str().unwrap();
    assert!(args.contains("tool.user@example.com"), "{args}");
    for n in [1usize, 3] {
        model(&h, &format!("mock-tool+split{n}")).await;
        let mut b = user("mail tool.user@example.com now");
        b["stream"] = json!(true);
        let (s, sse) = h.stream("/v1/chat/completions", b).await;
        assert_eq!(s, 200);
        let mut joined = String::new();
        for line in sse.lines() {
            if let Some(d) = line.strip_prefix("data: ") {
                if let Ok(v) = serde_json::from_str::<Value>(d) {
                    if let Some(a) = v["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str() {
                        joined.push_str(a);
                    }
                }
            }
        }
        let parsed: Value = serde_json::from_str(&joined).unwrap_or_else(|_| panic!("valid JSON args (split {n}): {joined}"));
        assert_eq!(parsed["to"], "mail tool.user@example.com now");
        assert!(!sse.contains("[[ZFX_"));
    }
    assert!(!h.upstream_text().contains("tool.user@example.com"));
}

#[tokio::test]
async fn fake_foreign_and_partial_placeholders_pass_through_unchanged() {
    let h = harness("mock-echo").await;
    let fixed = "fake [[ZFX_EMAIL_0123456789abcdef]] legacy [[PV_EMAIL_deadbeef]] partial [[ZFX_EMAIL_01 and arr[[ZFX_ end [[";
    model(&h, &format!("mock-fixed:{fixed}")).await;
    let (s, v) = h.chat(user("hello")).await;
    assert_eq!(s, 200);
    assert_eq!(v["choices"][0]["message"]["content"], fixed);
    model(&h, &format!("mock-fixed:{fixed}+split1")).await;
    let mut b = user("hello");
    b["stream"] = json!(true);
    let (s, sse) = h.stream("/v1/chat/completions", b).await;
    assert_eq!(s, 200);
    assert_eq!(sse_content(&sse), fixed, "prefix collisions are released intact at the end");
}

#[tokio::test]
async fn simultaneous_conversations_and_apps_are_isolated() {
    let h = std::sync::Arc::new(harness("mock-echo").await);
    let (_, key2) = pv_daemon::state::create_app(&h.st, "second-app", None, None).unwrap();
    let mut tasks = vec![];
    for i in 0..24 {
        let h = h.clone();
        let key = if i % 2 == 0 { h.key.clone() } else { key2.clone() };
        tasks.push(tokio::spawn(async move {
            let email = format!("user{i}.iso@example.net");
            let mut b = user(&format!("mail {email}"));
            b["stream"] = json!(i % 3 != 0);
            let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&key).header("x-zfx-conversation", format!("conv-{i}")).json(&b).send().await.unwrap();
            (i, email, r.text().await.unwrap())
        }));
    }
    for t in tasks {
        let (i, email, body) = t.await.unwrap();
        assert!(body.contains(&email), "{i} restored its own value");
        for j in 0..24 {
            if j != i {
                assert!(!body.contains(&format!("user{j}.iso@example.net")), "conversation {i} saw value of {j}");
            }
        }
    }
    // A token minted in conversation A, echoed back by the model in conversation B, stays a token.
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).header("x-zfx-conversation", "A").json(&user("mail a.only@example.com")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let up = h.upstream_text();
    let tok = up.split("[[ZFX_EMAIL_").last().unwrap().split("]]").next().unwrap();
    let token = format!("[[ZFX_EMAIL_{tok}]]");
    model(&h, &format!("mock-fixed:see {token}")).await;
    for (key, conv) in [(&h.key, "B"), (&key2, "A")] {
        let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(key).header("x-zfx-conversation", conv).json(&user("hi")).send().await.unwrap();
        let v: Value = r.json().await.unwrap();
        assert!(!v.to_string().contains("a.only@example.com"), "{conv}: {v}");
        assert!(v.to_string().contains(&token));
    }
    // Legacy conversation header still scopes correctly.
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).header("x-promptvpn-conversation", "A").json(&user("hi")).send().await.unwrap();
    let v: Value = r.json().await.unwrap();
    assert!(v.to_string().contains("a.only@example.com"), "{v}");
}

#[tokio::test]
async fn malformed_chunks_and_midstream_errors_end_the_stream_safely() {
    let h = harness("mock-malformed").await;
    let mut b = user("hello mal.formed@example.com");
    b["stream"] = json!(true);
    let (s, sse) = h.stream("/v1/chat/completions", b.clone()).await;
    assert_eq!(s, 200);
    assert!(sse.contains("ZFX_UPSTREAM_FAILURE"), "{sse}");
    assert!(!sse_content(&sse).contains("more"), "nothing after the malformed chunk: {sse}");
    model(&h, "mock-midstream-error").await;
    let (_, sse) = h.stream("/v1/chat/completions", b).await;
    assert!(sse.contains("ZFX_UPSTREAM_FAILURE"), "{sse}");
    assert!(!sse.contains("upstream failed on input"), "upstream error text is not forwarded: {sse}");
    model(&h, "mock-badutf8").await;
    let mut b = user("hello");
    b["stream"] = json!(true);
    let (_, sse) = h.stream("/v1/chat/completions", b).await;
    assert!(sse.contains("ZFX_UPSTREAM_FAILURE"), "invalid UTF-8 ends the stream: {sse}");
    let ev = h.events_text().await;
    assert!(ev.contains("malformed upstream chunk") && ev.contains("upstream reported an error mid-stream") && ev.contains("malformed upstream stream"), "{ev}");
}

#[tokio::test]
async fn idle_upstream_times_out() {
    let h = harness("mock-stall").await;
    set(&h, |s| s.stream_idle_timeout_secs = 1);
    let mut b = user("hello");
    b["stream"] = json!(true);
    let t = std::time::Instant::now();
    let (_, sse) = h.stream("/v1/chat/completions", b).await;
    assert!(t.elapsed() < std::time::Duration::from_secs(10));
    assert!(sse.contains("ZFX_UPSTREAM_FAILURE"), "{sse}");
    assert!(h.events_text().await.contains("idle timeout"));
}

#[tokio::test]
async fn client_cancellation_is_audited_and_not_replayed() {
    let h = harness("mock-slow").await;
    let mut b = user("hello slow@example.com");
    b["stream"] = json!(true);
    let mut r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).json(&b).send().await.unwrap();
    let first = r.chunk().await.unwrap();
    assert!(first.is_some());
    drop(r);
    let mut found = false;
    for _ in 0..100 {
        if h.events_text().await.contains("client cancelled the stream") {
            found = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(found, "cancellation recorded");
    assert_eq!(h.rec.count(), 1, "no retry after cancellation");
}

#[tokio::test]
async fn stream_failure_after_first_byte_is_never_replayed_to_failover() {
    let h = harness("mock-malformed").await;
    let rec2 = std::sync::Arc::new(pv_daemon::mock::Recorded::default());
    let (base2, _) = pv_daemon::mock::serve(0, rec2.clone()).await.unwrap();
    pv_daemon::admin::put_provider_inner(&h.st, provider("backup", &base2, "mock-echo", DestinationTier::ExternalUnverified, true, true), None).map_err(|_| ()).unwrap();
    set(&h, |s| s.failover = vec!["backup".into()]);
    let mut b = user("hello");
    b["stream"] = json!(true);
    let (_, sse) = h.stream("/v1/chat/completions", b).await;
    assert!(sse.contains("ZFX_UPSTREAM_FAILURE"));
    assert_eq!(rec2.count(), 0, "route was final once the primary answered");
    // Before any byte (HTTP 503), failover to an approved destination is allowed
    // and re-evaluated under policy.
    model(&h, "mock-fail-503").await;
    let (s, _) = h.chat(user("hello")).await;
    assert_eq!(s, 200);
    assert_eq!(rec2.count(), 1);
}

#[tokio::test]
async fn route_metadata_is_finalized_before_first_byte_and_never_contains_values() {
    let h = harness("mock-echo").await;
    let mut b = user("mail meta.user@example.com");
    b["stream"] = json!(true);
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).json(&b).send().await.unwrap();
    let route = r.headers().get("x-zfx-privacy-route").unwrap().to_str().unwrap().to_string();
    assert!(route.contains("tier=TRANSFORMED_PUBLIC") && route.contains("action=TRANSFORM") && route.contains("stream=incremental"), "{route}");
    assert!(!route.contains("meta.user"));
    let id = r.headers().get("x-zfx-request-id").unwrap().to_str().unwrap().to_string();
    let _ = r.text().await;
    let rec: Value = h.http.get(format!("{}/v1/zfx/requests/{id}/privacy", h.base)).bearer_auth(&h.key).send().await.unwrap().json().await.unwrap();
    assert_eq!(rec["privacy_tier"], "TRANSFORMED_PUBLIC");
    assert_eq!(rec["transformed_values"], 1);
    assert!(!rec.to_string().contains("meta.user"));
    // Another app cannot read it.
    let (_, k2) = pv_daemon::state::create_app(&h.st, "other", None, None).unwrap();
    let r = h.http.get(format!("{}/v1/zfx/requests/{id}/privacy", h.base)).bearer_auth(&k2).send().await.unwrap();
    assert_eq!(r.status(), 404);
    // Output buffering is announced when the policy needs full-output checks.
    set(&h, |s| s.outbound_span_model = true);
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).json(&b).send().await.unwrap();
    assert_eq!(r.headers().get("x-zfx-notice").unwrap(), "ZFX_STREAM_SAFETY_BUFFERED");
    assert!(r.headers().get("x-zfx-privacy-route").unwrap().to_str().unwrap().contains("stream=buffered"));
    let _ = r.text().await;
    b["zfx_privacy"] = json!({"stream_buffering": "reject"});
    let before = h.rec.count();
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).json(&b).send().await.unwrap();
    assert_eq!(r.status(), 409);
    assert_eq!(h.rec.count(), before, "rejected before anything was sent");
}

#[tokio::test]
async fn privacy_extension_only_tightens() {
    let h = harness("mock-echo").await;
    // transform_only with content that only needs transformation: allowed, and the extension is stripped.
    let mut b = user("mail ext.user@example.com");
    b["zfx_privacy"] = json!({"mode": "transform_only"});
    let (s, v) = h.chat(b).await;
    assert_eq!(s, 200, "{v}");
    assert!(!h.upstream_text().contains("zfx_privacy"));
    // local_only with no local destination: fails closed, nothing sent.
    let before = h.rec.count();
    let mut b = user("hello");
    b["zfx_privacy"] = json!({"mode": "local_only"});
    let (s, v) = h.chat(b).await;
    assert_eq!(s, 403, "{v}");
    assert_eq!(v["error"]["code"], "ZFX_FALLBACK_UNAVAILABLE");
    assert_eq!(h.rec.count(), before);
    // Unknown extension fields are rejected without echoing them.
    let mut b = user("hello");
    b["zfx_privacy"] = json!({"mode": "auto", "allow_everything": "alice@example.com"});
    let (s, v) = h.chat(b).await;
    assert_eq!(s, 400);
    assert!(!v.to_string().contains("alice@example.com"));
    // Legacy error names on request.
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).header("x-zfx-error-codes", "pv1").json(&user("ssn 123-45-6789")).send().await.unwrap();
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "PV_POLICY_BLOCKED");
    assert_eq!(v["error"]["zfx_code"], "ZFX_POLICY_BLOCKED");
}
