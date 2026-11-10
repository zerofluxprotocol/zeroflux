//! Record a live NEAR AI attestation report plus the Intel DCAP collateral for
//! every TDX quote in it, so tests can re-run full DCAP verification offline.
//! No API key is needed (the attestation endpoint is public).
//!
//!   cargo run -p pv-provider --example near_record -- <model> <out.json>
//!
//! The NVIDIA GPU evidence is recorded as returned; NRAS is not contacted, so
//! replays can only check the GPU payload's nonce binding, not NVIDIA's verdict.
use rand::RngCore;
use serde_json::{json, Value};

#[tokio::main]
async fn main() {
    let mut a = std::env::args().skip(1);
    let model = a.next().expect("model");
    let out = a.next().expect("out.json");
    let mut n = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut n);
    let nonce = hex::encode(n);
    let base = pv_provider::near::DEFAULT_NEAR_BASE;
    let url = format!("{base}/attestation/report?model={}&provider=near&nonce={nonce}&include_tls_fingerprint=false&signing_algo=ecdsa", urlenc(&model));
    let http = reqwest::Client::new();
    let report: Value = http.get(&url).header("x-no-aliasing", "true").header("accept-encoding", "identity").send().await.expect("fetch").json().await.expect("json");
    let mut quotes: Vec<(String, String)> = vec![];
    if let Some(q) = report["gateway_attestation"]["intel_quote"].as_str() {
        quotes.push(("gateway".into(), q.into()));
    }
    for (i, m) in report["model_attestations"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        if let Some(q) = m["intel_quote"].as_str() {
            quotes.push((format!("model[{i}]"), q.into()));
        }
        if let Some(q) = m["compose_manager_attestation"]["quote"].as_str() {
            quotes.push((format!("model[{i}].compose_manager"), q.into()));
        }
    }
    let client = dcap_qvl::collateral::CollateralClient::with_default_http(nearai_inference_sdk::DEFAULT_INTEL_PCCS_URL).expect("pccs client");
    let mut collateral = serde_json::Map::new();
    for (name, q) in &quotes {
        let bytes = hex::decode(q).expect("hex quote");
        let c = client.fetch(&bytes).await.expect("collateral");
        collateral.insert(q[..64].to_string(), json!({"for": name, "collateral": c}));
        eprintln!("collateral for {name}");
    }
    let captured_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let doc = json!({
        "note": "Recorded from the public NEAR AI attestation endpoint by examples/near_record.rs. Collateral from Intel PCS. NRAS not contacted.",
        "model": model,
        "base_url": base,
        "nonce": nonce,
        "captured_at_unix": captured_at,
        "report": report,
        "dcap_collateral_by_quote_prefix": collateral,
    });
    std::fs::write(&out, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    println!("wrote {out}");
}

fn urlenc(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.~".contains(c) { c.to_string() } else { format!("%{:02X}", c as u32) }).collect()
}
