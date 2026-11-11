//! HTTP API of a clearing-mode node (`openapi.yaml`). Public market data, signed trading actions
//! and the adapter's internal endpoints. Every amount is a decimal string.

use crate::api::{ApiError, App, bad, reply_ack};
use crate::auth::now_ns;
use crate::dto::{b64, key_from_str, key_to_str, sig_from_str};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use zfx_perps_clearing::ledger::{self, Now};
use zfx_perps_clearing::store::{Crypto, MemStore, NativeCrypto};
use zfx_perps_clearing::wire::{self, OrderType, Sig64, SignedOrder, TimeInForce};
use zfx_perps_clearing::{MarketState, MarketStatus, Op, Position};
use zfx_perps_engine::{ClearingEvent, Command, Engine, Stp};

pub fn routes(app: App) -> Router<App> {
    let _ = app;
    Router::new()
        .route("/v1/markets", get(markets))
        .route("/v1/oracle/{market}", get(oracle))
        .route("/v1/funding/{market}", get(funding))
        .route("/v1/estimate", post(estimate))
        .route("/v1/orders", post(place))
        .route("/v1/orders/cancel", post(cancel))
        .route("/v1/orders/min_nonce", post(min_nonce))
        .route("/v1/withdraw", post(withdraw))
        .route("/v1/margin", post(margin))
        .route("/v1/portfolio/{account}", get(portfolio))
        .route("/v1/openapi.yaml", get(openapi))
        .route("/internal/clearing/events", post(chain_events))
        .route("/internal/clearing/batches", get(batches))
        .route("/internal/oracle", post(post_print))
        .route("/internal/clearing/funding", post(funding_now))
}

fn cl(app: &App) -> &crate::config::ClearingCfg {
    app.cfg.clearing.as_ref().expect("clearing mode")
}

fn bad_req(e: impl Into<String>) -> ApiError {
    bad(e)
}

fn u(s: &str, what: &str) -> Result<u64, ApiError> {
    s.parse().map_err(|_| bad_req(format!("{what}: decimal string")))
}

pub fn market_json(m: &MarketState, now: u64, grace: u64) -> Value {
    let p = &m.params;
    let params = json!({
        "market_id": p.market_id, "market_type": format!("{:?}", p.market_type), "index_id": p.index_id, "index_version": p.index_version,
        "value_scale": p.value_scale, "index_units_per_tick": p.index_units_per_tick.to_string(), "tick_value": p.tick_value.to_string(),
        "min_qty": p.min_qty.to_string(), "max_qty": p.max_qty.to_string(), "max_price": p.max_price.to_string(),
        "initial_margin_bps": p.initial_margin_bps, "maintenance_margin_bps": p.maintenance_margin_bps, "max_leverage": p.max_leverage,
        "max_open_interest": p.max_open_interest.to_string(), "max_open_interest_notional": p.max_open_interest_notional.to_string(), "max_position": p.max_position.to_string(),
        "maker_fee_bps": p.maker_fee_bps, "taker_fee_bps": p.taker_fee_bps, "liq_fee_bps": p.liq_fee_bps,
        "liq_slippage_bps": p.liq_slippage_bps, "price_band_bps": p.price_band_bps,
        "oracle_max_age_sec": p.oracle_max_age_sec.to_string(), "funding_interval_sec": p.funding_interval_sec.to_string(),
        "funding_divisor": p.funding_divisor, "max_funding_ppm": p.max_funding_ppm,
        "print_close_only_from_sec": p.print_close_only_from_sec, "print_lag_days": p.print_lag_days, "mark_limit_bps": p.mark_limit_bps, "halt_jump_bps": p.halt_jump_bps, "print_move_close_only_bps": p.print_move_close_only_bps,
    });
    json!({
        "market_id": p.market_id, "params": params, "policy_hash": wire::hex32(&m.policy_hash),
        "status": m.status.name(), "effective_status": ledger::effective_status(m, Now::at(now), grace).name(),
        "mark": m.mark.to_string(), "index_ticks": m.index_ticks.to_string(), "print_move_flag": m.print_move_flag, "mark_fresh": ledger::mark_fresh(m, Now::at(now), grace),
        "open_interest": m.open_interest.to_string(), "bad_debt": m.bad_debt.to_string(),
    })
}

