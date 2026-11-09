//! Deterministic test-vector generation for ZFX-FP8C. The same JSON is
//! consumed by the C++/CUDA/HIP miner self-test and the Python reference.

use crate::hash::{sha256, to_hex};
use crate::header::BlockHeader;
use crate::pow::*;
use serde_json::{json, Value};

pub fn sample_header(profile: &Profile) -> BlockHeader {
    BlockHeader {
        version: profile.id,
        chain_id: 0x5a46_5801,
        height: 33,
        parent: sha256(&[b"vector-parent"]),
        tx_root: sha256(&[b"vector-txroot"]),
        state_root: [0; 32],
        timestamp: 1_800_000_000,
        target: [0xff; 32],
        nonce: 0,
    }
}

pub const VECTOR_NONCES: [u64; 5] = [0, 1, 2, 0x0123_4567_89ab_cdef, u64::MAX];

fn i8_hex(v: &[i8]) -> String {
    hex::encode(v.iter().map(|x| *x as u8).collect::<Vec<u8>>())
}

fn i32_le_bytes(v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn profile_vectors(profile: &Profile, full_out: bool) -> Value {
    let anchor = sha256(&[b"vector-anchor"]);
    let seed = epoch_seed(&anchor, profile);
    let w = Weights::generate_par(profile, &seed, 8);
    // PoW vectors are frozen on the v1 header bytes (consensus v1); the PoW
    // function is identical in v2, only the header layout changed.
    let header = sample_header(profile);
    let hwn = header.encode_v1_without_nonce();
    let c = challenge_bytes(&hwn);
    let layer_digests: Vec<String> = (0..w.layers.len()).map(|l| to_hex(&w.layer_digest(l))).collect();
    let first_rows: Vec<String> = w
        .layers
        .iter()
        .zip(profile.layers())
        .map(|(lw, (k, _, _))| i8_hex(&lw[..k.min(64)]))
        .collect();
    let attempts: Vec<Value> = VECTOR_NONCES
        .iter()
        .map(|&n| {
            let (t, d) = trace(&w, &c, n);
            let act_digests: Vec<String> = t
                .acts
                .iter()
                .map(|a| to_hex(&sha256(&[&a.iter().map(|x| *x as u8).collect::<Vec<u8>>()])))
                .collect();
            let mut v = json!({
                "nonce": n.to_string(),
                "attempt_id": to_hex(&attempt_id(&c, n)),
                "x0_hex": i8_hex(&t.x0),
                "act_sha256": act_digests,
                "out_sha256": to_hex(&sha256(&[&i32_le_bytes(&t.out)])),
                "out_head": t.out[..8].to_vec(),
                "digest": to_hex(&d),
            });
            if full_out {
                v["out"] = json!(t.out);
            }
            v
        })
        .collect();
    json!({
        "profile": {
            "id": profile.id, "name": profile.name, "d_in": profile.d_in,
            "hidden": profile.hidden, "n_hidden": profile.n_hidden, "d_out": profile.d_out,
            "shift_in": profile.shift_in, "shift_hidden": profile.shift_hidden,
            "macs_per_attempt": profile.macs_per_attempt(),
        },
        "anchor_hash": to_hex(&anchor),
        "epoch_seed": to_hex(&seed),
        "weight_layer_sha256": layer_digests,
        "weight_first_row_hex": first_rows,
        "header_without_nonce_hex": hex::encode(hwn),
        "challenge": to_hex(&c),
        "attempts": attempts,
    })
}

/// Arithmetic edge cases: extreme accumulators and requantization inputs.
pub fn edge_vectors() -> Value {
    let k = 4096usize;
    let cases: Vec<(&str, Vec<i8>, Vec<i8>)> = vec![
        ("max_positive", vec![16; k], vec![16; k]),
        ("max_negative", vec![16; k], vec![-16; k]),
        ("alternating", (0..k).map(|i| if i % 2 == 0 { 16 } else { -16 }).collect(), vec![16; k]),
        ("ones", vec![1; k], vec![-1; k]),
        ("ramp", (0..k).map(|i| byte_to_val(i as u8)).collect(), (0..k).map(|i| byte_to_val((i * 7) as u8)).collect()),
    ];
    let dots: Vec<Value> = cases
        .into_iter()
        .map(|(name, a, b)| {
            let d = dot_i8(&a, &b);
            json!({"name": name, "k": k, "a_hex": i8_hex(&a), "b_hex": i8_hex(&b), "dot": d,
                   "fp32_bits": format!("{:08x}", (d as f32).to_bits())})
        })
        .collect();
    let ys: Vec<i32> = vec![0, 1, -1, 255, 256, -256, 1 << 20, -(1 << 20), 123456, -987654, i32::MAX, i32::MIN];
    let rq: Vec<Value> = ys
        .iter()
        .map(|&y| json!({"y": y, "s0": requant(y, 0), "s2": requant(y, 2)}))
        .collect();
    let byte_map: Vec<i8> = (0..=255u8).map(byte_to_val).collect();
    json!({"dot_cases": dots, "requant_cases": rq, "byte_to_val": byte_map})
}

pub fn all_vectors() -> Value {
    json!({
        "format": "zfx-fp8c-vectors-v0.1",
        "edge": edge_vectors(),
        "dev": profile_vectors(&FP8C_V01_DEV, true),
        "v01": profile_vectors(&FP8C_V01, false),
    })
}

#[cfg(test)]
mod tests {
    /// Consensus-drift guard: regenerated vectors must equal the published file.
    #[test]
    fn vectors_match_published() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/vectors/fp8c_v0.1.json");
        let published: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let now = super::all_vectors();
        assert_eq!(now, published, "consensus output changed; regenerate vectors only for an intentional new version");
    }
}

