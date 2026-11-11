//! Index price oracle for perp markets: median mid price across exchange public tickers, with
//! outlier rejection and a minimum source count, like Orderly's multi-exchange index.
//!
//! The oracle runs in the node but only talks to the engine through journaled `SetMark`
//! commands, so replay never depends on the network. NEAR's on-chain oracles were checked
//! (2026-11-10): `priceoracle.near` returned no fresh prices and `pyth-oracle.near`'s NEAR/USD
//! was 77 days stale, so they can't drive marks. Pyth Hermes now requires an API key; it is
//! supported as a source (`pyth:<feed id>`) when `pyth_api_key` is set.

use crate::config::{FeedCfg, OracleCfg};
use crate::sequencer::Handle;
use crate::state::Shared;
use zfx_perps_engine::Command;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// Ticker URL for a source `exchange:SYMBOL`.
pub fn url(source: &str) -> Option<String> {
    let (ex, sym) = source.split_once(':')?;
    Some(match ex {
        "coinbase" => format!("https://api.exchange.coinbase.com/products/{sym}/ticker"),
        "kraken" => format!("https://api.kraken.com/0/public/Ticker?pair={sym}"),
        "okx" => format!("https://www.okx.com/api/v5/market/ticker?instId={sym}"),
        "binance" => format!("https://api.binance.com/api/v3/ticker/bookTicker?symbol={sym}"),
        "binanceus" => format!("https://api.binance.us/api/v3/ticker/bookTicker?symbol={sym}"),
        "gemini" => format!("https://api.gemini.com/v1/pubticker/{sym}"),
        "bitget" => format!("https://api.bitget.com/api/v2/spot/market/tickers?symbol={sym}"),
        "bybit" => format!("https://api.bybit.com/v5/market/tickers?category=spot&symbol={sym}"),
        "pyth" => format!("https://hermes.pyth.network/v2/updates/price/latest?ids%5B%5D={sym}&parsed=true"),
        _ => return None,
    })
}

fn f(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Mid price in USD from an exchange's ticker JSON.
pub fn parse(exchange: &str, v: &Value) -> Option<f64> {
    let (bid, ask) = match exchange {
        "coinbase" | "gemini" => (f(&v["bid"])?, f(&v["ask"])?),
        "kraken" => {
            let r = v["result"].as_object()?.values().next()?;
            (f(&r["b"][0])?, f(&r["a"][0])?)
        }
        "okx" => (f(&v["data"][0]["bidPx"])?, f(&v["data"][0]["askPx"])?),
        "binance" | "binanceus" => (f(&v["bidPrice"])?, f(&v["askPrice"])?),
        "bitget" => (f(&v["data"][0]["bidPr"])?, f(&v["data"][0]["askPr"])?),
        "bybit" => (f(&v["result"]["list"][0]["bid1Price"])?, f(&v["result"]["list"][0]["ask1Price"])?),
        "pyth" => {
            let p = &v["parsed"][0]["price"];
            let px = f(&p["price"])? * 10f64.powi(p["expo"].as_i64()? as i32);
            (px, px)
        }
        _ => return None,
    };
    (bid > 0.0 && ask >= bid && (ask - bid) / bid < 0.05).then_some((bid + ask) / 2.0)
}

/// Median after dropping sources further than `max_dev_bps` from the raw median.
pub fn aggregate(prices: &[f64], min_sources: usize, max_dev_bps: u64) -> Option<f64> {
    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = v.len();
        if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
    };
    let mut all: Vec<f64> = prices.iter().copied().filter(|p| p.is_finite() && *p > 0.0).collect();
    if all.len() < min_sources.max(1) {
        return None;
    }
    let m = median(&mut all);
    let mut kept: Vec<f64> = all
        .into_iter()
        .filter(|p| (p - m).abs() / m * 10_000.0 <= max_dev_bps as f64)
        .collect();
    (kept.len() >= min_sources.max(1)).then(|| median(&mut kept))
}

pub async fn fetch(http: &reqwest::Client, source: &str, pyth_key: Option<&str>) -> Result<f64, String> {
    let ex = source.split(':').next().unwrap_or("");
    let u = url(source).ok_or_else(|| format!("unknown source {source}"))?;
    let mut req = http.get(u).header("user-agent", "zfx-perps-oracle");
    if ex == "pyth" {
        let key = pyth_key.ok_or("pyth needs pyth_api_key")?;
        req = req.header("x-api-key", key).bearer_auth(key);
    }
    let r = req.send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("{source}: HTTP {}", r.status()));
    }
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    parse(ex, &v).ok_or_else(|| format!("{source}: unparseable ticker"))
}

/// One round for one feed: (index price in USD, sources used, per-source results).
pub async fn index(http: &reqwest::Client, feed: &FeedCfg, cfg: &OracleCfg) -> (Option<f64>, Vec<Value>) {
    let futs = feed.sources.iter().map(|s| async move {
        (
            s.clone(),
            tokio::time::timeout(Duration::from_secs(3), fetch(http, s, cfg.pyth_api_key.as_deref())).await,
        )
    });
    let results = futures_util::future::join_all(futs).await;
    let mut prices = vec![];
    let mut report = vec![];
    for (s, r) in results {
        match r {
            Ok(Ok(p)) => {
                prices.push(p);
                report.push(json!({"source": s, "price": p}));
            }
            Ok(Err(e)) => report.push(json!({"source": s, "error": e})),
            Err(_) => report.push(json!({"source": s, "error": "timeout"})),
        }
    }
    (aggregate(&prices, cfg.min_sources, cfg.max_deviation_bps), report)
}

