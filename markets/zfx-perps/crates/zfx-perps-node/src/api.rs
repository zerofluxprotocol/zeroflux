//! HTTP and WebSocket gateway.
//!
//! Public:   GET /v1/health, /v1/info, /v1/book/{market}, /v1/trades/{market}
//! Signed:   POST /v1/orders, /v1/orders/cancel, /v1/orders/cancel_all, /v1/withdraw
//!           GET  /v1/account/{account}  (headers x-zfx-perps-key, x-zfx-perps-ts, x-zfx-perps-signature)
//! Stream:   GET  /v1/ws
//! Internal: /internal/* with `Authorization: Bearer <admin_token>` (settlement adapter, admin)

use crate::auth::{Auth, AuthError, now_ns};
use crate::config::Config;
use crate::dto::*;
use crate::sequencer::{Handle, Reply, SubmitError};
use crate::state::{Channel, Shared};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use zfx_perps_engine::{ChainEvent, Command, NewOrder, Reject, Side, Stp, Tif};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::broadcast::error::RecvError;

#[derive(Clone)]
pub struct App {
    pub cfg: Arc<Config>,
    pub seq: Handle,
    pub shared: Arc<Shared>,
    pub auth: Arc<Auth>,
}

pub fn router(app: App) -> Router {
    if app.cfg.clearing.is_some() {
        return Router::new()
            .route("/v1/health", get(health))
            .route("/v1/info", get(info))
            .route("/v1/book/{market}", get(book))
            .route("/v1/trades/{market}", get(trades))
            .route("/v1/ws", get(ws))
            .route("/internal/status", get(status))
            .route("/internal/cut", post(cut))
            .merge(crate::clearing_api::routes(app.clone()))
            .layer(tower_http::cors::CorsLayer::permissive())
            .with_state(app);
    }
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/info", get(info))
        .route("/v1/book/{market}", get(book))
        .route("/v1/trades/{market}", get(trades))
        .route("/v1/orders", post(place))
        .route("/v1/orders/cancel", post(cancel))
        .route("/v1/orders/cancel_all", post(cancel_all))
        .route("/v1/withdraw", post(withdraw))
        .route("/v1/account/{account}", get(account))
        .route("/v1/ws", get(ws))
        .route("/internal/status", get(status))
        .route("/internal/chain_events", post(chain_events))
        .route("/internal/batches", get(batches))
        .route("/internal/cut", post(cut))
        .route("/internal/halt", post(halt))
        .route("/internal/mark", post(set_mark))
        .route("/internal/funding", post(funding))
        .route("/v1/perp/{market}", get(perp_info))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(app)
}

pub struct ApiError(pub StatusCode, pub String, pub String);

impl ApiError {
    pub fn not_found(code: &str) -> Self {
        ApiError(StatusCode::NOT_FOUND, code.into(), code.into())
    }
    pub fn conflict(code: &str, msg: &str) -> Self {
        ApiError(StatusCode::CONFLICT, code.into(), msg.into())
    }
}

/// An engine reply as JSON, or a 409 with the reject code.
pub fn reply_ack(r: Reply) -> Result<Value, ApiError> {
    reply(r).map(|Json(v)| v)
}

pub fn admin_check(app: &App, h: &HeaderMap) -> Result<(), ApiError> {
    admin(app, h)
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1, "message": self.2}))).into_response()
    }
}

pub fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, "bad_request".into(), msg.into())
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        let code = if e == AuthError::RateLimited {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::UNAUTHORIZED
        };
        let msg = match &e {
            AuthError::BadKey(m) => m.clone(),
            _ => e.code().to_string(),
        };
        ApiError(code, e.code().into(), msg)
    }
}

impl From<SubmitError> for ApiError {
    fn from(e: SubmitError) -> Self {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{e:?}").to_lowercase(),
            "sequencer unavailable".into(),
        )
    }
}

pub fn reject_code(r: Reject) -> String {
    match r {
        Reject::Clearing(c) => serde_json::to_value(c).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
        _ => serde_json::to_value(r)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default(),
    }
}

