//! The clearing core's OraclePrint codec against the index workstream's frozen golden vectors
//! (`markets/index/vectors/oracle_print_v1.json`, copied to tests/data), and against a real
//! reporter-signed print of the Claude Opus 5.5 index.

use serde_json::Value;
use zfx_perps_clearing::print::{CodecError, OraclePrint};
use zfx_perps_clearing::store::{Crypto, NativeCrypto};
use zfx_perps_clearing::wire::{hex, unhex};

fn load(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap()
}

fn arr<const N: usize>(h: &str) -> [u8; N] {
    unhex(h).unwrap().try_into().unwrap()
}

#[test]
fn golden_vectors_decode_encode_hash_and_verify() {
    let v = load("oracle_print_v1.json");
    let pos = v["vectors"].as_array().unwrap();
    assert!(pos.len() >= 4);
    for x in pos {
        let bytes = unhex(x["canonicalHex"].as_str().unwrap()).unwrap();
        let p = OraclePrint::decode(&bytes).unwrap();
        assert_eq!(p.encode().unwrap(), bytes, "{}", x["name"]);
        assert_eq!(p.index_id, x["indexId"].as_str().unwrap());
        assert_eq!(p.value.to_string(), x["valueInt"].as_str().unwrap());
        assert_eq!(p.source_end_ms.to_string(), x["sourceEndMs"].as_str().unwrap());
        assert_eq!(p.valid_until_ms.to_string(), x["validUntilMs"].as_str().unwrap());
        assert_eq!(p.status.name(), x["status"].as_str().unwrap());
        assert_eq!(p.data_status.name(), x["dataStatus"].as_str().unwrap());
        assert_eq!(p.finality.name(), x["finality"].as_str().unwrap());
        assert_eq!(hex(&NativeCrypto.sha256(&bytes)), x["printHash"].as_str().unwrap());
        let pk: [u8; 32] = arr(x["pubkeyHex"].as_str().unwrap());
        let sig: [u8; 64] = arr(x["signatureHex"].as_str().unwrap());
        assert!(NativeCrypto.ed25519_verify(&sig, &bytes, &pk), "{}", x["name"]);
    }
    for x in v["negative"].as_array().unwrap() {
        let bytes = unhex(x["canonicalHex"].as_str().unwrap()).unwrap();
        let expect = x["expect"].as_str().unwrap();
        match OraclePrint::decode(&bytes) {
            Err(e) => assert!(expect.contains(match e {
                CodecError::TrailingBytes => "TrailingBytes",
                CodecError::BadDomain => "BadDomain",
                CodecError::BadEnum => "BadEnum",
                CodecError::Truncated => "Truncated",
                CodecError::StringTooLong => "StringTooLong",
                CodecError::BadUtf8 => "BadUtf8",
            }), "{}: got {e:?}, expected {expect}", x["name"]),
            Ok(_) => {
                let pk: [u8; 32] = arr(x["pubkeyHex"].as_str().unwrap());
                let sig: [u8; 64] = arr(x["signatureHex"].as_str().unwrap());
                assert!(!NativeCrypto.ed25519_verify(&sig, &bytes, &pk), "{}", x["name"]);
                assert!(expect.contains("signature invalid"), "{}", x["name"]);
            }
        }
    }
}

#[test]
fn real_signed_opus_print_verifies() {
    for (file, index_id) in [
        ("real_print_opus.json", "or-model-spend/anthropic/claude-opus-5.5-20260921"),
        ("real_print_opus_lic7d.json", "or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921"),
    ] {
        check_real(file, index_id);
    }
}

fn check_real(file: &str, index_id: &str) {
    let v = load(file);
    let p = &v["print"];
    let bytes = unhex(p["canonicalHex"].as_str().unwrap()).unwrap();
    let d = OraclePrint::decode(&bytes).unwrap();
    assert_eq!(d.index_id, index_id);
    assert!(d.usable());
    let pk: [u8; 32] = arr(v["reporter_pubkey_hex"].as_str().unwrap());
    let sig_entry = p["reporterSignatures"][0].as_str().unwrap();
    let sig: [u8; 64] = arr(sig_entry.rsplit(':').next().unwrap());
    assert!(NativeCrypto.ed25519_verify(&sig, &bytes, &pk));
    assert_eq!(hex(&NativeCrypto.sha256(&bytes)), p["printHash"].as_str().unwrap());
}
