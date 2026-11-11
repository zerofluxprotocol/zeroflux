//! Golden vectors: markets/index/vectors/oracle_print_v1.json must keep verifying.
//! If this test fails, the wire format changed. That needs a new domain tag.

use zfx_oracle_print::*;

fn h(s: &serde_json::Value) -> Vec<u8> {
    hex::decode(s.as_str().unwrap()).unwrap()
}
fn n(s: &serde_json::Value) -> i64 {
    s.as_str().unwrap().parse().unwrap()
}

#[test]
fn golden_vectors() {
    let raw = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../vectors/oracle_print_v1.json")).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(v["domain_hex"].as_str().unwrap(), hex::encode(DOMAIN));
    let vs = v["vectors"].as_array().unwrap();
    assert!(vs.len() >= 4);
    for x in vs {
        let bytes = h(&x["canonicalHex"]);
        let p = OraclePrint::decode(&bytes).unwrap();
        assert_eq!(p.encode().unwrap(), bytes, "{}", x["name"]);
        assert_eq!(hex::encode(sha256(&bytes)), x["printHash"].as_str().unwrap());
        assert_eq!(p.index_id, x["indexId"].as_str().unwrap());
        assert_eq!(p.version, x["version"].as_str().unwrap());
        assert_eq!(p.value.to_string(), x["valueUnitsInt"].as_str().unwrap());
        assert_eq!(p.source_start_ms, n(&x["sourceStartMs"]));
        assert_eq!(p.source_end_ms, n(&x["sourceEndMs"]));
        assert_eq!(p.observed_at_ms, n(&x["observedAtMs"]));
        assert_eq!(p.published_at_ms, n(&x["publishedAtMs"]));
        assert_eq!(p.valid_until_ms, n(&x["validUntilMs"]));
        assert_eq!(p.status.name(), x["status"].as_str().unwrap());
        assert_eq!(p.data_status.name(), x["dataStatus"].as_str().unwrap());
        assert_eq!(p.finality.name(), x["finality"].as_str().unwrap());
        let seed: [u8; 32] = h(&x["testSeedHex"]).try_into().unwrap();
        let pk: [u8; 32] = h(&x["pubkeyHex"]).try_into().unwrap();
        let sig: [u8; 64] = h(&x["signatureHex"]).try_into().unwrap();
        assert_eq!(public_key(&seed), pk);
        assert_eq!(sign(&bytes, &seed), (pk, sig), "ed25519 is deterministic");
        assert!(verify(&bytes, &pk, &sig));
    }
    for x in v["negative"].as_array().unwrap() {
        let bytes = h(&x["canonicalHex"]);
        match x["name"].as_str().unwrap() {
            "bit_flip_in_index_id" => {
                assert!(OraclePrint::decode(&bytes).is_ok());
                let pk: [u8; 32] = h(&x["pubkeyHex"]).try_into().unwrap();
                let sig: [u8; 64] = h(&x["signatureHex"]).try_into().unwrap();
                assert!(!verify(&bytes, &pk, &sig));
            }
            "trailing_byte" => assert_eq!(OraclePrint::decode(&bytes), Err(DecodeError::TrailingBytes)),
            "unknown_status_enum" => assert_eq!(OraclePrint::decode(&bytes), Err(DecodeError::BadEnum)),
            other => panic!("unknown negative vector {other}"),
        }
    }
}