fn reply(r: Reply) -> Result<Json<Value>, ApiError> {
    match r {
        Ok(ack) => Ok(Json(serde_json::to_value(ack).unwrap())),
        Err(rej) => Err(ApiError(StatusCode::CONFLICT, reject_code(rej), format!("{rej:?}"))),
    }
}

fn market_id(app: &App, symbol: &str) -> Result<u16, ApiError> {
    app.shared
        .dir
        .read()
        .market_by_symbol
        .get(symbol)
        .copied()
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "unknown_market".into(), symbol.into()))
}

fn parse_side(s: &str) -> Result<Side, ApiError> {
    match s {
        "buy" => Ok(Side::Buy),
        "sell" => Ok(Side::Sell),
        _ => Err(bad("side must be buy|sell")),
    }
}

fn parse_tif(s: &str) -> Result<Tif, ApiError> {
    Ok(match s {
        "gtc" => Tif::Gtc,
        "ioc" => Tif::Ioc,
        "fok" => Tif::Fok,
        "post_only" => Tif::PostOnly,
        _ => return Err(bad("tif must be gtc|ioc|fok|post_only")),
    })
}

fn parse_stp(s: &str) -> Result<Stp, ApiError> {
    Ok(match s {
        "cancel_taker" => Stp::CancelTaker,
        "cancel_maker" => Stp::CancelMaker,
        "cancel_both" => Stp::CancelBoth,
        _ => return Err(bad("stp must be cancel_taker|cancel_maker|cancel_both")),
    })
}

// ---------- public ----------

async fn health(State(app): State<App>) -> Json<Value> {
    let s = &app.seq.stats;
    Json(json!({
        "ok": true,
        "seq": s.last_seq.load(Ordering::Relaxed),
        "durable_seq": s.durable_seq.load(Ordering::Relaxed),
        "published_seq": app.shared.published_seq.load(Ordering::Relaxed),
        "fsyncs": s.fsyncs.load(Ordering::Relaxed),
    }))
}

async fn info(State(app): State<App>) -> Json<Value> {
    if let Some(c) = &app.cfg.clearing {
        return Json(json!({"mode": "clearing", "chain_id": c.chain_id, "clearing_contract": c.contract, "collateral": c.collateral,
            "collateral_decimals": c.collateral_decimals, "insurance_account": c.insurance_account}));
    }
    let d = app.shared.dir.read();
    let assets: Vec<Value> = d
        .assets
        .iter()
        .map(|a| json!({"id": a.id, "symbol": a.symbol, "token": a.chain_ref, "decimals": a.decimals}))
        .collect();
    let markets: Vec<Value> = d
        .markets
        .iter()
        .map(|m| {
            json!({"id": m.id, "symbol": m.symbol, "base": d.assets[m.base as usize].symbol, "quote": d.assets[m.quote as usize].symbol,
                   "base_token": d.assets[m.base as usize].chain_ref, "quote_token": d.assets[m.quote as usize].chain_ref,
                   "base_lot": m.base_lot.to_string(), "quote_tick": m.quote_tick.to_string(), "min_qty": m.min_qty, "max_qty": m.max_qty,
                   "max_price": m.max_price, "maker_fee_bps": m.maker_fee_bps, "taker_fee_bps": m.taker_fee_bps, "max_open_orders": m.max_open_orders,
                   "kind": if m.perp.is_some() { "perp" } else { "spot" }, "perp": m.perp})
        })
        .collect();
    Json(
        json!({"settlement_contract": app.cfg.settlement_contract, "fee_account": app.cfg.fee_account, "assets": assets, "markets": markets}),
    )
}

#[derive(Deserialize)]
struct DepthQ {
    depth: Option<usize>,
    limit: Option<usize>,
}

fn book_snapshot(app: &App, m: u16, depth: usize) -> Value {
    let b = app.shared.book(m).expect("market exists");
    let b = b.read();
    let bids: Vec<(u64, u64)> = b.bids.iter().rev().take(depth).map(|(p, q)| (*p, *q)).collect();
    let asks: Vec<(u64, u64)> = b.asks.iter().take(depth).map(|(p, q)| (*p, *q)).collect();
    let symbol = app.shared.dir.read().markets[m as usize].symbol.clone();
    json!({"channel": "book", "type": "snapshot", "market": symbol, "seq": b.seq, "bids": bids, "asks": asks})
}

