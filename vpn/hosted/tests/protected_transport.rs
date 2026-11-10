//! Golden test (gate 5, spec 3.5 "local-to-hosted protected transport"): a
//! local ZFX VPN daemon transforms the request and keeps the map; the hosted
//! gateway, and the model behind it, receive no known raw value and no map.
//! A recording relay between the daemon and the hosted gateway captures the
//! exact bytes (body and headers) that reach the hosted side.

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use pv_daemon::mock::{self, Recorded};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

mod common;
use common::*;

type Captured = (Vec<(String, String)>, Vec<u8>);

#[derive(Default)]
struct Wire {
    requests: Mutex<Vec<Captured>>,
}

impl Wire {
    fn all(&self) -> String {
        self.requests.lock().unwrap().iter().map(|(h, b)| format!("{}\n{}", h.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n"), String::from_utf8_lossy(b))).collect::<Vec<_>>().join("\n----\n")
    }
}

struct Relay {
    wire: Arc<Wire>,
    target: String,
    http: reqwest::Client,
}

async fn relay(State(r): State<Arc<Relay>>, uri: axum::http::Uri, headers: HeaderMap, body: Bytes) -> Response {
    r.wire.requests.lock().unwrap().push((headers.iter().map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect(), body.to_vec()));
    let mut rq = r.http.post(format!("{}{}", r.target, uri.path())).body(body.to_vec());
    for (k, v) in headers.iter() {
        if k != "host" && k != "content-length" {
            rq = rq.header(k, v);
        }
    }
    let up = rq.send().await.unwrap();
    let mut resp = Response::builder().status(up.status().as_u16());
    for (k, v) in up.headers() {
        if k != "content-length" && k != "transfer-encoding" {
            resp = resp.header(k, v);
        }
    }
    resp.body(Body::from_stream(up.bytes_stream())).unwrap()
}

#[tokio::test]
async fn hosted_side_never_receives_raw_values_or_the_map() {
    let g = gateway(0).await;
    // Hosted tenant whose upstream is a recording mock model.
    let t = g.tenant("app-builder", "mock-echo", json!({}), json!(["inference", "privacy:read"])).await;

    // Relay in front of the hosted gateway records the exact bytes it gets.
    let wire = Arc::new(Wire::default());
    let rl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_base = format!("http://{}", rl.local_addr().unwrap());
    let st = Arc::new(Relay { wire: wire.clone(), target: g.base.clone(), http: reqwest::Client::builder().no_proxy().build().unwrap() });
    let app = axum::Router::new().route("/v1/{*rest}", axum::routing::post(relay)).with_state(st);
    tokio::spawn(async move { axum::serve(rl, app).await.unwrap() });

    // Local daemon with the hosted gateway as protected transport.
    let dir = tempfile::tempdir().unwrap();
    let local = pv_daemon::state::bootstrap_with_backend(pv_daemon::state::Paths::at(dir.path()), Some(Arc::new(pv_proxy::notify::Recorder::default())), Arc::new(pv_vault::keys::MemoryBackend::default())).await.unwrap();
    local.engine.detector.set_settings(pv_detectors::DetectorSettings { span_enabled: false, span_required: false, span_timeout_ms: 2000 });
    let cfg = pv_core::ProviderConfig {
        schema_version: 1,
        id: "zfx-hosted".into(),
        kind: pv_core::ProviderKind::OpenaiCompatible,
        display_name: "ZFX hosted (protected transport)".into(),
        base_url: format!("{relay_base}/v1"),
        default_model: "frontier/mock-echo".into(),
        declared_tier: pv_core::DestinationTier::ExternalUnverified,
        credential_ref: Some("provider:zfx-hosted".into()),
        enabled: true,
        allow_failover: false,
        supports_responses: true,
        timeout_secs: 10,
        zfx_hosted_transport: true,
    };
    pv_daemon::admin::put_provider_inner(&local, cfg, Some(t.key.clone())).map_err(|_| ()).unwrap();
    let mut s = local.engine.settings();
    s.default_destination = Some("zfx-hosted".into());
    pv_daemon::state::save_settings(&local, &s, "test").unwrap();
    // A value only the local user registered: the hosted side cannot know it.
    pv_daemon::admin::add_secret_inner(&local, pv_daemon::admin::AddSecretInput { kind: pv_detectors::exact::ExactKind::Value, label: "codename".into(), value: "Bluefin-Orchid-42".into(), norm: pv_detectors::normalize::NormOpts { unicode: true, casefold: true, whitespace: true, separators: false }, action: pv_core::Action::Tokenize, allow: false }).map_err(|_| ()).unwrap();
    let (_, local_key) = pv_daemon::state::create_app(&local, "agent", None, None).unwrap();
    let l = pv_daemon::bind(0).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let l2 = local.clone();
    tokio::spawn(async move { pv_daemon::serve(l2, l).await.unwrap() });
    for _ in 0..50 {
        if local.port.load(std::sync::atomic::Ordering::Relaxed) == port {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let raw = ["erin.customer@example.com", "415-555-0144", "Bluefin-Orchid-42"];
    let prompt = format!("Customer {} (phone {}) asks about project {}.", raw[0], raw[1], raw[2]);

    // Non-streaming Chat, streaming Chat, Responses.
    let v: Value = http.post(format!("http://127.0.0.1:{port}/v1/chat/completions")).bearer_auth(&local_key).json(&json!({"model": "m", "messages": [{"role": "user", "content": prompt}]})).send().await.unwrap().json().await.unwrap();
    let content = v["choices"][0]["message"]["content"].as_str().unwrap_or_else(|| panic!("{v}"));
    for r in raw {
        assert!(content.contains(r), "restored locally: {r} in {content}");
    }
    let sse = http.post(format!("http://127.0.0.1:{port}/v1/chat/completions")).bearer_auth(&local_key).json(&json!({"model": "m", "stream": true, "messages": [{"role": "user", "content": prompt}]})).send().await.unwrap().text().await.unwrap();
    for r in raw {
        assert!(sse.contains(&r[..6]), "stream restored locally: {sse}");
    }
    let resp: Value = http.post(format!("http://127.0.0.1:{port}/v1/responses")).bearer_auth(&local_key).json(&json!({"model": "m", "input": prompt})).send().await.unwrap().json().await.unwrap();
    assert!(resp.to_string().contains(raw[2]), "{resp}");

    // What the hosted side received: transformed payloads, marked preprocessed, no raw value, no map.
    let w = wire.all();
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
    for r in raw {
        assert!(!w.contains(r), "raw value {r} reached the hosted gateway:\n{w}");
    }
    assert!(w.contains("[[ZFX_EMAIL_") && w.contains("[[ZFX_EXACT_SECRET_"));
    assert!(w.contains("x-zfx-preprocessed: v1"));
    assert!(w.contains("\"preprocessed\":true"));
    assert!(!w.contains(&local_key), "local app key forwarded");
    // A map would pair tokens with values; no value is present at all, so no
    // map can be. The local vault holds the mappings.
    assert!(local.vault().mapping_count().unwrap() >= 3);
    // The model behind the hosted gateway saw the same transformed payloads.
    let up = t.rec.all_raw();
    for r in raw {
        assert!(!up.contains(r), "raw value {r} reached the hosted upstream");
    }
    // The hosted gateway minted nothing and restored nothing (preprocessed),
    // and it recorded the route as preprocessed.
    let ids: Vec<String> = local.db().events(&Default::default()).unwrap().into_iter().map(|e| e.event_id).collect();
    assert!(!ids.is_empty());
    let hosted_routes = {
        let eng = g.h.engine(&t.id).unwrap();
        let c = eng.db.conn();
        let mut st = c.prepare("SELECT json FROM privacy_routes").unwrap();
        let v: Vec<Value> = st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|j| serde_json::from_str(&j.unwrap()).unwrap()).collect();
        v
    };
    assert_eq!(hosted_routes.len(), 3);
    for r in &hosted_routes {
        assert_eq!(r["preprocessed"], true);
        assert_eq!(r["transformed_values"], 0, "hosted transformed nothing itself: {r}");
    }
}

#[tokio::test]
async fn hosted_refuses_to_transform_preprocessed_traffic() {
    let g = gateway(0).await;
    let t = g.tenant("strict", "mock-echo", json!({}), json!(["inference"])).await;
    // A client claims preprocessing but forgot to transform an email: the
    // gateway must not mint a placeholder (it would then hold a map).
    let (s, v, h) = g.chat(&t.key, json!({"model": "m", "messages": [{"role": "user", "content": "mail leaked.raw@example.com"}]}), &[("x-zfx-preprocessed", "v1")]).await;
    assert_eq!(s, 403, "{v}");
    assert_eq!(v["error"]["code"], "ZFX_POLICY_BLOCKED");
    assert!(v["error"]["message"].as_str().unwrap().contains("client must transform"));
    assert_eq!(h.get("x-zfx-mode").unwrap(), "hosted-preprocessed");
    assert_eq!(t.rec.count(), 0);
    // Placeholders in preprocessed output are returned as is, for the client to restore.
    let (s, v, _) = g.chat(&t.key, json!({"model": "m", "messages": [{"role": "user", "content": "mail [[ZFX_EMAIL_0123456789abcdef]]"}]}), &[("x-zfx-preprocessed", "v1")]).await;
    assert_eq!(s, 200, "{v}");
    assert!(v.to_string().contains("[[ZFX_EMAIL_0123456789abcdef]]"));
    let _ = Recorded::default;
    let _ = mock::serve;
}
