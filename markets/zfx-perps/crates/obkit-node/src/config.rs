//! Node configuration (JSON file).

use obkit_engine::{AssetSpec, EngineConfig, MarketSpec, PerpParams};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetCfg {
    pub symbol: String,
    /// NEP-141 token contract id.
    pub token: String,
    pub decimals: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketCfg {
    pub symbol: String,
    pub base: String,
    pub quote: String,
    /// Base atoms per lot (decimal string; u128).
    pub base_lot: String,
    /// Quote atoms per lot per price tick (decimal string; u128).
    pub quote_tick: String,
    #[serde(default = "one")]
    pub min_qty: u64,
    #[serde(default = "big")]
    pub max_qty: u64,
    #[serde(default = "big")]
    pub max_price: u64,
    #[serde(default)]
    pub maker_fee_bps: u16,
    #[serde(default)]
    pub taker_fee_bps: u16,
    #[serde(default = "max_open")]
    pub max_open_orders: u32,
    /// Makes this a USDC-margined perpetual. `base` is then just the underlying's name (it needn't
    /// be a listed asset) and `quote` is the collateral asset.
    #[serde(default)]
    pub perp: Option<PerpCfg>,
}

/// Perp parameters; every field has the engine default (10x, 5% maintenance, ...).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PerpCfg {
    pub imr_bps: Option<u16>,
    pub mmr_bps: Option<u16>,
    pub liq_fee_bps: Option<u16>,
    pub price_band_bps: Option<u16>,
    pub liq_slippage_bps: Option<u16>,
    pub funding_divisor: Option<u32>,
    pub max_funding_ppm: Option<u32>,
}

impl PerpCfg {
    pub fn params(&self) -> PerpParams {
        let d = PerpParams::default();
        PerpParams {
            imr_bps: self.imr_bps.unwrap_or(d.imr_bps),
            mmr_bps: self.mmr_bps.unwrap_or(d.mmr_bps),
            liq_fee_bps: self.liq_fee_bps.unwrap_or(d.liq_fee_bps),
            price_band_bps: self.price_band_bps.unwrap_or(d.price_band_bps),
            liq_slippage_bps: self.liq_slippage_bps.unwrap_or(d.liq_slippage_bps),
            funding_divisor: self.funding_divisor.unwrap_or(d.funding_divisor),
            max_funding_ppm: self.max_funding_ppm.unwrap_or(d.max_funding_ppm),
        }
    }
}

/// Index price feed for one perp market.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeedCfg {
    /// `exchange:SYMBOL`, e.g. `coinbase:NEAR-USD`, `kraken:NEARUSD`, `okx:NEAR-USDT`,
    /// `binanceus:NEARUSDT`, `binance:NEARUSDT`, `gemini:nearusd`, `bitget:NEARUSDT`,
    /// `bybit:NEARUSDT`, `pyth:<feed id>` (needs `pyth_api_key`).
    pub sources: Vec<String>,
    /// Market ticks per 1 USD of underlying price (e.g. 10000 for $0.0001 ticks).
    pub ticks_per_usd: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OracleCfg {
    #[serde(default = "oracle_ms")]
    pub interval_ms: u64,
    /// With fewer live sources than this the mark is not updated.
    #[serde(default = "two")]
    pub min_sources: usize,
    /// Sources further than this from the median are dropped.
    #[serde(default = "dev_bps")]
    pub max_deviation_bps: u64,
    #[serde(default)]
    pub pyth_api_key: Option<String>,
    pub feeds: BTreeMap<String, FeedCfg>,
}

fn oracle_ms() -> u64 {
    1_000
}
fn two() -> usize {
    2
}
fn dev_bps() -> u64 {
    200
}

fn one() -> u64 {
    1
}
fn big() -> u64 {
    1_000_000_000
}
fn max_open() -> u32 {
    1_000
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "listen")]
    pub listen: String,
    pub data_dir: PathBuf,
    #[serde(default = "yes")]
    pub fsync: bool,
    /// NEAR settlement contract id; part of every withdrawal message.
    pub settlement_contract: String,
    /// Account credited with trading fees.
    pub fee_account: String,
    /// Bearer token for /internal/* (settlement adapter, admin).
    pub admin_token: String,
    #[serde(default = "batch_ms")]
    pub batch_interval_ms: u64,
    #[serde(default = "snap")]
    pub snapshot_every: u64,
    #[serde(default = "entries")]
    pub max_batch_entries: usize,
    #[serde(default = "withdrawals")]
    pub max_batch_withdrawals: usize,
    /// Per-account request budget (requests per second, burst = 2x).
    #[serde(default = "rate")]
    pub rate_limit_per_sec: u32,
    /// Settlement-chain account of the perp insurance fund. Required when any perp is listed.
    #[serde(default)]
    pub insurance_account: Option<String>,
    #[serde(default = "funding_ms")]
    pub funding_interval_ms: u64,
    #[serde(default)]
    pub oracle: Option<OracleCfg>,
    pub assets: Vec<AssetCfg>,
    pub markets: Vec<MarketCfg>,
}