async fn book(State(app): State<App>, Path(market): Path<String>, Query(q): Query<DepthQ>) -> Result<Json<Value>, ApiError> {
    let m = market_id(&app, &market)?;
    Ok(Json(book_snapshot(&app, m, q.depth.unwrap_or(50).min(5_000))))
}

async fn trades(State(app): State<App>, Path(market): Path<String>, Query(q): Query<DepthQ>) -> Result<Json<Value>, ApiError> {
    let m = market_id(&app, &market)?;
    let b = app.shared.book(m).unwrap();
    let b = b.read();
    let n = q.limit.unwrap_or(100).min(crate::state::RECENT_TRADES);
    let t: Vec<&TradeDto> = b.trades.iter().rev().take(n).collect();
    Ok(Json(json!({"market": market, "trades": t})))
}

// ---------- signed ----------

async fn place(State(app): State<App>, Json(r): Json<OrderReq>) -> Result<Json<Value>, ApiError> {
    let market = market_id(&app, &r.market)?;
    let (side, tif, stp) = (parse_side(&r.side)?, parse_tif(&r.tif)?, parse_stp(&r.stp)?);
    let msg = zfx_perps_wire::OrderMsg {
        account: &r.account,
        market: &r.market,
        side: &r.side,
        price: r.price,
        qty: r.qty,
        tif: &r.tif,
        stp: &r.stp,
        client_id: r.client_id,
        ts_ms: r.ts,
    }
    .to_bytes();
    let (account, _, _) = app
        .auth
        .check(&app.shared, &r.account, &r.public_key, &r.signature, &msg, Some(r.ts))?;
    let cmd = Command::Place(NewOrder {
        account,
        market,
        side,
        price: r.price,
        qty: r.qty,
        tif,
        stp,
        client_id: r.client_id,
        ts_ns: now_ns(),
    });
    reply(app.seq.submit(cmd).await?)
}

async fn cancel(State(app): State<App>, Json(r): Json<CancelReq>) -> Result<Json<Value>, ApiError> {
    let market = market_id(&app, &r.market)?;
    if (r.order_id == 0) == (r.client_id == 0) {
        return Err(bad("exactly one of order_id / client_id"));
    }
    let msg = zfx_perps_wire::cancel_msg(&r.account, &r.market, r.order_id, r.client_id, r.ts);
    let (account, _, _) = app
        .auth
        .check(&app.shared, &r.account, &r.public_key, &r.signature, &msg, Some(r.ts))?;
    let cmd = if r.order_id != 0 {
        Command::Cancel {
            account,
            market,
            order_id: r.order_id,
        }
    } else {
        Command::CancelByClientId {
            account,
            market,
            client_id: r.client_id,
        }
    };
    reply(app.seq.submit(cmd).await?)
}

async fn cancel_all(State(app): State<App>, Json(r): Json<CancelAllReq>) -> Result<Json<Value>, ApiError> {
    let market = match &r.market {
        Some(m) => Some(market_id(&app, m)?),
        None => None,
    };
    let msg = zfx_perps_wire::cancel_all_msg(&r.account, r.market.as_deref(), r.ts);
    let (account, _, _) = app
        .auth
        .check(&app.shared, &r.account, &r.public_key, &r.signature, &msg, Some(r.ts))?;
    reply(app.seq.submit(Command::CancelAll { account, market }).await?)
}

async fn withdraw(State(app): State<App>, Json(r): Json<WithdrawReq>) -> Result<Json<Value>, ApiError> {
    let asset = app
        .shared
        .dir
        .read()
        .asset_by_token
        .get(&r.token)
        .copied()
        .ok_or_else(|| bad("unknown token"))?;
    let amount: u128 = r.amount.parse().map_err(|_| bad("amount must be a decimal string"))?;
    let msg = zfx_perps_wire::withdraw_msg(&app.cfg.settlement_contract, &r.account, &r.token, amount, r.nonce);
    let (account, pk, sig) = app.auth.check(&app.shared, &r.account, &r.public_key, &r.signature, &msg, None)?;
    reply(
        app.seq
            .submit(Command::Withdraw {
                account,
                asset,
                amount,
                nonce: r.nonce,
                public_key: pk,
                signature: sig.to_vec(),
            })
            .await?,
    )
}

