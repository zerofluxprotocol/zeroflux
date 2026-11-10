//! Cross-language vectors for the clearing wire formats: canonical messages, sha256 order hashes,
//! ed25519 signatures from a fixed seed, and one OraclePrint. The TypeScript SDK test
//! (packages/sdk-ts/test/clearing.test.ts) must produce identical bytes and signatures.
//! Regenerate: ZFX_PERPS_WRITE_VECTORS=1 cargo test -p zfx-perps-clearing --test vectors

use serde_json::json;
use zfx_perps_clearing::testkit::Key;
use zfx_perps_clearing::wire::*;
use zfx_perps_clearing::*;

fn hexs(b: &[u8]) -> String {
    hex(b)
}

#[test]
fn clearing_vectors_match_file() {
    let k = Key::from_seed([7; 32]);
    let c = NativeCrypto;
    let policy = [0xab; 32];
    let order = SignedOrder {
        chain_id: "near:testnet".into(),
        clearing_contract: "perps.zeroflux.testnet".into(),
        account: "alice.testnet".into(),
        market: "OPUS-PERP".into(),
        side: Side::Buy,
        order_type: OrderType::Limit,
        price: 1_250,
        qty: 12,
        reduce_only: false,
        post_only: true,
        tif: TimeInForce::Gtc,
        nonce: 7,
        expiry_ns: 1_800_000_000_000_000_000,
        client_order_id: 42,
        max_fee_bps: 10,
        policy_hash: policy,
        public_key: k.public(),
        signature: Sig64([0; 64]),
    };
    let market_order = SignedOrder {
        side: Side::Sell,
        order_type: OrderType::Market,
        price: 1,
        qty: u64::MAX,
        reduce_only: true,
        post_only: false,
        tif: TimeInForce::Ioc,
        nonce: u64::MAX,
        expiry_ns: u64::MAX,
        client_order_id: 0,
        max_fee_bps: 1_000,
        ..order.clone()
    };
    let oh = c.sha256(&order.message());
    let msgs: Vec<(&str, Vec<u8>)> = vec![
        ("order_limit", order.message()),
        ("order_market", market_order.message()),
        ("cancel", cancel_message("near:testnet", "perps.zeroflux.testnet", "alice.testnet", &oh, 1_800_000_000_000_000_000)),
        ("min_nonce", min_nonce_message("near:testnet", "perps.zeroflux.testnet", "alice.testnet", 100)),
        ("withdraw", withdraw_message("near:testnet", "perps.zeroflux.testnet", "alice.testnet", "usdc.zeroflux.testnet", u128::MAX, 3)),
        ("margin_add", margin_message("near:testnet", "perps.zeroflux.testnet", "alice.testnet", "OPUS-PERP", 5_000_000, 4)),
        ("margin_remove", margin_message("near:testnet", "perps.zeroflux.testnet", "alice.testnet", "OPUS-PERP", -5_000_000, 5)),
        ("auth", auth_message("near:testnet", "perps.zeroflux.testnet", "alice.testnet", 1_700_000_000_004)),
    ];
    let print = OraclePrint {
        index_id: "or-model-spend/example/model-20260101".into(),
        version: "ZFX-OR-MODEL-SPEND-USD-v1".into(),
        value_scale: 6,
        value: 42_500_000,
        source_start_ms: 1_767_225_600_000,
        source_end_ms: 1_767_312_000_000,
        observed_at_ms: 1_767_340_800_000,
        published_at_ms: 1_767_340_801_000,
        valid_until_ms: 1_767_484_800_000,
        source_hash: [0x11; 32],
        formula_hash: [0x22; 32],
        status: OracleStatus::Fresh,
        data_status: DataStatus::Ok,
        finality: Finality::Final,
        revision: 1,
        prev_print_hash: [0; 32],
    };
    let pb = print.encode().unwrap();
    let v = json!({
        "seed_hex": "07".repeat(32),
        "public_key": format!("ed25519:{}", bs58::encode(k.public()).into_string()),
        "policy_hash": hex32(&policy),
        "order_fields": {
            "chainId": order.chain_id, "clearingContract": order.clearing_contract, "accountId": order.account, "marketId": order.market,
            "side": "BUY", "type": "LIMIT", "price": "1250", "quantity": "12", "reduceOnly": false, "postOnly": true, "timeInForce": "GTC",
            "nonce": "7", "expiryNs": "1800000000000000000", "clientOrderId": "42", "maxFeeBps": 10, "policyHash": hex32(&policy)
        },
        "order_hash": hex32(&oh),
        "messages": msgs.iter().map(|(n, m)| json!({
            "name": n, "message": String::from_utf8(m.clone()).unwrap(), "sha256": hexs(&c.sha256(m)),
            "signature_hex": hexs(&k.sign(m)),
        })).collect::<Vec<_>>(),
        "oracle_print": {
            "fields": {"indexId": print.index_id, "version": print.version, "valueScale": 6, "value": "42500000",
                "sourceStartMs": "1767225600000", "sourceEndMs": "1767312000000", "observedAtMs": "1767340800000",
                "publishedAtMs": "1767340801000", "validUntilMs": "1767484800000", "sourceHash": hex32(&[0x11; 32]),
                "formulaHash": hex32(&[0x22; 32]), "status": "FRESH", "dataStatus": "OK", "finality": "FINAL", "revision": 1,
                "prevPrintHash": hex32(&[0; 32])},
            "canonical_hex": hexs(&pb), "print_hash": hexs(&c.sha256(&pb)), "signature_hex": hexs(&k.sign(&pb)),
        },
    });
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/sdk-ts/test/clearing_vectors.json");
    let text = serde_json::to_string_pretty(&v).unwrap() + "\n";
    if std::env::var("ZFX_PERPS_WRITE_VECTORS").is_ok() {
        std::fs::write(path, &text).unwrap();
    }
    assert_eq!(std::fs::read_to_string(path).expect("run with ZFX_PERPS_WRITE_VECTORS=1"), text);
}
