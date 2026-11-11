//! Fixtures shared by tests and benchmarks.

use crate::types::*;
use crate::{Engine, EngineConfig};

pub const NEAR: AssetId = 0;
pub const USDC: AssetId = 1;
pub const M: MarketId = 0;

/// NEAR-USDC: 1 lot = 0.01 NEAR (1e22 yocto), 1 tick = $0.0001, so quote atoms (1e-6 USDC) per lot
/// per tick = 0.01 * 0.0001 * 1e6 = 1. Price 42_500 ticks = $4.25.
pub fn spec(maker_bps: u16, taker_bps: u16) -> MarketSpec {
    MarketSpec {
        id: M,
        symbol: "NEAR-USDC".into(),
        base: NEAR,
        quote: USDC,
        base_lot: 10_000_000_000_000_000_000_000,
        quote_tick: 1,
        min_qty: 1,
        max_qty: 100_000_000,
        max_price: 100_000_000,
        maker_fee_bps: maker_bps,
        taker_fee_bps: taker_bps,
        max_open_orders: 1_000_000,
        perp: None,
    }
}

pub fn engine_with(accounts: usize, near_each: u128, usdc_each: u128, maker_bps: u16, taker_bps: u16) -> Engine {
    let mut e = Engine::new(EngineConfig::default());
    let mut out = vec![];
    e.apply(
        &Command::AddAsset(AssetSpec {
            id: NEAR,
            symbol: "NEAR".into(),
            chain_ref: "wrap.near".into(),
            decimals: 24,
        }),
        &mut out,
    )
    .unwrap();
    e.apply(
        &Command::AddAsset(AssetSpec {
            id: USDC,
            symbol: "USDC".into(),
            chain_ref: "usdc.near".into(),
            decimals: 6,
        }),
        &mut out,
    )
    .unwrap();
    e.apply(&Command::AddMarket(spec(maker_bps, taker_bps)), &mut out).unwrap();
    let mut seq = 0;
    for i in 0..accounts {
        for (token, amount) in [("wrap.near", near_each), ("usdc.near", usdc_each)] {
            seq += 1;
            e.apply(
                &Command::Chain(ChainEvent::Deposit {
                    seq,
                    account: format!("u{i}.near"),
                    token: token.into(),
                    amount,
                }),
                &mut out,
            )
            .unwrap();
        }
        seq += 1;
        e.apply(
            &Command::Chain(ChainEvent::KeyAdded {
                seq,
                account: format!("u{i}.near"),
                public_key: [i as u8; 32],
            }),
            &mut out,
        )
        .unwrap();
    }
    e
}

pub fn order(account: AccountId, side: Side, price: u64, qty: u64, tif: Tif) -> Command {
    Command::Place(NewOrder {
        account,
        market: M,
        side,
        price,
        qty,
        tif,
        stp: Stp::CancelTaker,
        client_id: 0,
        ts_ns: 0,
    })
}

pub const P: MarketId = 1;

/// NEAR-PERP on USDC collateral: 1 lot = 0.01 NEAR, 1 tick = $0.0001, so a lot-tick is 1 USDC
/// atom (1e-6), same as the spot market. 10x max leverage, 5% maintenance.
pub fn perp_spec() -> MarketSpec {
    MarketSpec {
        id: P,
        symbol: "NEAR-PERP".into(),
        base: USDC,
        quote: USDC,
        perp: Some(PerpParams::default()),
        ..spec(2, 5)
    }
}

/// Engine with the spot market plus NEAR-PERP, `accounts` users holding `usdc_each`, and the mark set.
pub fn perp_engine(accounts: usize, usdc_each: u128, mark: u64) -> Engine {
    let mut e = engine_with(accounts, 0, usdc_each, 2, 5);
    let mut out = vec![];
    e.apply(&Command::AddMarket(perp_spec()), &mut out).unwrap();
    e.apply(
        &Command::SetMark {
            market: P,
            mark,
            index: mark,
            ts_ns: 0,
        },
        &mut out,
    )
    .unwrap();
    e
}

pub fn porder(account: AccountId, side: Side, price: u64, qty: u64, tif: Tif) -> Command {
    Command::Place(NewOrder {
        account,
        market: P,
        side,
        price,
        qty,
        tif,
        stp: Stp::CancelTaker,
        client_id: 0,
        ts_ns: 0,
    })
}