async fn account(State(app): State<App>, Path(name): Path<String>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    let hv = |k: &str| {
        h.get(k)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .ok_or_else(|| bad(format!("missing header {k}")))
    };
    let ts: u64 = hv("x-zfx-perps-ts")?.parse().map_err(|_| bad("x-zfx-perps-ts"))?;
    let msg = zfx_perps_wire::auth_msg(&name, ts);
    let (id, _, _) = app
        .auth
        .check(&app.shared, &name, &hv("x-zfx-perps-key")?, &hv("x-zfx-perps-signature")?, &msg, Some(ts))?;
    let v = app
        .seq
        .query(move |e| {
            let a = &e.accounts[id as usize];
            let balances: Vec<Value> = e
                .assets
                .iter()
                .map(|s| {
                    let b = a.balance(s.id);
                    json!({"token": s.chain_ref, "symbol": s.symbol, "total": b.total.to_string(), "held": b.held.to_string(), "available": b.available().to_string()})
                })
                .collect();
            let orders: Vec<Value> = e
                .open_orders(id)
                .into_iter()
                .map(|(oid, m, side, price, rem, cid)| json!({"order_id": oid, "market": e.markets[m as usize].spec.symbol, "side": side.as_str(), "price": price, "remaining": rem, "client_id": cid}))
                .collect();
            let positions: Vec<Value> = a
                .perp
                .positions
                .iter()
                .map(|(m, p)| {
                    let spec = &e.markets[*m as usize].spec;
                    let mark = e.perps.get(m).map_or(0, |s| s.mark);
                    let value = p.size as i128 * mark as i128 * spec.quote_tick as i128;
                    let entry_signed = if p.size > 0 { p.entry as i128 } else { -(p.entry as i128) };
                    json!({"market": spec.symbol, "size": p.size, "entry_notional": p.entry.to_string(),
                           "entry_price": (p.entry / (p.size.unsigned_abs() as u128 * spec.quote_tick)) as u64, "mark": mark,
                           "unrealized_pnl": (value - entry_signed).to_string()})
                })
                .collect();
            let ms = e.margin_summary(id);
            let margin = json!({"equity": ms.equity.to_string(), "initial": ms.initial.to_string(), "maintenance": ms.maintenance.to_string(),
                                "free": ms.free.to_string(), "perp_cash": a.perp.cash.to_string(), "settled_perp_value": a.perp.last_view.to_string()});
            json!({"account": a.name, "balances": balances, "open_orders": orders, "withdraw_nonce": a.withdraw_nonce, "positions": positions, "margin": margin})
        })
        .await?;
    Ok(Json(v))
}

// ---------- websocket ----------

async fn ws(State(app): State<App>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |s| ws_session(app, s))
}