fn listen() -> String {
    "127.0.0.1:8794".into()
}
fn yes() -> bool {
    true
}
fn batch_ms() -> u64 {
    1_000
}
fn snap() -> u64 {
    200_000
}
fn entries() -> usize {
    48
}
fn withdrawals() -> usize {
    4
}
fn rate() -> u32 {
    2_000
}
fn funding_ms() -> u64 {
    3_600_000
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let c: Config = serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !obkit_wire::valid_account(&self.settlement_contract) || !obkit_wire::valid_account(&self.fee_account) {
            return Err("settlement_contract and fee_account must be NEAR account ids".into());
        }
        for a in &self.assets {
            if !obkit_wire::valid_account(&a.token) {
                return Err(format!("asset {}: bad token id", a.symbol));
            }
        }
        for m in &self.markets {
            if !obkit_wire::valid_market(&m.symbol) {
                return Err(format!("market {}: symbols are [A-Z0-9-]", m.symbol));
            }
            let needs: Vec<&String> = if m.perp.is_some() {
                vec![&m.quote]
            } else {
                vec![&m.base, &m.quote]
            };
            for s in needs {
                if !self.assets.iter().any(|a| &a.symbol == s) {
                    return Err(format!("market {}: unknown asset {s}", m.symbol));
                }
            }
        }
        if self.markets.iter().any(|m| m.perp.is_some()) && !self.insurance_account.as_deref().is_some_and(obkit_wire::valid_account) {
            return Err("perp markets need an insurance_account (a NEAR account id)".into());
        }
        if let Some(o) = &self.oracle {
            for (sym, f) in &o.feeds {
                if !self.markets.iter().any(|m| &m.symbol == sym && m.perp.is_some()) {
                    return Err(format!("oracle feed for {sym}: not a perp market"));
                }
                if f.sources.is_empty() || f.ticks_per_usd == 0 {
                    return Err(format!("oracle feed for {sym}: needs sources and ticks_per_usd"));
                }
            }
        }
        if self.admin_token.len() < 16 {
            return Err("admin_token must be at least 16 characters".into());
        }
        Ok(())
    }

    pub fn engine_config(&self) -> EngineConfig {
        EngineConfig {
            fee_account: self.fee_account.clone(),
            max_batch_entries: self.max_batch_entries,
            max_batch_withdrawals: self.max_batch_withdrawals,
            insurance_account: self
                .insurance_account
                .clone()
                .unwrap_or_else(|| format!("insurance.{}", self.settlement_contract)),
        }
    }

    pub fn asset_spec(&self, id: u16, a: &AssetCfg) -> AssetSpec {
        AssetSpec {
            id,
            symbol: a.symbol.clone(),
            chain_ref: a.token.clone(),
            decimals: a.decimals,
        }
    }

    pub fn market_spec(&self, id: u16, m: &MarketCfg, asset_id: impl Fn(&str) -> Option<u16>) -> Result<MarketSpec, String> {
        let p = |s: &str| s.parse::<u128>().map_err(|e| format!("market {}: {e}", m.symbol));
        Ok(MarketSpec {
            id,
            symbol: m.symbol.clone(),
            base: if m.perp.is_some() {
                asset_id(&m.quote).ok_or("unknown quote")?
            } else {
                asset_id(&m.base).ok_or("unknown base")?
            },
            quote: asset_id(&m.quote).ok_or("unknown quote")?,
            base_lot: p(&m.base_lot)?,
            quote_tick: p(&m.quote_tick)?,
            min_qty: m.min_qty,
            max_qty: m.max_qty,
            max_price: m.max_price,
            maker_fee_bps: m.maker_fee_bps,
            taker_fee_bps: m.taker_fee_bps,
            max_open_orders: m.max_open_orders,
            perp: m.perp.as_ref().map(|p| p.params()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the defaults (1e9 lots, 1e9 ticks) with a 24-decimal base must form a valid
    /// market. They once tripped an overly strict overflow guard.
    #[test]
    fn defaults_with_24_decimal_base_are_a_valid_market() {
        let c: Config = serde_json::from_str(
            r#"{"data_dir":"/tmp/x","settlement_contract":"ob.near","fee_account":"fees.near","admin_token":"0123456789abcdef",
                "assets":[{"symbol":"NEAR","token":"wrap.near","decimals":24},{"symbol":"USDC","token":"usdc.near","decimals":6}],
                "markets":[{"symbol":"NEAR-USDC","base":"NEAR","quote":"USDC","base_lot":"10000000000000000000000","quote_tick":"1"}]}"#,
        )
        .unwrap();
        c.validate().unwrap();
        let mut e = obkit_engine::Engine::new(c.engine_config());
        let mut out = vec![];
        for (i, a) in c.assets.iter().enumerate() {
            e.apply(&obkit_engine::Command::AddAsset(c.asset_spec(i as u16, a)), &mut out)
                .unwrap();
        }
        let spec = c
            .market_spec(0, &c.markets[0], |s| c.assets.iter().position(|a| a.symbol == s).map(|i| i as u16))
            .unwrap();
        assert_eq!(
            e.apply(&obkit_engine::Command::AddMarket(spec), &mut out),
            Ok(obkit_engine::Ack::Ok)
        );
    }
}
