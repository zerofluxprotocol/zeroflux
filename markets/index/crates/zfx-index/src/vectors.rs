//! Golden test vectors for OraclePrint v1. Deterministic: fixed test seeds
//! (never used for real prints) and RFC 8032 deterministic signatures.

use crate::engine::{print_json, FORMULA_VERSION};
use serde_json::{json, Value};
use zfx_oracle_print::{sign, verify, DataStatus, Finality, OraclePrint, OracleStatus, DOMAIN};

fn seed(b: u8) -> [u8; 32] {
    let mut s = [0u8; 32];
    for (i, x) in s.iter_mut().enumerate() {
        *x = b.wrapping_add(i as u8);
    }
    s
}

pub fn cases() -> Vec<(&'static str, OraclePrint, [u8; 32])> {
    let fh = zfx_oracle_print::sha256(b"example formula text");
    let base = OraclePrint {
        index_id: "or-model-spend/example/model-20260101".into(),
        version: FORMULA_VERSION.into(),
        value_scale: 6,
        value: 42_500_000, // $42.50, the spec 4.2 worked example
        source_start_ms: 1_793_923_200_000, // 2026-11-06T00:00:00.000Z
        source_end_ms: 1_794_009_600_000,   // 2026-11-07T00:00:00.000Z
        observed_at_ms: 1_794_031_200_000,  // 2026-11-07T06:00:00.000Z
        published_at_ms: 1_794_031_260_123,
        valid_until_ms: 1_794_182_400_000, // end + 48 h
        source_hash: zfx_oracle_print::source_set_hash(&[zfx_oracle_print::sha256(b"tokens blob"), zfx_oracle_print::sha256(b"prices blob")]),
        formula_hash: fh,
        status: OracleStatus::Fresh,
        data_status: DataStatus::Ok,
        finality: Finality::Final,
        revision: 1,
        prev_print_hash: [0; 32],
    };
    let mut not_reported = base.clone();
    not_reported.value = 0;
    not_reported.status = OracleStatus::Unavailable;
    not_reported.data_status = DataStatus::NotReported;
    let mut corrected = base.clone();
    corrected.value = 43_125_001;
    corrected.finality = Finality::Corrected;
    corrected.revision = 2;
    corrected.published_at_ms += 86_400_000;
    corrected.prev_print_hash = base.hash().unwrap();
    let mut big = base.clone();
    big.index_id = "or-model-spend/anthropic/claude-opus-5.5-20260921".into();
    big.value = u64::MAX;
    big.source_start_ms = -86_400_000;
    big.source_end_ms = 0;
    let mut weekly = base.clone();
    weekly.index_id = "or-model-spend-7d/example/model-20260101".into();
    weekly.version = crate::weekly::FORMULA_VERSION_7D.into();
    weekly.value = 41_071_429; // mean of 7 days summing to $287.500003
    weekly.source_start_ms = 1_793_404_800_000; // 2026-10-31T00:00:00.000Z (d-6)
    weekly.source_hash = zfx_oracle_print::source_set_hash(&[[0xa1; 32], [0xa2; 32], [0xa3; 32], [0xa4; 32], [0xa5; 32], [0xa6; 32], [0xa7; 32]]);
    weekly.formula_hash = zfx_oracle_print::sha256(b"example 7d formula text");
    let mut lic = base.clone();
    lic.index_id = "or-model-spend-lic/example/model-20260101".into();
    lic.version = crate::licensed::FORMULA_VERSION_LIC.into();
    lic.value = 62_550_000; // licensed::tests::value_formula_exact: $62.55
    lic.formula_hash = zfx_oracle_print::sha256(b"example licensed formula text || params");
    let mut lic7 = weekly.clone();
    lic7.index_id = "or-model-spend-lic-7d/example/model-20260101".into();
    lic7.version = crate::weekly::FORMULA_VERSION_LIC_7D.into();
    lic7.formula_hash = zfx_oracle_print::sha256(b"example licensed 7d formula text");
    vec![
        ("final_ok_worked_example", base, seed(1)),
        ("not_reported_unavailable", not_reported, seed(1)),
        ("corrected_revision_2", corrected, seed(1)),
        ("edge_u64_max_negative_time", big, seed(0x80)),
        ("weekly_7d_final", weekly, seed(1)),
        ("licensed_daily_final", lic, seed(1)),
        ("licensed_7d_final", lic7, seed(1)),
    ]
}