async fn ws_session(app: App, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let mut feed = app.shared.out.subscribe();
    let mut books: HashMap<u16, u64> = HashMap::new();
    let mut trades: HashSet<u16> = HashSet::new();
    let mut perps: HashSet<u16> = HashSet::new();
    let mut account: Option<u32> = None;
    loop {
        tokio::select! {
            msg = rx.next() => {
                let Some(Ok(msg)) = msg else { return };
                let Message::Text(t) = msg else { continue };
                let resp = match serde_json::from_str::<Value>(&t) {
                    Ok(v) => ws_op(&app, &v, &mut books, &mut trades, &mut perps, &mut account),
                    Err(e) => vec![json!({"error": "bad_json", "message": e.to_string()})],
                };
                for r in resp {
                    if tx.send(Message::Text(r.to_string().into())).await.is_err() { return }
                }
            }
            out = feed.recv() => {
                match out {
                    Ok(o) => {
                        let deliver = match o.channel {
                            Channel::Book => o.market.and_then(|m| books.get_mut(&m)).is_some_and(|last| {
                                if o.seq > *last { *last = o.seq; true } else { false }
                            }),
                            Channel::Trades => o.market.is_some_and(|m| trades.contains(&m)),
                            Channel::Perp => o.market.is_some_and(|m| perps.contains(&m)),
                            Channel::Account => account.is_some() && o.account == account,
                        };
                        if deliver && tx.send(Message::Text(o.text.to_string().into())).await.is_err() { return }
                    }
                    Err(RecvError::Lagged(n)) => {
                        // Too slow: tell the client and re-snapshot every subscribed book.
                        let _ = tx.send(Message::Text(json!({"type": "lagged", "dropped": n}).to_string().into())).await;
                        for (m, last) in books.iter_mut() {
                            let snap = book_snapshot(&app, *m, 5_000);
                            *last = snap["seq"].as_u64().unwrap_or(0);
                            if tx.send(Message::Text(snap.to_string().into())).await.is_err() { return }
                        }
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        }
    }
}

fn ws_op(
    app: &App,
    v: &Value,
    books: &mut HashMap<u16, u64>,
    trades: &mut HashSet<u16>,
    perps: &mut HashSet<u16>,
    account: &mut Option<u32>,
) -> Vec<Value> {
    let op = v["op"].as_str().unwrap_or("");
    let market = || {
        v["market"]
            .as_str()
            .and_then(|s| app.shared.dir.read().market_by_symbol.get(s).copied())
    };
    match (op, v["channel"].as_str()) {
        ("subscribe", Some("book")) => match market() {
            Some(m) => {
                let snap = book_snapshot(app, m, v["depth"].as_u64().unwrap_or(5_000) as usize);
                books.insert(m, snap["seq"].as_u64().unwrap_or(0));
                vec![snap]
            }
            None => vec![json!({"error": "unknown_market"})],
        },
        ("subscribe", Some("perp")) => match market() {
            Some(m) if app.shared.dir.read().markets[m as usize].perp.is_some() || app.cfg.clearing.is_some() => {
                perps.insert(m);
                vec![json!({"op": "subscribed", "channel": "perp", "market": v["market"]})]
            }
            _ => vec![json!({"error": "unknown_market", "message": "not a perp market"})],
        },
        ("subscribe", Some("trades")) => match market() {
            Some(m) => {
                trades.insert(m);
                vec![json!({"op": "subscribed", "channel": "trades", "market": v["market"]})]
            }
            None => vec![json!({"error": "unknown_market"})],
        },
        ("unsubscribe", Some(ch)) => {
            if let Some(m) = market() {
                if ch == "perp" {
                    perps.remove(&m);
                } else if ch == "book" {
                    books.remove(&m);
                } else {
                    trades.remove(&m);
                }
            }
            vec![json!({"op": "unsubscribed", "channel": ch})]
        }
        ("auth", _) => {
            let (Some(acct), Some(ts), Some(pk), Some(sig)) = (
                v["account"].as_str(),
                v["ts"].as_u64(),
                v["public_key"].as_str(),
                v["signature"].as_str(),
            ) else {
                return vec![json!({"error": "bad_request", "message": "auth needs account, ts, public_key, signature"})];
            };
            let msg = match &app.cfg.clearing {
                Some(c) => zfx_perps_clearing::wire::auth_message(&c.chain_id, &c.contract, acct, ts),
                None => zfx_perps_wire::auth_msg(acct, ts),
            };
            match app.auth.check(&app.shared, acct, pk, sig, &msg, Some(ts))
            {
                Ok((id, _, _)) => {
                    *account = Some(id);
                    vec![json!({"op": "auth", "ok": true, "account": acct})]
                }
                Err(e) => vec![json!({"op": "auth", "ok": false, "error": e.code()})],
            }
        }
        _ => vec![json!({"error": "unknown_op"})],
    }
}

// ---------- internal ----------

fn admin(app: &App, h: &HeaderMap) -> Result<(), ApiError> {
    let ok = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == app.cfg.admin_token);
    if ok {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "unauthorized".into(),
            "admin token required".into(),
        ))
    }
}

