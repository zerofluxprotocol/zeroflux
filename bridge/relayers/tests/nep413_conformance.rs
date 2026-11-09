//! Conformance: our NEP-413 hashing verifies the signed intents published in
//! the official NEAR Intents documentation (bridge/intents/fixtures).
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

#[test]
fn docs_nep413_signatures_verify_with_our_hash() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../intents/fixtures/docs_nep413_examples.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let ex = v["examples"].as_array().unwrap();
    assert!(ex.len() >= 5, "expected several doc examples, got {}", ex.len());
    let mut verified = 0;
    let mut not_verifying = Vec::new();
    for e in ex {
        let s = &e["signed"];
        let p = &s["payload"];
        let nonce: [u8; 32] = base64::engine::general_purpose::STANDARD.decode(p["nonce"].as_str().unwrap()).unwrap().try_into().unwrap();
        let h = zfx_bridge::near::nep413_hash(p["message"].as_str().unwrap(), &nonce, p["recipient"].as_str().unwrap(), p["callbackUrl"].as_str());
        let pk: [u8; 32] = bs58::decode(s["public_key"].as_str().unwrap().trim_start_matches("ed25519:")).into_vec().unwrap().try_into().unwrap();
        let sig: [u8; 64] = bs58::decode(s["signature"].as_str().unwrap().trim_start_matches("ed25519:")).into_vec().unwrap().try_into().unwrap();
        let ok = VerifyingKey::from_bytes(&pk).unwrap().verify(&h, &Signature::from_bytes(&sig)).is_ok();
        eprintln!("{} {} {}", if ok { "VERIFIES" } else { "DOES-NOT-VERIFY" }, e["source"], &s["signature"].as_str().unwrap()[..20]);
        if !ok {
            not_verifying.push(s["signature"].as_str().unwrap().to_string());
            continue;
        }
        // a one-byte change to the message must break it
        let bad = zfx_bridge::near::nep413_hash(&format!("{} ", p["message"].as_str().unwrap()), &nonce, p["recipient"].as_str().unwrap(), None);
        assert!(VerifyingKey::from_bytes(&pk).unwrap().verify(&bad, &Signature::from_bytes(&sig)).is_err());
        verified += 1;
    }
    eprintln!("verified {} official NEP-413 doc signatures", verified);
    // The Message Bus RPC page's publish_intent sample is illustrative (its
    // message omits the nonce); it is the only example that does not verify.
    assert_eq!(not_verifying.len(), 1, "unexpected non-verifying examples: {:?}", not_verifying);
    assert!(not_verifying[0].starts_with("ed25519:5tk3UyFcAgnd"));
    assert!(verified >= 13);
}