/// Run the oracle loop: every `interval_ms`, compute each feed's index and submit `SetMark`.
pub async fn run(cfg: OracleCfg, seq: Handle, shared: Arc<Shared>) {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(4)).build().unwrap();
    let mut t = tokio::time::interval(Duration::from_millis(cfg.interval_ms.max(200)));
    loop {
        t.tick().await;
        for (symbol, feed) in &cfg.feeds {
            let Some(m) = shared.dir.read().market_by_symbol.get(symbol).copied() else {
                continue;
            };
            let (px, report) = index(&http, feed, &cfg).await;
            let status = json!({"price": px, "sources": report, "at_ms": crate::auth::now_ms()});
            shared.oracle.write().insert(m, status);
            let Some(px) = px else {
                tracing::warn!(market = %symbol, "oracle: not enough agreeing sources; mark not updated");
                continue;
            };
            let ticks = (px * feed.ticks_per_usd as f64).round() as u64;
            match seq
                .submit(Command::SetMark {
                    market: m,
                    mark: ticks,
                    index: ticks,
                    ts_ns: crate::auth::now_ns(),
                })
                .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(r)) => tracing::warn!(market = %symbol, ?r, "oracle: mark rejected"),
                Err(e) => {
                    tracing::warn!(?e, "oracle: sequencer unavailable");
                    if matches!(e, crate::sequencer::SubmitError::Stopped) {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_tickers() {
        // Shapes recorded from each exchange on 2026-11-10.
        let cases = [
            ("coinbase", json!({"ask": "3.5733", "bid": "3.5717", "price": "3.573"}), 3.5725),
            (
                "kraken",
                json!({"error": [], "result": {"NEARUSD": {"a": ["3.6010", "1", "1"], "b": ["3.6000", "1", "1"]}}}),
                3.6005,
            ),
            ("okx", json!({"code": "0", "data": [{"askPx": "3.578", "bidPx": "3.576"}]}), 3.577),
            (
                "binanceus",
                json!({"symbol": "NEARUSDT", "bidPrice": "3.57500000", "askPrice": "3.57800000"}),
                3.5765,
            ),
            ("gemini", json!({"bid": "3.5706", "ask": "3.5777", "last": "3.5636"}), 3.57415),
            ("bitget", json!({"data": [{"bidPr": "3.577", "askPr": "3.578"}]}), 3.5775),
            (
                "bybit",
                json!({"result": {"list": [{"bid1Price": "3.57", "ask1Price": "3.58"}]}}),
                3.575,
            ),
            ("pyth", json!({"parsed": [{"price": {"price": "357250000", "expo": -8}}]}), 3.5725),
        ];
        for (ex, v, want) in cases {
            let got = parse(ex, &v).unwrap_or_else(|| panic!("{ex}"));
            assert!((got - want).abs() < 1e-9, "{ex}: {got} vs {want}");
        }
        // Crossed or absurd spreads are rejected.
        assert!(parse("coinbase", &json!({"bid": "3.6", "ask": "3.5"})).is_none());
        assert!(parse("coinbase", &json!({"bid": "3.0", "ask": "4.0"})).is_none());
    }

    #[test]
    fn median_with_outlier_rejection() {
        assert_eq!(aggregate(&[3.57, 3.58, 3.56], 2, 200), Some(3.57));
        // A 10% outlier is dropped; the median of the rest is used.
        let p = aggregate(&[3.57, 3.58, 3.56, 3.93], 2, 200).unwrap();
        assert!((p - 3.57).abs() < 1e-9);
        assert_eq!(aggregate(&[3.57], 2, 200), None);
        // Two sources that disagree by more than the band: neither survives with min 2.
        assert_eq!(aggregate(&[3.0, 4.0], 2, 200), None);
    }

    /// Live check against the real exchanges. `cargo test -p zfx-perps-node oracle::tests::live -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn live() {
        let http = reqwest::Client::new();
        let feed = FeedCfg {
            sources: [
                "coinbase:NEAR-USD",
                "kraken:NEARUSD",
                "okx:NEAR-USDT",
                "binanceus:NEARUSDT",
                "gemini:nearusd",
                "bitget:NEARUSDT",
                "binance:NEARUSDT",
                "bybit:NEARUSDT",
            ]
            .map(String::from)
            .to_vec(),
            ticks_per_usd: 10_000,
        };
        let cfg = OracleCfg {
            interval_ms: 1000,
            min_sources: 3,
            max_deviation_bps: 200,
            pyth_api_key: None,
            feeds: Default::default(),
        };
        let (px, report) = index(&http, &feed, &cfg).await;
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        println!("index {px:?}");
        assert!(px.is_some());
    }
}
