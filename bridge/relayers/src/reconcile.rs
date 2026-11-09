//! Full bridge reconciliation, computed from authoritative chain state only
//! (ZeroFlux node RPC + NEAR views at finality "final").
//!
//! Invariant (spec B3): minted - burned <= locked - released.
//! Exact accounting identity, with explicit in-flight liabilities:
//!   (locked - released) - (minted - burned) == pending_deposits - pending_releases
//! where pending_deposits = locked deposits not yet minted and
//! pending_releases = finalized burns not yet released natively.
//! Orphans (mint without a backing on-chain deposit, release without a
//! NEAR burn) are critical and make the report fail.

use crate::near::NearClient;
use crate::zfx::ZfxClient;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

fn num(v: &Value) -> u128 {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64().map(|x| x as u128)).unwrap_or(0)
}

pub fn all_burns(near: &NearClient, contract: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut from = 0u64;
    loop {
        let page = near.view(contract, "burns_since", json!({"from_nonce": from.to_string(), "limit": 100}), "final")?;
        let arr = page.as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            break;
        }
        from += arr.len() as u64;
        out.extend(arr);
    }
    Ok(out)
}

pub fn reconcile(zfx: &ZfxClient, near: &NearClient, contract: &str, chain_id: u32) -> Result<Value, String> {
    let info = zfx.call("zfx_bridge_info", json!({}))?;
    let deposits = zfx.call("zfx_bridge_list_deposits", json!({"from_height": 0, "limit": 1000}))?.as_array().cloned().unwrap_or_default();
    let releases = zfx.call("zfx_bridge_list_releases", json!({}))?.as_array().cloned().unwrap_or_default();
    let ns = near.view(contract, "bridge_state", json!({}), "final")?;
    let burns = all_burns(near, contract)?;
    let locked = num(&info["locked_total"]);
    let released = num(&info["released_total"]);
    let lockbox = info["lockbox_balance"].as_u64().unwrap_or(0) as u128;
    let minted = num(&ns["total_minted"]);
    let burned = num(&ns["total_burned"]);
    let supply = num(&ns["total_supply"]);
    let mut minted_on_chain = 0u128;
    let mut pending_deposits = 0u128;
    let mut pending_dep_list = Vec::new();
    for d in &deposits {
        let id = d["deposit_id"].as_str().unwrap_or("");
        let amt = d["amount"].as_u64().unwrap_or(0) as u128;
        if near.view(contract, "is_minted", json!({"deposit_id": id}), "final")? == true {
            minted_on_chain += amt;
        } else {
            pending_deposits += amt;
            pending_dep_list.push(json!({"deposit_id": id, "amount": amt.to_string(), "confirmations": d["confirmations"]}));
        }
    }
    let released_ids: HashMap<String, u128> = releases.iter().map(|r| (r["withdrawal_id"].as_str().unwrap_or("").to_string(), r["amount"].as_u64().unwrap_or(0) as u128)).collect();
    let burn_ids: HashSet<String> = burns.iter().map(|b| b["withdrawal_id"].as_str().unwrap_or("").to_string()).collect();
    let mut pending_releases = 0u128;
    let mut pending_rel_list = Vec::new();
    for b in &burns {
        let wid = b["withdrawal_id"].as_str().unwrap_or("");
        if !released_ids.contains_key(wid) {
            pending_releases += num(&b["amount"]);
            pending_rel_list.push(json!({"withdrawal_id": wid, "amount": b["amount"], "nonce": b["nonce"]}));
        }
    }
    let mut orphans = Vec::new();
    if minted > minted_on_chain {
        orphans.push(json!({"kind": "mint_without_onchain_deposit", "unbacked": (minted - minted_on_chain).to_string(),
            "note": "a minted deposit is no longer on the native best chain (deep reorg) or was never there"}));
    }
    for (wid, amt) in &released_ids {
        if !burn_ids.contains(wid) {
            orphans.push(json!({"kind": "release_without_near_burn", "withdrawal_id": wid, "amount": amt.to_string()}));
        }
    }
    let native_outstanding = locked as i128 - released as i128;
    let wrapped_outstanding = minted as i128 - burned as i128;
    let identity = native_outstanding - wrapped_outstanding == pending_deposits as i128 - pending_releases as i128;
    let invariant_ok = wrapped_outstanding <= native_outstanding;
    let custody_ok = lockbox as i128 == native_outstanding;
    let supply_ok = supply as i128 == wrapped_outstanding;
    let ok = invariant_ok && identity && custody_ok && supply_ok && orphans.is_empty();
    Ok(json!({
        "ok": ok,
        "invariant_ok": invariant_ok,
        "accounting_identity_ok": identity,
        "native_custody_ok": custody_ok,
        "wrapped_supply_ok": supply_ok,
        "native": {"chain_id": chain_id, "locked_total": locked.to_string(), "released_total": released.to_string(), "lockbox_balance": lockbox.to_string(),
                   "deposits": deposits.len(), "releases": releases.len(), "paused": info["paused"]},
        "near": {"contract": contract, "total_minted": minted.to_string(), "total_burned": burned.to_string(), "total_supply": supply.to_string(),
                 "burns": burns.len(), "paused": ns["paused"], "epoch": ns["epoch"]},
        "pending_deposits": {"total": pending_deposits.to_string(), "items": pending_dep_list},
        "pending_releases": {"total": pending_releases.to_string(), "items": pending_rel_list},
        "orphans": orphans,
        "fees": {"bridge_fee": "0", "note": "v1 charges no bridge fee; native tx fees go to miners and never touch the lockbox"},
    }))
}
