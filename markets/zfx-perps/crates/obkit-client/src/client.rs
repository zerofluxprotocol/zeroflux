use crate::{Error, Signer};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssetInfo {
    pub id: u16,
    pub symbol: String,
    pub token: String,
    pub decimals: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MarketInfo {
    pub id: u16,
    pub symbol: String,
    pub base: String,
    pub quote: String,
    pub base_token: String,
    pub quote_token: String,
    pub base_lot: String,
    pub quote_tick: String,
    pub min_qty: u64,
    pub max_qty: u64,
    pub max_price: u64,
    pub maker_fee_bps: u16,
    pub taker_fee_bps: u16,
    pub max_open_orders: u32,
    /// "spot" or "perp".
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub perp: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Info {
    pub settlement_contract: String,
    pub fee_account: String,
    pub assets: Vec<AssetInfo>,
    pub markets: Vec<MarketInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct BookSnapshot {
    pub market: String,
    pub seq: u64,
    pub bids: Vec<(u64, u64)>,
    pub asks: Vec<(u64, u64)>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Trade {
    pub trade_id: u64,
    pub price: u64,
    pub qty: u64,
    pub taker_side: String,
    pub ts_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Ack {
    Ok,
    Duplicate,
    /// `status`: resting | filled | canceled.
    Placed {
        order_id: u64,
        status: String,
        filled: u64,
        remaining: u64,
    },
    Canceled {
        order_id: u64,
        remaining: u64,
    },
    CanceledAll {
        count: u32,
    },
    WithdrawQueued {
        nonce: u64,
    },
    BatchesCut {
        first: u64,
        count: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct BalanceView {
    pub token: String,
    pub symbol: String,
    pub total: String,
    pub held: String,
    pub available: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct OpenOrder {
    pub order_id: u64,
    pub market: String,
    pub side: String,
    pub price: u64,
    pub remaining: u64,
    pub client_id: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PositionView {
    pub market: String,
    /// Signed lots: positive long, negative short.
    pub size: i64,
    pub entry_notional: String,
    /// Average entry price in ticks.
    pub entry_price: u64,
    pub mark: u64,
    pub unrealized_pnl: String,
}

/// Cross-margin summary in collateral atoms (decimal strings, may be negative).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct MarginView {
    pub equity: String,
    pub initial: String,
    pub maintenance: String,
    pub free: String,
    pub perp_cash: String,
    pub settled_perp_value: String,
}

impl MarginView {
    pub fn equity(&self) -> i128 {
        self.equity.parse().unwrap_or(0)
    }
    pub fn free(&self) -> i128 {
        self.free.parse().unwrap_or(0)
    }
    pub fn maintenance(&self) -> i128 {
        self.maintenance.parse().unwrap_or(0)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AccountView {
    pub account: String,
    pub balances: Vec<BalanceView>,
    pub open_orders: Vec<OpenOrder>,
    pub withdraw_nonce: u64,
    #[serde(default)]
    pub positions: Vec<PositionView>,
    #[serde(default)]
    pub margin: MarginView,
}

impl AccountView {
    pub fn position(&self, market: &str) -> i64 {
        self.positions.iter().find(|p| p.market == market).map_or(0, |p| p.size)
    }
}

/// `/v1/perp/{market}`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PerpInfo {
    pub market: String,
    pub mark: u64,
    pub index: u64,
    pub open_interest: u64,
    pub last_funding_rate_ppm: i64,
    pub cum_funding_per_lot: String,
    pub funding_interval_ms: u64,
    pub params: serde_json::Value,
    #[serde(default)]
    pub oracle: serde_json::Value,
}

impl AccountView {
    /// Total balance of a token (smallest unit), 0 if absent.
    pub fn total(&self, token: &str) -> u128 {
        self.balances
            .iter()
            .find(|b| b.token == token)
            .and_then(|b| b.total.parse().ok())
            .unwrap_or(0)
    }
    pub fn available(&self, token: &str) -> u128 {
        self.balances
            .iter()
            .find(|b| b.token == token)
            .and_then(|b| b.available.parse().ok())
            .unwrap_or(0)
    }
}

/// An order to place. Prices are ticks and quantities lots (see `MarketInfo`).
#[derive(Clone, Debug)]
pub struct Order {
    pub market: String,
    pub side: String,
    pub price: u64,
    pub qty: u64,
    pub tif: String,
    pub stp: String,
    pub client_id: u64,
}

impl Order {
    pub fn limit(market: &str, side: &str, price: u64, qty: u64) -> Self {
        Order {
            market: market.into(),
            side: side.into(),
            price,
            qty,
            tif: "gtc".into(),
            stp: "cancel_taker".into(),
            client_id: 0,
        }
    }
    pub fn ioc(market: &str, side: &str, price: u64, qty: u64) -> Self {
        Self::limit(market, side, price, qty).tif("ioc")
    }
    pub fn tif(mut self, tif: &str) -> Self {
        self.tif = tif.into();
        self
    }
    pub fn stp(mut self, stp: &str) -> Self {
        self.stp = stp.into();
        self
    }
    pub fn client_id(mut self, id: u64) -> Self {
        self.client_id = id;
        self
    }
}

async fn decode<T: DeserializeOwned>(r: reqwest::Response) -> Result<T, Error> {
    let status = r.status();
    let body = r.bytes().await?;
    if !status.is_success() {
        let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
        return Err(Error::Api {
            status: status.as_u16(),
            code: v["error"].as_str().unwrap_or("http_error").into(),
            message: v["message"].as_str().unwrap_or(&String::from_utf8_lossy(&body)).into(),
        });
    }
    serde_json::from_slice(&body).map_err(|e| Error::Decode(format!("{e}: {}", String::from_utf8_lossy(&body))))
}

/// Trading client for one account.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    pub account: String,
    pub signer: Signer,
}

impl Client {
    pub fn new(base: &str, account: &str, signer: Signer) -> Self {
        Client {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').into(),
            account: account.into(),
            signer,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        decode(self.http.get(format!("{}{path}", self.base)).send().await?).await
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T, Error> {
        decode(self.http.post(format!("{}{path}", self.base)).json(body).send().await?).await
    }

    pub async fn info(&self) -> Result<Info, Error> {
        self.get("/v1/info").await
    }

    pub async fn health(&self) -> Result<Value, Error> {
        self.get("/v1/health").await
    }

    pub async fn book(&self, market: &str, depth: usize) -> Result<BookSnapshot, Error> {
        self.get(&format!("/v1/book/{market}?depth={depth}")).await
    }

    pub async fn perp(&self, market: &str) -> Result<PerpInfo, Error> {
        self.get(&format!("/v1/perp/{market}")).await
    }

    pub async fn trades(&self, market: &str, limit: usize) -> Result<Vec<Trade>, Error> {
        let v: Value = self.get(&format!("/v1/trades/{market}?limit={limit}")).await?;
        serde_json::from_value(v["trades"].clone()).map_err(|e| Error::Decode(e.to_string()))
    }

    /// Build the signed JSON body for an order without sending it (useful for load tests).
    pub fn signed_order(&self, o: &Order, ts_ms: u64) -> Value {
        let msg = obkit_wire::OrderMsg {
            account: &self.account,
            market: &o.market,
            side: &o.side,
            price: o.price,
            qty: o.qty,
            tif: &o.tif,
            stp: &o.stp,
            client_id: o.client_id,
            ts_ms,
        }
        .to_bytes();
        json!({"account": self.account, "market": o.market, "side": o.side, "price": o.price, "qty": o.qty, "tif": o.tif, "stp": o.stp,
               "client_id": o.client_id, "ts": ts_ms, "public_key": self.signer.public_key(), "signature": self.signer.sign_b64(&msg)})
    }

    pub async fn place(&self, o: &Order) -> Result<Ack, Error> {
        self.post("/v1/orders", &self.signed_order(o, now_ms())).await
    }

    pub async fn cancel(&self, market: &str, order_id: u64) -> Result<Ack, Error> {
        self.cancel_inner(market, order_id, 0).await
    }

    pub async fn cancel_client_id(&self, market: &str, client_id: u64) -> Result<Ack, Error> {
        self.cancel_inner(market, 0, client_id).await
    }

    async fn cancel_inner(&self, market: &str, order_id: u64, client_id: u64) -> Result<Ack, Error> {
        let ts = now_ms();
        let sig = self
            .signer
            .sign_b64(&obkit_wire::cancel_msg(&self.account, market, order_id, client_id, ts));
        self.post(
            "/v1/orders/cancel",
            &json!({"account": self.account, "market": market, "order_id": order_id, "client_id": client_id, "ts": ts, "public_key": self.signer.public_key(), "signature": sig}),
        )
        .await
    }

    pub async fn cancel_all(&self, market: Option<&str>) -> Result<Ack, Error> {
        let ts = now_ms();
        let sig = self.signer.sign_b64(&obkit_wire::cancel_all_msg(&self.account, market, ts));
        self.post(
            "/v1/orders/cancel_all",
            &json!({"account": self.account, "market": market, "ts": ts, "public_key": self.signer.public_key(), "signature": sig}),
        )
        .await
    }

    /// Request a withdrawal. The signature is also what the settlement contract verifies before
    /// paying out, so `contract` must be the settlement contract id (`Info::settlement_contract`).
    pub async fn withdraw(&self, contract: &str, token: &str, amount: u128, nonce: u64) -> Result<Ack, Error> {
        let sig = self
            .signer
            .sign_b64(&obkit_wire::withdraw_msg(contract, &self.account, token, amount, nonce));
        self.post(
            "/v1/withdraw",
            &json!({"account": self.account, "token": token, "amount": amount.to_string(), "nonce": nonce, "public_key": self.signer.public_key(), "signature": sig}),
        )
        .await
    }

    /// Withdraw with the next nonce, reading the contract id and current nonce from the node.
    pub async fn withdraw_next(&self, token: &str, amount: u128) -> Result<Ack, Error> {
        let info = self.info().await?;
        let nonce = self.account().await?.withdraw_nonce + 1;
        self.withdraw(&info.settlement_contract, token, amount, nonce).await
    }

    pub async fn account(&self) -> Result<AccountView, Error> {
        let ts = now_ms();
        let sig = self.signer.sign_b64(&obkit_wire::auth_msg(&self.account, ts));
        let r = self
            .http
            .get(format!("{}/v1/account/{}", self.base, self.account))
            .header("x-obkit-key", self.signer.public_key())
            .header("x-obkit-ts", ts.to_string())
            .header("x-obkit-signature", sig)
            .send()
            .await?;
        decode(r).await
    }

    /// WebSocket auth message for the private account channel.
    pub fn ws_auth(&self) -> Value {
        let ts = now_ms();
        json!({"op": "auth", "account": self.account, "ts": ts, "public_key": self.signer.public_key(), "signature": self.signer.sign_b64(&obkit_wire::auth_msg(&self.account, ts))})
    }
}

/// Operator client for `/internal/*` (settlement adapter, admin tools).
#[derive(Clone)]
pub struct Admin {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Admin {
    pub fn new(base: &str, token: &str) -> Self {
        Admin {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').into(),
            token: token.into(),
        }
    }

    async fn req<T: DeserializeOwned>(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<T, Error> {
        let mut r = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        decode(r.send().await?).await
    }

    pub async fn status(&self) -> Result<Value, Error> {
        self.req(reqwest::Method::GET, "/internal/status", None).await
    }

    /// Forward settlement-contract events (contract JSON format plus `seq` as a string).
    pub async fn chain_events(&self, events: Vec<Value>) -> Result<Value, Error> {
        self.req(reqwest::Method::POST, "/internal/chain_events", Some(json!({"events": events})))
            .await
    }

    /// Unconfirmed settlement batches, in `settle_batch` argument format.
    pub async fn batches(&self) -> Result<Value, Error> {
        self.req(reqwest::Method::GET, "/internal/batches", None).await
    }

    pub async fn cut(&self) -> Result<Value, Error> {
        self.req(reqwest::Method::POST, "/internal/cut", None).await
    }

    /// Manual oracle update (tests, external oracles).
    pub async fn set_mark(&self, market: &str, mark: u64) -> Result<Value, Error> {
        self.req(
            reqwest::Method::POST,
            "/internal/mark",
            Some(json!({"market": market, "mark": mark})),
        )
        .await
    }

    pub async fn funding(&self, market: &str) -> Result<Value, Error> {
        self.req(reqwest::Method::POST, "/internal/funding", Some(json!({"market": market})))
            .await
    }

    pub async fn halt(&self, market: &str, halted: bool) -> Result<Value, Error> {
        self.req(
            reqwest::Method::POST,
            "/internal/halt",
            Some(json!({"market": market, "halted": halted})),
        )
        .await
    }
}