/// Consensus v2 golden vectors (header v2, tx, bridge digests, difficulty,
/// state root). Signatures are hedged (randomized), so the file is
/// generated once and frozen; tests check every recorded value recomputes.
pub fn consensus_v2_vectors() -> Value {
    use crate::bridge::{admin_digest, lockbox_address, native_asset_id, release_digest, AdminAction, AdminOp, Release};
    use crate::difficulty::{next_target, DifficultyParams};
    use crate::state::{ConsensusParams, State};
    use crate::tx::{address_to_string, P2mrKeys};
    use crate::U256;
    let cid = 0x5a46_0002u32;
    let h = BlockHeader {
        version: 2,
        chain_id: cid,
        height: 7,
        parent: sha256(&[b"v2-parent"]),
        tx_root: sha256(&[b"v2-txroot"]),
        state_root: sha256(&[b"v2-stateroot"]),
        timestamp: 1_790_000_000,
        target: U256::MAX.shr(10).to_be_bytes(),
        nonce: 42,
    };
    let alice = P2mrKeys::from_seed(&[0xa1; 32]);
    let plain = alice.sign_tx(cid, [0xb2; 32], 1234, 1000, 0, 0).unwrap();
    let memo = alice.sign_tx_memo(cid, lockbox_address(cid), 5000, 1000, 1, 0, Some("alice.test.near".into())).unwrap();
    let rel = Release { epoch: 0, withdrawal_id: [0x11; 32], near_burn_ref: "wzfx.test.near:7:abc".into(), recipient: [0x22; 32], amount: 777, expiry_height: 900, approvals: vec![] };
    let adm = AdminOp { epoch: 0, op_nonce: 3, action: AdminAction::Pause, approvals: vec![] };
    let dp = DifficultyParams { solvetime_rule: crate::difficulty::SolvetimeRule::RunningMax, target_spacing_secs: 30, pow_limit: U256::MAX.shr(2), fixed: false };
    let g = U256::MAX.shr(12);
    let hist: Vec<(u64, U256)> = (0..31u64).map(|i| (1_790_000_000 + i * 20, g)).collect();
    let params = ConsensusParams { chain_id: cid, subsidy: crate::monetary::SubsidySchedule::Constant { per_block: 50_0000_0000 }, coinbase_maturity: 5, bridge: None };
    let st = State::genesis(&params, &[(alice.address(), 1_000_000)]).unwrap();
    json!({
        "format": "zfx-consensus-v2-vectors",
        "header_v2": {"fields": h, "encoded_hex": hex::encode(h.encode()), "hash": to_hex(&h.hash()),
                      "challenge": to_hex(&challenge(&h))},
        "tx_plain": {"json": plain, "core_hex": hex::encode(plain.core_bytes()), "txid": to_hex(&plain.txid()), "wtxid": to_hex(&plain.wtxid()),
                     "from": address_to_string(&plain.from)},
        "tx_bridge_deposit": {"json": memo, "core_hex": hex::encode(memo.core_bytes()), "txid": to_hex(&memo.txid()), "wtxid": to_hex(&memo.wtxid())},
        "bridge": {"chain_id": cid, "native_asset_id": native_asset_id(cid), "lockbox": address_to_string(&lockbox_address(cid)),
                   "release": rel, "release_digest": to_hex(&release_digest(cid, &rel)),
                   "admin_pause": adm, "admin_digest": to_hex(&admin_digest(cid, &adm))},
        "difficulty": {"spacing": 30, "genesis_target": g.to_hex(), "history_spacing_secs": 20,
                       "next_target": next_target(&dp, &g, &hist).to_hex()},
        "state": {"allocation": [address_to_string(&alice.address()), 1_000_000], "chain_id": cid, "maturity": 5, "root": to_hex(&st.root())},
    })
}