async fn status(State(app): State<App>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let v = app
        .seq
        .query(|e| {
            match e.clearing() {
                Some(c) => json!({"mode": "clearing", "chain_cursor": e.chain_cursor, "last_settled": c.last_settled,
                    "next_batch_id": c.next_batch_id, "unconfirmed": c.unconfirmed.len(), "pending": !c.pending.is_empty(),
                    "accounts": e.accounts.len(), "diverged": c.diverged, "frozen": c.frozen}),
                None => json!({"chain_cursor": e.chain_cursor, "last_settled": e.settle.last_settled, "next_batch_id": e.settle.next_batch_id,
                           "unconfirmed": e.settle.unconfirmed.len(), "pending": !e.settle.is_empty(), "accounts": e.accounts.len()}),
            }
        })
        .await?;
    Ok(Json(v))
}

async fn chain_events(State(app): State<App>, h: HeaderMap, Json(r): Json<ChainEventsReq>) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let mut results = vec![];
    for e in &r.events {
        let ce = ChainEvent::try_from(e).map_err(bad)?;
        let seq = ce.seq();
        let res = app.seq.submit(Command::Chain(ce)).await?;
        let gap = matches!(res, Err(Reject::ChainEventGap));
        results.push(json!({"seq": seq, "result": match res { Ok(a) => serde_json::to_value(a).unwrap(), Err(r) => json!({"error": reject_code(r)}) }}));
        if gap {
            break;
        }
    }
    Ok(Json(json!({"results": results})))
}

async fn batches(State(app): State<App>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let b: Vec<BatchDto> = app.shared.batches.read().iter().map(BatchDto::from).collect();
    Ok(Json(
        json!({"last_settled": app.shared.last_settled.load(Ordering::Relaxed), "batches": b}),
    ))
}

async fn cut(State(app): State<App>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    app.seq.cut()?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct HaltReq {
    market: String,
    halted: bool,
}

async fn halt(State(app): State<App>, h: HeaderMap, Json(r): Json<HaltReq>) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let market = market_id(&app, &r.market)?;
    reply(app.seq.submit(Command::SetHalted { market, halted: r.halted }).await?)
}

// ---------- perps ----------

fn perp_market(app: &App, symbol: &str) -> Result<u16, ApiError> {
    let m = market_id(app, symbol)?;
    if app.shared.dir.read().markets[m as usize].perp.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_a_perp".into(), symbol.into()));
    }
    Ok(m)
}

async fn perp_info(State(app): State<App>, Path(market): Path<String>) -> Result<Json<Value>, ApiError> {
    let m = perp_market(&app, &market)?;
    let interval = app.cfg.funding_interval_ms;
    let v = app
        .seq
        .query(move |e| {
            let st = &e.perps[&m];
            json!({"market": e.markets[m as usize].spec.symbol, "mark": st.mark, "index": st.index, "open_interest": st.oi,
                   "last_funding_rate_ppm": st.last_rate_ppm, "cum_funding_per_lot": st.cum_funding_per_lot.to_string(),
                   "premium_samples": st.premium_n, "funding_interval_ms": interval, "params": e.markets[m as usize].spec.perp})
        })
        .await?;
    let mut v = v;
    v["oracle"] = app.shared.oracle.read().get(&m).cloned().unwrap_or(Value::Null);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct MarkReq {
    market: String,
    mark: u64,
    #[serde(default)]
    index: Option<u64>,
}

/// Manual oracle update (tests, or an external oracle process).
async fn set_mark(State(app): State<App>, h: HeaderMap, Json(r): Json<MarkReq>) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let market = perp_market(&app, &r.market)?;
    reply(
        app.seq
            .submit(Command::SetMark {
                market,
                mark: r.mark,
                index: r.index.unwrap_or(r.mark),
                ts_ns: now_ns(),
            })
            .await?,
    )
}

#[derive(Deserialize)]
struct FundingReq {
    market: String,
}

async fn funding(State(app): State<App>, h: HeaderMap, Json(r): Json<FundingReq>) -> Result<Json<Value>, ApiError> {
    admin(&app, &h)?;
    let market = perp_market(&app, &r.market)?;
    reply(app.seq.submit(Command::FundingTick { market, ts_ns: now_ns() }).await?)
}
