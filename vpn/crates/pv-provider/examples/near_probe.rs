//! Live probe: verify NEAR attestation for models without an API key.
//! cargo run -p pv-provider --example near_probe -- <model>...
use nearai_inference_sdk::*;
use std::time::Instant;

#[tokio::main]
async fn main() {
    let models: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = InferenceClientOptions::default();
    opts.api_key = std::env::var("NEARAI_API_KEY").ok();
    opts.model_verification.policy = Some(ModelAttestationPolicy { accepted_tcb_statuses: None, gpu_evidence: GpuEvidenceRequirement::Required });
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("x-no-aliasing", "true".parse().unwrap());
    opts.headers = h;
    let client = InferenceClient::with_options(opts).expect("client");
    for m in models {
        let t = Instant::now();
        match client.verify(&m).await {
            Ok(r) => {
                println!("{m}: OK in {:?}; gateway signer {} tcb {:?} tls {:?}", t.elapsed(), r.gateway.evidence.signer.signing_address, r.gateway.evidence.tcb_status, r.gateway.tls_binding);
                for mm in &r.models {
                    println!("  model signer {} tcb {:?} gpu {:?} pubkey? {}", mm.evidence.signer.signing_address, mm.evidence.tcb_status, mm.gpu_evidence, mm.signing_public_key.is_some());
                }
                if r.models.is_empty() {
                    println!("  (gateway-only / incognito: no model evidence)");
                }
            }
            Err(e) => println!("{m}: ERR in {:?}: {e} / {e:?}", t.elapsed()),
        }
    }
}