fn position_json(m: &MarketState, p: &Position) -> Value {
    json!({
        "market": m.params.market_id, "size": p.size.to_string(), "margin": p.margin.to_string(),
        "equity": ledger::equity(m, p, m.mark).to_string(), "mark": m.mark.to_string(),
        "initial_margin": ledger::initial_margin(m, p.size.unsigned_abs(), m.mark).to_string(),
        "maintenance_margin": ledger::maintenance_margin(m, p.size.unsigned_abs(), m.mark).to_string(),
        "liquidation_price": ledger::liquidation_price(m, p).map(|x| x.to_string()),
        "funding_owed": ledger::funding_owed(m, p).to_string(),
    })
}

fn ledger_view(s: &MemStore, account: &str) -> Value {
    let positions: Vec<Value> = s
        .positions_of(account)
        .filter_map(|(mid, p)| s.markets.get(mid).map(|m| position_json(m, p)))
        .collect();
    json!({"free": s.accounts.get(account).map_or(0, |a| a.free).to_string(), "positions": positions})
}

// ---------------- public ----------------

async fn markets(State(app): State<App>) -> Result<Json<Value>, ApiError> {
    let v = app
        .seq
        .query(|e| {
            let c = e.clearing().unwrap();
            let now = now_ns();
            let ms: Vec<Value> = c.spec.markets.values().map(|m| market_json(m, now, c.cfg.settle_grace_ns)).collect();
            json!({"chain_id": c.cfg.chain_id, "clearing_contract": c.cfg.contract, "collateral": c.cfg.collateral, "markets": ms})
        })
        .await?;
    Ok(Json(v))
}

async fn oracle(State(app): State<App>, Path(market): Path<String>) -> Result<Json<Value>, ApiError> {
    let v = app
        .seq
        .query(move |e| {
            let c = e.clearing().unwrap();
            let m = c.spec.markets.get(&market)?;
            let now = now_ns();
            let rec = c.prints.get(&market);
            let print = m.print.as_ref().map(|p| {
                json!({
                    "hash": wire::hex32(&p.hash), "value": p.value.to_string(), "status": p.status.name(), "data_status": p.data_status.name(),
                    "source_start_ms": p.source_start_ms.to_string(), "source_end_ms": p.source_end_ms.to_string(),
                    "published_at_ms": p.published_at_ms.to_string(), "valid_until_ms": p.valid_until_ms.to_string(), "revision": p.revision,
                    "canonical_hex": rec.map(|r| wire::hex(&r.print)),
                    "signatures": rec.map(|r| r.sigs.iter().map(|(k, s)| json!({"public_key": key_to_str(k), "signature_hex": wire::hex(&s.0)})).collect::<Vec<_>>()),
                })
            });
            Some(json!({
                "market": market, "index_id": m.params.index_id, "index_version": m.params.index_version, "value_scale": m.params.value_scale,
                "index_units_per_tick": m.params.index_units_per_tick.to_string(),
                "mark": m.mark.to_string(), "index_ticks": m.index_ticks.to_string(), "print_move_flag": m.print_move_flag, "mark_fresh": ledger::mark_fresh(m, Now::at(now), c.cfg.settle_grace_ns),
                "effective_status": ledger::effective_status(m, Now::at(now), c.cfg.settle_grace_ns).name(),
                "oracle_max_age_sec": m.params.oracle_max_age_sec.to_string(),
                "reporters": c.spec.globals.reporters.iter().map(key_to_str).collect::<Vec<_>>(), "quorum": c.spec.globals.quorum,
                "print": print,
            }))
        })
        .await?;
    v.map(Json).ok_or_else(|| ApiError::not_found("unknown_market"))
}