#[cfg(test)]
mod v2_tests {
    use super::*;
    use crate::bridge::{admin_digest, release_digest, AdminOp, Release};
    use crate::tx::Transaction;

    #[test]
    fn consensus_v2_vectors_recompute() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/vectors/consensus_v2.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("run `zfx-research vectors-v2`")).unwrap();
        let h: BlockHeader = serde_json::from_value(v["header_v2"]["fields"].clone()).unwrap();
        assert_eq!(hex::encode(h.encode()), v["header_v2"]["encoded_hex"]);
        assert_eq!(BlockHeader::decode(&hex::decode(v["header_v2"]["encoded_hex"].as_str().unwrap()).unwrap()).unwrap(), h);
        assert_eq!(to_hex(&h.hash()), v["header_v2"]["hash"]);
        assert_eq!(to_hex(&challenge(&h)), v["header_v2"]["challenge"]);
        for k in ["tx_plain", "tx_bridge_deposit"] {
            let t: Transaction = serde_json::from_value(v[k]["json"].clone()).unwrap();
            t.verify_authorization().unwrap();
            assert_eq!(hex::encode(t.core_bytes()), v[k]["core_hex"]);
            assert_eq!(to_hex(&t.txid()), v[k]["txid"]);
            assert_eq!(to_hex(&t.wtxid()), v[k]["wtxid"]);
        }
        let cid = v["bridge"]["chain_id"].as_u64().unwrap() as u32;
        let r: Release = serde_json::from_value(v["bridge"]["release"].clone()).unwrap();
        assert_eq!(to_hex(&release_digest(cid, &r)), v["bridge"]["release_digest"]);
        let a: AdminOp = serde_json::from_value(v["bridge"]["admin_pause"].clone()).unwrap();
        assert_eq!(to_hex(&admin_digest(cid, &a)), v["bridge"]["admin_digest"]);
        // deterministic parts recompute exactly from the generator
        let fresh = consensus_v2_vectors();
        for k in ["header_v2", "difficulty", "state"] {
            assert_eq!(fresh[k], v[k], "{}", k);
        }
        assert_eq!(fresh["bridge"]["release_digest"], v["bridge"]["release_digest"]);
        assert_eq!(fresh["bridge"]["lockbox"], v["bridge"]["lockbox"]);
    }
}
