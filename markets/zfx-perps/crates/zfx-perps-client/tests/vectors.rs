//! Cross-language vectors: canonical messages and ed25519 signatures from a fixed seed.
//! The TypeScript SDK test (packages/sdk-ts/test/wire.test.ts) checks it produces the same bytes.
//! Regenerate with ZFX_PERPS_WRITE_VECTORS=1 cargo test -p zfx-perps-client --test vectors.

use zfx_perps_client::Signer;
use serde_json::json;

#[test]
fn vectors_match_file() {
    let s = Signer::from_seed([7; 32]);
    let order = zfx_perps_wire::OrderMsg {
        account: "alice.test.near",
        market: "NEAR-USDC",
        side: "buy",
        price: 42_500,
        qty: 12,
        tif: "gtc",
        stp: "cancel_taker",
        client_id: 7,
        ts_ms: 1_700_000_000_000,
    };
    let msgs = [
        ("order", order.to_bytes()),
        (
            "cancel",
            zfx_perps_wire::cancel_msg("alice.test.near", "NEAR-USDC", 91, 0, 1_700_000_000_001),
        ),
        ("cancel_all", zfx_perps_wire::cancel_all_msg("alice.test.near", None, 1_700_000_000_002)),
        (
            "cancel_all_market",
            zfx_perps_wire::cancel_all_msg("alice.test.near", Some("NEAR-USDC"), 1_700_000_000_003),
        ),
        (
            "withdraw",
            zfx_perps_wire::withdraw_msg("ob.test.near", "alice.test.near", "usdc.test.near", u128::MAX, 3),
        ),
        ("auth", zfx_perps_wire::auth_msg("alice.test.near", 1_700_000_000_004)),
    ];
    let v = json!({
        "seed_hex": "07".repeat(32),
        "public_key": s.public_key(),
        "near_secret": s.near_secret(),
        "messages": msgs.iter().map(|(k, m)| json!({"name": k, "message": String::from_utf8(m.clone()).unwrap(), "signature_b64": s.sign_b64(m)})).collect::<Vec<_>>(),
    });
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/sdk-ts/test/vectors.json");
    let text = serde_json::to_string_pretty(&v).unwrap() + "\n";
    if std::env::var("ZFX_PERPS_WRITE_VECTORS").is_ok() {
        std::fs::write(path, &text).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("vectors.json (run with ZFX_PERPS_WRITE_VECTORS=1)"),
        text
    );
}