async fn funding(State(app): State<App>, Path(market): Path<String>) -> Result<Json<Value>, ApiError> {
    let v = app
        .seq
        .query(move |e| {
            let c = e.clearing().unwrap();
            let m = c.spec.markets.get(&market)?;
            let vwap = if m.fund_qty > 0 { m.fund_px_qty * 1_000_000 / m.fund_qty } else { 0 };
            let hist: Vec<Value> = c
                .funding_log
                .get(&market)
                .map(|l| l.iter().rev().map(|(t, r, p)| json!({"ts_ns": t.to_string(), "rate_ppm": r, "pay_per_lot": p.to_string()})).collect())
                .unwrap_or_default();
            Some(json!({
                "market": market, "funding_interval_sec": m.params.funding_interval_sec.to_string(),
                "funding_divisor": m.params.funding_divisor, "max_funding_ppm": m.params.max_funding_ppm,
                "last_funding_ns": m.last_funding_ns.to_string(),
                "next_funding_ns": (m.last_funding_ns + m.params.funding_interval_sec * 1_000_000_000).to_string(),
                "last_rate_ppm": m.last_funding_rate_ppm, "cum_funding": m.cum_funding.to_string(),
                "window_vwap_e6": vwap.to_string(), "window_qty": m.fund_qty.to_string(), "history": hist,
            }))
        })
        .await?;
    v.map(Json).ok_or_else(|| ApiError::not_found("unknown_market"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EstimateReq {
    account_id: Option<String>,
    market_id: String,
    side: String,
    price: String,
    quantity: String,
}

async fn estimate(State(app): State<App>, Json(r): Json<EstimateReq>) -> Result<Json<Value>, ApiError> {
    let side = wire::Side::parse(&r.side).ok_or_else(|| bad_req("side: BUY|SELL"))?;
    let (price, qty) = (u(&r.price, "price")?, u(&r.quantity, "quantity")?);
    let v = app
        .seq
        .query(move |e| {
            let s = &e.clearing().unwrap().spec;
            let m = s.markets.get(&r.market_id)?.clone();
            let acct = r.account_id.clone().unwrap_or_default();
            let mut p = s.positions.get(&(r.market_id.clone(), acct.clone())).cloned().unwrap_or_default();
            let free = s.accounts.get(&acct).map_or(0, |a| a.free);
            ledger::touch(&m, &mut p);
            let new = p.size + side.sign() * qty as i64;
            let n = m.notional(qty, price);
            let fee = ledger::fee(n, m.params.taker_fee_bps);
            p.margin -= side.sign() as i128 * n as i128 + fee as i128;
            p.size = new;
            let eq = ledger::equity(&m, &p, m.mark);
            let im = ledger::initial_margin(&m, new.unsigned_abs(), m.mark);
            let need = (im as i128 - eq).max(0) as u128;
            p.margin += need as i128;
            Some(json!({"initial_margin": im.to_string(), "fee": fee.to_string(), "required_free": need.to_string(),
                "free": free.to_string(), "ok": need <= free, "mark": m.mark.to_string(),
                "liquidation_price": ledger::liquidation_price(&m, &p).map(|x| x.to_string())}))
        })
        .await?;
    v.map(Json).ok_or_else(|| ApiError::not_found("unknown_market"))
}

async fn openapi() -> (StatusCode, [(&'static str, &'static str); 1], &'static str) {
    (StatusCode::OK, [("content-type", "application/yaml")], include_str!("../../../openapi.yaml"))
}

// ---------------- signed ----------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedOrderDto {
    pub chain_id: String,
    pub clearing_contract: String,
    pub account_id: String,
    pub market_id: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String,
    pub price: String,
    pub quantity: String,
    pub reduce_only: bool,
    pub post_only: bool,
    pub time_in_force: String,
    pub nonce: String,
    pub expiry_ns: String,
    pub client_order_id: String,
    pub max_fee_bps: u16,
    pub policy_hash: String,
    pub public_key: String,
    pub signature: String,
}

impl SignedOrderDto {
    pub fn to_order(&self) -> Result<SignedOrder, String> {
        let p = |s: &str, w: &str| s.parse::<u64>().map_err(|_| format!("{w}: decimal string"));
        Ok(SignedOrder {
            chain_id: self.chain_id.clone(),
            clearing_contract: self.clearing_contract.clone(),
            account: self.account_id.clone(),
            market: self.market_id.clone(),
            side: wire::Side::parse(&self.side).ok_or("side: BUY|SELL")?,
            order_type: OrderType::parse(&self.order_type).ok_or("type: LIMIT|MARKET")?,
            price: p(&self.price, "price")?,
            qty: p(&self.quantity, "quantity")?,
            reduce_only: self.reduce_only,
            post_only: self.post_only,
            tif: TimeInForce::parse(&self.time_in_force).ok_or("timeInForce: GTC|IOC|FOK")?,
            nonce: p(&self.nonce, "nonce")?,
            expiry_ns: p(&self.expiry_ns, "expiryNs")?,
            client_order_id: p(&self.client_order_id, "clientOrderId")?,
            max_fee_bps: self.max_fee_bps,
            policy_hash: wire::parse_hex32(&self.policy_hash).ok_or("policyHash: 64 lower-case hex chars")?,
            public_key: key_from_str(&self.public_key)?,
            signature: Sig64(sig_from_str(&self.signature)?),
        })
    }
}

async fn place(State(app): State<App>, Json(r): Json<SignedOrderDto>) -> Result<Json<Value>, ApiError> {
    let o = r.to_order().map_err(bad_req)?;
    if !o.well_formed() {
        return Err(bad_req("malformed order"));
    }
    // Signature and key registration, before the order reaches the sequencer.
    app.auth.check(&app.shared, &o.account, &r.public_key, &r.signature, &o.message(), None)?;
    let hash = wire::hex32(&NativeCrypto.sha256(&o.message()));
    let res = app
        .seq
        .submit(Command::PlaceSigned {
            order: Box::new(o),
            stp: Stp::CancelTaker,
            ts_ns: now_ns(),
        })
        .await?;
    let mut v = reply_ack(res)?;
    v["order_hash"] = json!(hash);
    for k in ["order_id", "filled", "remaining"] {
        if let Some(x) = v.get(k).and_then(|x| x.as_u64()) {
            v[k] = json!(x.to_string());
        }
    }
    if let Some(s) = v.get("status").and_then(|s| s.as_str()).map(String::from) {
        v["status"] = json!(s.to_lowercase());
    }
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelReq {
    account_id: String,
    order_hash: String,
    expiry_ns: String,
    public_key: String,
    signature: String,
    #[serde(default)]
    relay_on_chain: bool,
}

async fn cancel(State(app): State<App>, Json(r): Json<CancelReq>) -> Result<Json<Value>, ApiError> {
    let c = cl(&app);
    let h = wire::parse_hex32(&r.order_hash).ok_or_else(|| bad_req("orderHash"))?;
    let exp = u(&r.expiry_ns, "expiryNs")?;
    let msg = wire::cancel_message(&c.chain_id, &c.contract, &r.account_id, &h, exp);
    let (account, pk, sig) = app.auth.check(&app.shared, &r.account_id, &r.public_key, &r.signature, &msg, None)?;
    if r.relay_on_chain {
        let op = Op::Cancel {
            account: r.account_id.clone(),
            order_hash: h,
            expiry_ns: exp,
            public_key: pk,
            signature: Sig64(sig),
        };
        return Ok(Json(reply_ack(app.seq.submit(Command::ClearingAction { op: Box::new(op), ts_ns: now_ns() }).await?)?));
    }
    let id = app
        .seq
        .query(move |e| e.clearing_order_id(&h))
        .await?
        .ok_or_else(|| ApiError::conflict("unknown_order", "no resting order with that hash"))?;
    let market = app.seq.query(move |e| e.order_market.get(&id).copied()).await?.unwrap_or(0);
    Ok(Json(reply_ack(app.seq.submit(Command::Cancel { account, market, order_id: id }).await?)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MinNonceReq {
    account_id: String,
    min_nonce: String,
    public_key: String,
    signature: String,
}

async fn min_nonce(State(app): State<App>, Json(r): Json<MinNonceReq>) -> Result<Json<Value>, ApiError> {
    let c = cl(&app);
    let n = u(&r.min_nonce, "minNonce")?;
    let msg = wire::min_nonce_message(&c.chain_id, &c.contract, &r.account_id, n);
    let (_, pk, sig) = app.auth.check(&app.shared, &r.account_id, &r.public_key, &r.signature, &msg, None)?;
    let op = Op::MinNonce {
        account: r.account_id,
        min_nonce: n,
        public_key: pk,
        signature: Sig64(sig),
    };
    Ok(Json(reply_ack(app.seq.submit(Command::ClearingAction { op: Box::new(op), ts_ns: now_ns() }).await?)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WithdrawReq {
    account_id: String,
    amount: String,
    nonce: String,
    public_key: String,
    signature: String,
}

async fn withdraw(State(app): State<App>, Json(r): Json<WithdrawReq>) -> Result<Json<Value>, ApiError> {
    let c = cl(&app);
    let amount: u128 = r.amount.parse().map_err(|_| bad_req("amount"))?;
    let n = u(&r.nonce, "nonce")?;
    let msg = wire::withdraw_message(&c.chain_id, &c.contract, &r.account_id, &c.collateral, amount, n);
    let (_, pk, sig) = app.auth.check(&app.shared, &r.account_id, &r.public_key, &r.signature, &msg, None)?;
    let op = Op::Withdraw {
        account: r.account_id,
        amount,
        nonce: n,
        public_key: pk,
        signature: Sig64(sig),
    };
    Ok(Json(reply_ack(app.seq.submit(Command::ClearingAction { op: Box::new(op), ts_ns: now_ns() }).await?)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarginReq {
    account_id: String,
    market_id: String,
    delta: String,
    nonce: String,
    public_key: String,
    signature: String,
}

async fn margin(State(app): State<App>, Json(r): Json<MarginReq>) -> Result<Json<Value>, ApiError> {
    let c = cl(&app);
    let delta: i128 = r.delta.parse().map_err(|_| bad_req("delta"))?;
    let n = u(&r.nonce, "nonce")?;
    let msg = wire::margin_message(&c.chain_id, &c.contract, &r.account_id, &r.market_id, delta, n);
    let (_, pk, sig) = app.auth.check(&app.shared, &r.account_id, &r.public_key, &r.signature, &msg, None)?;
    let op = Op::AdjustMargin {
        account: r.account_id,
        market: r.market_id,
        delta,
        nonce: n,
        public_key: pk,
        signature: Sig64(sig),
    };
    Ok(Json(reply_ack(app.seq.submit(Command::ClearingAction { op: Box::new(op), ts_ns: now_ns() }).await?)?))
}

async fn portfolio(State(app): State<App>, Path(name): Path<String>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    let c = cl(&app);
    let hv = |k: &str| {
        h.get(k)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .ok_or_else(|| bad_req(format!("missing header {k}")))
    };
    let ts: u64 = hv("x-zfx-perps-ts")?.parse().map_err(|_| bad_req("x-zfx-perps-ts"))?;
    let msg = wire::auth_message(&c.chain_id, &c.contract, &name, ts);
    let (id, _, _) = app
        .auth
        .check(&app.shared, &name, &hv("x-zfx-perps-key")?, &hv("x-zfx-perps-signature")?, &msg, Some(ts))?;
    let v = app.seq.query(move |e| portfolio_json(e, id, &name)).await?;
    Ok(Json(v))
}

pub fn portfolio_json(e: &Engine, id: u32, name: &str) -> Value {
    let c = e.clearing().unwrap();
    let a = c.spec.accounts.get(name).cloned().unwrap_or_default();
    let orders: Vec<Value> = e
        .open_orders(id)
        .into_iter()
        .filter_map(|(oid, _, _, _, rem, _)| {
            c.orders.get(&oid).map(|o| {
                json!({"order_id": oid.to_string(), "order_hash": wire::hex32(&NativeCrypto.sha256(&o.message())), "market": o.market,
                       "side": o.side.as_str(), "price": o.price.to_string(), "quantity": o.qty.to_string(), "remaining": rem.to_string(),
                       "nonce": o.nonce.to_string(), "expiry_ns": o.expiry_ns.to_string(), "client_order_id": o.client_order_id.to_string()})
            })
        })
        .collect();
    json!({"account": name, "pending": ledger_view(&c.spec, name), "settled": ledger_view(&c.confirmed, name),
           "min_nonce": a.min_nonce.to_string(), "action_nonce": a.action_nonce.to_string(), "open_orders": orders,
           "last_settled_batch": c.last_settled.to_string(), "unconfirmed_batches": c.unconfirmed.len(), "pending_ops": c.pending.len()})
}

// ---------------- internal ----------------

fn admin(app: &App, h: &HeaderMap) -> Result<(), ApiError> {
    crate::api::admin_check(app, h)
}

/// Contract event JSON (as emitted by the clearing contract) to an engine event.
pub fn parse_event(v: &Value) -> Result<ClearingEvent, String> {
    let s = |k: &str| v[k].as_str().map(String::from).ok_or(format!("{k} missing"));
    let n = |k: &str| -> Result<u64, String> { s(k)?.parse().map_err(|_| format!("{k}: number")) };
    let n128 = |k: &str| -> Result<u128, String> { s(k)?.parse().map_err(|_| format!("{k}: number")) };
    let ty = v["type"].as_str().ok_or("type missing")?;
    Ok(match ty {
        "deposit" => ClearingEvent::Deposit { deposit_id: n("deposit_id")?, account: s("account")?, amount: n128("amount")? },
        "deposit_reclaimed" => ClearingEvent::DepositReclaimed { deposit_id: n("deposit_id")? },
        "key_added" => ClearingEvent::KeyAdded {
            account: s("account")?,
            public_key: key_from_str(&s("public_key")?)?,
            expires_ns: n("expires_ns")?,
            can_withdraw: v["can_withdraw"].as_bool().unwrap_or(true),
        },
        "key_removed" => ClearingEvent::KeyRemoved { account: s("account")?, public_key: key_from_str(&s("public_key")?)? },
        "order_canceled" => ClearingEvent::OrderCanceled {
            account: s("account")?,
            order_hash: wire::parse_hex32(&s("order_hash")?).ok_or("order_hash")?,
            expiry_ns: n("expiry_ns")?,
        },
        "min_nonce" => ClearingEvent::MinNonce { account: s("account")?, min_nonce: n("min_nonce")? },
        "forced_withdrawal" => ClearingEvent::ForcedWithdrawal { request_id: n("request_id")?, account: s("account")?, amount: n128("amount")? },
        "withdrawal_reverted" => ClearingEvent::WithdrawalReverted { account: s("account")?, amount: n128("amount")? },
        "batch_settled" => {
            let rejected = v["rejected"]
                .as_array()
                .ok_or("rejected")?
                .iter()
                .map(|r| {
                    let i = r[0].as_u64().ok_or("rejected index")? as u32;
                    let reason: zfx_perps_clearing::Reject = serde_json::from_value(r[1].clone()).map_err(|e| format!("reason: {e}"))?;
                    Ok((i, reason))
                })
                .collect::<Result<Vec<_>, String>>()?;
            ClearingEvent::BatchSettled { batch_id: n("batch_id")?, block_ns: n("block_ns")?, rejected }
        }
        "market_updated" => ClearingEvent::MarketUpdated {
            params: serde_json::from_value(v["params"].clone()).map_err(|e| format!("params: {e}"))?,
            policy_hash: wire::parse_hex32(&s("policy_hash")?).ok_or("policy_hash")?,
            status: status_from(&s("status")?)?,
            created_ns: n("created_ns")?,
        },
        "reporters_updated" => ClearingEvent::ReportersUpdated {
            reporters: v["reporters"]
                .as_array()
                .ok_or("reporters")?
                .iter()
                .map(|k| key_from_str(k.as_str().unwrap_or("")))
                .collect::<Result<_, _>>()?,
            quorum: v["quorum"].as_u64().ok_or("quorum")? as u8,
        },
        "frozen" => ClearingEvent::Frozen,
        "initialized" => ClearingEvent::Initialized {
            chain_id: s("chain_id")?,
            contract_id: s("contract_id")?,
            collateral: s("collateral")?,
            insurance_account: s("insurance_account")?,
            settle_grace_ns: n("settle_grace_ns")?,
            max_order_ttl_ns: n("max_order_ttl_ns")?,
        },
        _ => ClearingEvent::Other,
    })
}

fn status_from(s: &str) -> Result<MarketStatus, String> {
    Ok(match s {
        "PROPOSED" => MarketStatus::Proposed,
        "ACTIVE" => MarketStatus::Active,
        "CLOSE_ONLY" => MarketStatus::CloseOnly,
        "HALTED" => MarketStatus::Halted,
        "SETTLING" => MarketStatus::Settling,
        "CLOSED" => MarketStatus::Closed,
        _ => return Err(format!("status {s}")),
    })
}

#[derive(Deserialize)]
struct EventsReq {
    /// `[seq, event]` pairs from the contract's `get_events`.
    events: Vec<(String, Value)>,
}

async fn chain_events(State(app): State<App>, h: HeaderMap, Json(r): Json<EventsReq>) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let mut results = vec![];
    for (seq, ev) in r.events {
        let seq: u64 = seq.parse().map_err(|_| bad_req("seq"))?;
        let event = parse_event(&ev).map_err(bad_req)?;
        let res = app.seq.submit(Command::ClearingChain { seq, event, ts_ns: now_ns() }).await?;
        let gap = matches!(res, Err(zfx_perps_engine::Reject::ChainEventGap));
        results.push(json!({"seq": seq, "result": match res { Ok(a) => serde_json::to_value(a).unwrap(), Err(e) => json!({"error": crate::api::reject_code(e)}) }}));
        if gap {
            break;
        }
    }
    Ok(Json(json!({"results": results})))
}

async fn batches(State(app): State<App>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let v = app
        .seq
        .query(|e| {
            let c = e.clearing().unwrap();
            let b: Vec<Value> = c
                .unconfirmed
                .iter()
                .map(|cb| {
                    json!({"id": cb.batch.id.to_string(), "ops": cb.batch.ops.len(), "orders": cb.batch.orders.len(),
                       "borsh_b64": b64(&borsh::to_vec(&cb.batch).unwrap()), "predicted_rejected": cb.predicted_rejected})
                })
                .collect();
            json!({"last_settled": c.last_settled.to_string(), "pending_ops": c.pending.len(), "batches": b, "diverged": c.diverged})
        })
        .await?;
    Ok(Json(v))
}

#[derive(Clone, Deserialize)]
pub struct PrintReq {
    pub market: String,
    pub print_hex: String,
    pub signatures: Vec<SigDto>,
}

#[derive(Clone, Deserialize)]
pub struct SigDto {
    pub public_key: String,
    pub signature_hex: String,
}

pub fn print_op(r: &PrintReq) -> Result<Op, String> {
    let print = wire::unhex(&r.print_hex).ok_or("print_hex")?;
    let sigs = r
        .signatures
        .iter()
        .map(|s| {
            let k = key_from_str(&s.public_key)?;
            let b = wire::unhex(&s.signature_hex).ok_or("signature_hex")?;
            Ok((k, Sig64(b.try_into().map_err(|_| "signature: 64 bytes")?)))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Op::Oracle {
        market: r.market.clone(),
        print,
        sigs,
    })
}

/// Submit a reporter-signed print (from the index publisher or an operator relaying it).
async fn post_print(State(app): State<App>, h: HeaderMap, Json(r): Json<PrintReq>) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let op = print_op(&r).map_err(bad_req)?;
    Ok(Json(reply_ack(app.seq.submit(Command::ClearingAction { op: Box::new(op), ts_ns: now_ns() }).await?)?))
}

#[derive(Deserialize)]
struct FundingReq {
    market: String,
}

async fn funding_now(State(app): State<App>, h: HeaderMap, Json(r): Json<FundingReq>) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let op = Op::Funding { market: r.market };
    Ok(Json(reply_ack(app.seq.submit(Command::ClearingAction { op: Box::new(op), ts_ns: now_ns() }).await?)?))
}

#[derive(Deserialize)]
pub struct DepthQ {
    pub depth: Option<usize>,
}

pub fn _unused(_: Query<DepthQ>) {}

pub fn published_seq(app: &App) -> u64 {
    app.shared.published_seq.load(Ordering::Relaxed)
}