pub fn generate() -> Value {
    let mut out = Vec::new();
    for (name, p, sd) in cases() {
        let bytes = p.encode().unwrap();
        let (pk, sig) = sign(&bytes, &sd);
        assert!(verify(&bytes, &pk, &sig));
        let mut v = print_json(&p, &[(pk, sig)]);
        let o = v.as_object_mut().unwrap();
        o.insert("name".into(), json!(name));
        o.insert("testSeedHex".into(), json!(hex::encode(sd)));
        o.insert("pubkeyHex".into(), json!(hex::encode(pk)));
        o.insert("signatureHex".into(), json!(hex::encode(sig)));
        o.insert("canonicalLen".into(), json!(bytes.len()));
        o.insert("valueUnitsInt".into(), json!(p.value.to_string()));
        o.insert("sourceStartMs".into(), json!(p.source_start_ms.to_string()));
        o.insert("sourceEndMs".into(), json!(p.source_end_ms.to_string()));
        o.insert("observedAtMs".into(), json!(p.observed_at_ms.to_string()));
        o.insert("publishedAtMs".into(), json!(p.published_at_ms.to_string()));
        o.insert("validUntilMs".into(), json!(p.valid_until_ms.to_string()));
        out.push(v);
    }
    let first = cases().remove(0);
    let good = first.1.encode().unwrap();
    let (pk, sig) = sign(&good, &first.2);
    let mut flipped = good.clone();
    flipped[DOMAIN.len() + 10] ^= 0x01;
    let mut trailing = good.clone();
    trailing.push(0);
    let mut bad_enum = good.clone();
    let n = bad_enum.len();
    bad_enum[n - 32 - 4 - 3] = 7; // status byte
    json!({
        "format": "ZFX OraclePrint v1",
        "domain_ascii": String::from_utf8_lossy(DOMAIN),
        "domain_hex": hex::encode(DOMAIN),
        "hash": "sha256(canonical bytes)",
        "signature": "ed25519 (RFC 8032, pure, no prehash) over the canonical bytes; verify strictly",
        "layout": [
            "domain: 19 raw bytes 'ZFX-oracle-print-v1'",
            "index_id: u32le length + utf8 (max 256)",
            "version: u32le length + utf8 (max 256)",
            "value_scale: u8",
            "value: u64le",
            "source_start_ms: i64le", "source_end_ms: i64le", "observed_at_ms: i64le", "published_at_ms: i64le", "valid_until_ms: i64le",
            "source_hash: 32 bytes", "formula_hash: 32 bytes",
            "status: u8 (0 FRESH, 1 STALE, 2 DISPUTED, 3 UNAVAILABLE)",
            "data_status: u8 (0 OK, 1 NOT_REPORTED, 2 UNPRICED, 3 NO_VALID_CLOSE_PRICE)",
            "finality: u8 (0 PROVISIONAL, 1 FINAL, 2 CORRECTED)",
            "revision: u32le",
            "prev_print_hash: 32 bytes (zeros when revision = 1)"
        ],
        "source_set_hash": "sha256('ZFX-source-set-v1' || u32le(n) || h1 || ... || hn)",
        "warning": "testSeedHex values are public test keys. Never use them for real prints.",
        "vectors": out,
        "negative": [
            {"name": "bit_flip_in_index_id", "canonicalHex": hex::encode(&flipped), "pubkeyHex": hex::encode(pk), "signatureHex": hex::encode(sig), "expect": "signature invalid (decodes, but verify fails)"},
            {"name": "trailing_byte", "canonicalHex": hex::encode(&trailing), "expect": "decode error TrailingBytes"},
            {"name": "unknown_status_enum", "canonicalHex": hex::encode(&bad_enum), "expect": "decode error BadEnum"}
        ]
    })
}
