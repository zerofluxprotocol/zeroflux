//! Hosted gateway: disclosure, tenant isolation, per-request maps, no raw
//! retention, scopes, key lifecycle, rate limits, quotas, idempotent retries,
//! Chat + Responses stream and non-stream.

mod common;
use common::*;
use serde_json::{json, Value};

fn all() -> Value {
    json!(["inference", "models:read", "privacy:read"])
}

fn user(t: &str) -> Value {
    json!({"model": "m", "messages": [{"role": "user", "content": t}]})
}

fn walk(p: &std::path::Path, out: &mut Vec<u8>) {
    for e in std::fs::read_dir(p).unwrap().flatten() {
        let path = e.path();
        if path.is_dir() {
            walk(&path, out);
        } else if let Ok(b) = std::fs::read(&path) {
            out.extend(b);
        }
    }
}

#[tokio::test]
async fn disclosure_is_explicit() {
    let g = gateway(0).await;
    let v: Value = g.http.get(format!("{}/v1/zfx/privacy-policy", g.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["modes"]["hosted"]["operator_sees_raw_prompts"], true);
    assert_eq!(v["zero_knowledge"], false);
    assert_eq!(v["enclave"], false);
    assert!(v["modes"]["hosted"]["disclosure"].as_str().unwrap().contains("operator can technically see"));
    assert_eq!(v["modes"]["hosted-preprocessed"]["operator_sees_raw_prompts"], false);
    let t = g.tenant("acme", "mock-echo", json!({}), all()).await;
    let (s, _, h) = g.chat(&t.key, user("hello"), &[]).await;
    assert_eq!(s, 200);
    assert_eq!(h.get("x-zfx-mode").unwrap(), "hosted");
    assert!(h.get("x-zfx-trust").unwrap().to_str().unwrap().contains("operator-sees-raw-prompt"));
    assert!(h.get("x-zfx-privacy-route").unwrap().to_str().unwrap().contains("TRANSFORMED_PUBLIC"));
}

#[tokio::test]
async fn transforms_restores_and_retains_nothing() {
    let g = gateway(0).await;
    let t = g.tenant("acme", "mock-echo", json!({}), all()).await;
    let raw = ["dana.private@example.com", "415-555-0199"];
    let (s, v, _) = g.chat(&t.key, user(&format!("Email {} or call {}", raw[0], raw[1])), &[]).await;
    assert_eq!(s, 200, "{v}");
    let content = v["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(content.contains(raw[0]) && content.contains(raw[1]), "{content}");
    let up = t.rec.all_raw();
    for r in raw {
        assert!(!up.contains(r), "{r} reached the upstream");
    }
    assert!(up.contains("[[ZFX_EMAIL_"));
    // The tenant's own upstream credential was used, and nothing else.
    assert!(up.contains("upstream-secret-of-acme"));
    assert!(!up.contains(&t.key), "tenant API key forwarded upstream");
    // Streaming, Chat and Responses.
    let mut b = user(&format!("mail {}", raw[0]));
    b["stream"] = json!(true);
    let sse = g.http.post(format!("{}/v1/chat/completions", g.base)).bearer_auth(&t.key).json(&b).send().await.unwrap().text().await.unwrap();
    assert!(sse.contains("dana.priv") && !sse.contains("[[ZFX_"), "{sse}");
    let r: Value = g.http.post(format!("{}/v1/responses", g.base)).bearer_auth(&t.key).json(&json!({"model": "m", "input": format!("mail {}", raw[0])})).send().await.unwrap().json().await.unwrap();
    assert!(r.to_string().contains(raw[0]), "{r}");
    let sse = g.http.post(format!("{}/v1/responses", g.base)).bearer_auth(&t.key).json(&json!({"model": "m", "stream": true, "input": format!("mail {}", raw[0])})).send().await.unwrap().text().await.unwrap();
    assert!(sse.contains("response.completed") && !sse.contains("[[ZFX_"), "{sse}");
    // Critical values: blocked, nothing sent.
    let n = t.rec.count();
    let (s, v, _) = g.chat(&t.key, user("ssn 123-45-6789"), &[]).await;
    assert_eq!(s, 403);
    assert_eq!(v["error"]["code"], "ZFX_POLICY_BLOCKED");
    assert_eq!(t.rec.count(), n);
    // Nothing raw on disk: control DB, tenant metadata DB, WAL files.
    g.h.housekeeping();
    let mut disk = vec![];
    walk(g.dir.path(), &mut disk);
    let disk = String::from_utf8_lossy(&disk);
    for r in raw.iter().chain(["123-45-6789"].iter()) {
        assert!(!disk.contains(r), "{r} written to disk");
    }
    assert!(!disk.contains("upstream-secret-of-acme"), "upstream credential stored unsealed");
    assert!(!disk.contains(&t.key), "API key stored in plaintext");
}

#[tokio::test]
async fn maps_are_per_request_and_tenants_are_isolated() {
    let g = gateway(0).await;
    let a = g.tenant("alpha", "mock-echo", json!({}), all()).await;
    let b = g.tenant("beta", "mock-echo", json!({}), all()).await;
    let (s, _, ha) = g.chat(&a.key, user("mail only.alpha@example.com"), &[("x-zfx-conversation", "c1")]).await;
    assert_eq!(s, 200);
    let up = a.rec.all_text();
    let tok = format!("[[ZFX_EMAIL_{}]]", up.split("[[ZFX_EMAIL_").nth(1).unwrap().split("]]").next().unwrap());
    // The model echoes A's token back in a LATER request: same tenant, same
    // conversation id. Hosted maps live for one request, so it stays a token.
    let (s, v, _) = g.chat(&a.key, user(&format!("repeat {tok}")), &[("x-zfx-conversation", "c1")]).await;
    assert_eq!(s, 200);
    assert!(v.to_string().contains(&tok) && !v.to_string().contains("only.alpha@example.com"), "{v}");
    // Tenant B gets the token: never restored, and B's upstream never saw A's credential.
    let (s, v, _) = g.chat(&b.key, user(&format!("repeat {tok}")), &[("x-zfx-conversation", "c1")]).await;
    assert_eq!(s, 200);
    assert!(!v.to_string().contains("only.alpha@example.com"));
    assert!(!b.rec.all_raw().contains("upstream-secret-of-alpha"));
    assert!(!a.rec.all_raw().contains("upstream-secret-of-beta"));
    // Route records: readable by the owner, 404 for another tenant.
    let id = ha.get("x-zfx-request-id").unwrap().to_str().unwrap();
    let r = g.http.get(format!("{}/v1/zfx/requests/{id}/privacy", g.base)).bearer_auth(&a.key).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let rec: Value = r.json().await.unwrap();
    assert_eq!(rec["privacy_tier"], "TRANSFORMED_PUBLIC");
    assert!(!rec.to_string().contains("only.alpha"));
    let r = g.http.get(format!("{}/v1/zfx/requests/{id}/privacy", g.base)).bearer_auth(&b.key).send().await.unwrap();
    assert_eq!(r.status(), 404);
    // Concurrent requests of two tenants.
    let mut tasks = vec![];
    for i in 0..16 {
        let (key, http, base) = (if i % 2 == 0 { a.key.clone() } else { b.key.clone() }, g.http.clone(), g.base.clone());
        tasks.push(tokio::spawn(async move {
            let email = format!("p{i}.conc@example.org");
            let v: Value = http.post(format!("{base}/v1/chat/completions")).bearer_auth(&key).json(&user(&format!("mail {email}"))).send().await.unwrap().json().await.unwrap();
            (i, v.to_string())
        }));
    }
    for t in tasks {
        let (i, s) = t.await.unwrap();
        assert!(s.contains(&format!("p{i}.conc@example.org")));
        for j in (0..16).filter(|j| *j != i) {
            assert!(!s.contains(&format!("p{j}.conc@example.org")));
        }
    }
}

#[tokio::test]
async fn keys_scopes_and_lifecycle() {
    let g = gateway(0).await;
    let t = g.tenant("acme", "mock-echo", json!({}), json!(["models:read"])).await;
    let (s, v, _) = g.chat(&t.key, user("hi"), &[]).await;
    assert_eq!(s, 403);
    assert_eq!(v["error"]["code"], "ZFX_FORBIDDEN_SCOPE");
    let m: Value = g.http.get(format!("{}/v1/models", g.base)).bearer_auth(&t.key).send().await.unwrap().json().await.unwrap();
    assert_eq!(m["data"][0]["privacy_tier"], "TRANSFORMED_PUBLIC");
    // Rotate: the old key stops working, the new one works.
    let (s, r) = g.admin_post(&format!("/admin/v1/tenants/{}/keys/{}/rotate", t.id, t.key_id), json!({})).await;
    assert_eq!(s, 200, "{r}");
    let new = r["key"].as_str().unwrap();
    assert_eq!(g.http.get(format!("{}/v1/models", g.base)).bearer_auth(&t.key).send().await.unwrap().status(), 401);
    assert_eq!(g.http.get(format!("{}/v1/models", g.base)).bearer_auth(new).send().await.unwrap().status(), 200);
    // Revoke.
    let kid = r["info"]["key_id"].as_str().unwrap();
    let r = g.http.delete(format!("{}/admin/v1/tenants/{}/keys/{kid}", g.admin, t.id)).bearer_auth(OP).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(g.http.get(format!("{}/v1/models", g.base)).bearer_auth(new).send().await.unwrap().status(), 401);
    // Operator API needs the operator token, and tenant keys are not operator tokens.
    assert_eq!(g.http.get(format!("{}/admin/v1/tenants", g.admin)).bearer_auth(new).send().await.unwrap().status(), 401);
    assert_eq!(g.http.get(format!("{}/admin/v1/tenants", g.admin)).send().await.unwrap().status(), 401);
    // Unknown scopes are refused.
    let (s, _) = g.admin_post(&format!("/admin/v1/tenants/{}/keys", t.id), json!({"scopes": ["admin"]})).await;
    assert_eq!(s, 400);
    // A disabled tenant's keys stop working.
    let t2 = g.tenant("beta", "mock-echo", json!({}), all()).await;
    g.admin_post(&format!("/admin/v1/tenants/{}/disable", t2.id), json!({})).await;
    let (s, _, _) = g.chat(&t2.key, user("hi"), &[]).await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn rate_limits_and_quotas() {
    let g = gateway(0).await;
    let t = g.tenant("rl", "mock-echo", json!({"requests_per_minute": 2}), all()).await;
    assert_eq!(g.chat(&t.key, user("a"), &[]).await.0, 200);
    assert_eq!(g.chat(&t.key, user("b"), &[]).await.0, 200);
    let (s, v, h) = g.chat(&t.key, user("c"), &[]).await;
    assert_eq!(s, 429);
    assert_eq!(v["error"]["code"], "ZFX_RATE_LIMITED");
    assert!(h.get("retry-after").is_some());
    assert_eq!(t.rec.count(), 2);
    let q = g.tenant("quota", "mock-echo", json!({"requests_per_month": 2, "requests_per_minute": 0}), all()).await;
    assert_eq!(g.chat(&q.key, user("a"), &[]).await.0, 200);
    // Blocked requests are not billable and do not use quota.
    assert_eq!(g.chat(&q.key, user("ssn 123-45-6789"), &[]).await.0, 403);
    assert_eq!(g.chat(&q.key, user("b"), &[]).await.0, 200);
    let (s, v, _) = g.chat(&q.key, user("c"), &[]).await;
    assert_eq!(s, 429);
    assert_eq!(v["error"]["code"], "ZFX_QUOTA_EXCEEDED");
    let u: Value = g.http.get(format!("{}/v1/zfx/usage", g.base)).bearer_auth(&q.key).send().await.unwrap().json().await.unwrap();
    assert_eq!(u["usage"]["billable_requests"], 2);
    assert_eq!(u["usage"]["blocked"], 1);
    assert_eq!(u["usage"]["prompt_tokens"], 10);
}

#[tokio::test]
async fn idempotent_retries_never_call_upstream_twice() {
    // Default: results are not retained, so a retry after completion is refused.
    let g = gateway(0).await;
    let t = g.tenant("idem", "mock-echo", json!({}), all()).await;
    let b = user("mail idem.user@example.com");
    let (s, _, _) = g.chat(&t.key, b.clone(), &[("idempotency-key", "op-1")]).await;
    assert_eq!(s, 200);
    let (s, v, _) = g.chat(&t.key, b.clone(), &[("idempotency-key", "op-1")]).await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["error"]["code"], "ZFX_IDEMPOTENCY_CONFLICT");
    assert!(v["error"]["message"].as_str().unwrap().contains("not retained"));
    let (s, v, _) = g.chat(&t.key, user("different body"), &[("idempotency-key", "op-1")]).await;
    assert_eq!(s, 409);
    assert!(v["error"]["message"].as_str().unwrap().contains("different request body"));
    assert_eq!(t.rec.count(), 1);
    // Keys are per tenant.
    let other = g.tenant("other", "mock-echo", json!({}), all()).await;
    assert_eq!(g.chat(&other.key, b.clone(), &[("idempotency-key", "op-1")]).await.0, 200);

    // Opt-in in-memory replay: identical response, no second upstream call or charge.
    let g = gateway(300).await;
    let t = g.tenant("idem2", "mock-echo", json!({}), all()).await;
    let (s1, v1, _) = g.chat(&t.key, b.clone(), &[("idempotency-key", "op-2")]).await;
    let (s2, v2, h2) = g.chat(&t.key, b.clone(), &[("idempotency-key", "op-2")]).await;
    assert_eq!((s1, s2), (200, 200));
    assert_eq!(v1, v2);
    assert_eq!(h2.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(t.rec.count(), 1);
    let u: Value = g.http.get(format!("{}/v1/zfx/usage", g.base)).bearer_auth(&t.key).send().await.unwrap().json().await.unwrap();
    assert_eq!(u["usage"]["billable_requests"], 1);
    // The replay cache is memory only: nothing raw on disk.
    let mut disk = vec![];
    walk(g.dir.path(), &mut disk);
    assert!(!String::from_utf8_lossy(&disk).contains("idem.user@example.com"));
}

#[tokio::test]
async fn hosted_never_queues_for_review_and_rejects_review_option() {
    let g = gateway(0).await;
    let t = g.tenant("rev", "mock-echo", json!({}), all()).await;
    let mut b = user("hello");
    b["zfx_privacy"] = json!({"on_unavailable": "review"});
    let (s, v, _) = g.chat(&t.key, b, &[]).await;
    assert_eq!(s, 400, "{v}");
    // Confidential mode without a NEAR fallback configured: fail closed.
    let mut b = user("hello");
    b["zfx_privacy"] = json!({"mode": "confidential"});
    let (s, v, _) = g.chat(&t.key, b, &[]).await;
    assert_eq!(s, 403, "{v}");
    assert_eq!(v["error"]["code"], "ZFX_FALLBACK_UNAVAILABLE");
    assert_eq!(t.rec.count(), 0);
}

#[tokio::test]
async fn critical_categories_cannot_be_weakened_by_tenants() {
    let g = gateway(0).await;
    let cfg = json!({"name": "weak", "upstreams": [], "categories": {"GOVERNMENT_ID": "ALLOW"}});
    let (s, v) = g.admin_post("/admin/v1/tenants", cfg).await;
    assert_eq!(s, 400, "{v}");
}
