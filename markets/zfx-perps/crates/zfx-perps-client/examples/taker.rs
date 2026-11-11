//! Takes liquidity: prints the book, sends one IOC, prints the result, balances and positions.
//!
//! ZFX_PERPS_ACCOUNT=alice.test.near ZFX_PERPS_TRADING_KEY=ed25519:... [ZFX_PERPS_MARKET=NEAR-PERP] \
//!   cargo run -p zfx-perps-client --example taker -- buy 50

use zfx_perps_client::{Client, Order, Signer};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("ZFX_PERPS_URL").unwrap_or_else(|_| "http://127.0.0.1:8794".into());
    let market = std::env::var("ZFX_PERPS_MARKET").unwrap_or_else(|_| "NEAR-USDC".into());
    let c = Client::new(
        &url,
        &std::env::var("ZFX_PERPS_ACCOUNT")?,
        Signer::from_near_secret(&std::env::var("ZFX_PERPS_TRADING_KEY")?)?,
    );
    let args: Vec<String> = std::env::args().collect();
    let side = args.get(1).map(String::as_str).unwrap_or("buy");
    let qty: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let book = c.book(&market, 5).await?;
    println!("{market}\n  bids {:?}\n  asks {:?}", book.bids, book.asks);
    let limit = if side == "buy" {
        book.asks.last().map_or(u64::MAX / 4, |l| l.0)
    } else {
        book.bids.last().map_or(1, |l| l.0)
    };
    let ack = c.place(&Order::ioc(&market, side, limit.min(100_000_000), qty)).await?;
    println!("{ack:?}");
    let v = c.account().await?;
    for b in &v.balances {
        println!("{:>5} total {:>30} available {:>30}", b.symbol, b.total, b.available);
    }
    for p in &v.positions {
        println!(
            "position {} size {} entry {} mark {} uPnL {}",
            p.market, p.size, p.entry_price, p.mark, p.unrealized_pnl
        );
    }
    if !v.positions.is_empty() {
        println!(
            "margin equity {} initial {} maintenance {} free {}",
            v.margin.equity, v.margin.initial, v.margin.maintenance, v.margin.free
        );
    }
    Ok(())
}
