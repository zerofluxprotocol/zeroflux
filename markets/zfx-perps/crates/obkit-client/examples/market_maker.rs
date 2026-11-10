//! A simple market maker: keeps `LEVELS` bids and asks around a reference price, requoting every
//! second with post-only orders, and prints its fills from the private stream. On a perp market
//! the reference is the oracle mark; on spot it random-walks from OBKIT_MID.
//!
//! OBKIT_URL=http://127.0.0.1:8794 OBKIT_ACCOUNT=mm.test.near OBKIT_TRADING_KEY=ed25519:... \
//!   cargo run -p obkit-client --example market_maker

use obkit_client::{Client, Feed, Order, Signer};
use std::time::Duration;

const LEVELS: u64 = 5;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("OBKIT_URL").unwrap_or_else(|_| "http://127.0.0.1:8794".into());
    let account = std::env::var("OBKIT_ACCOUNT")?;
    let signer = Signer::from_near_secret(&std::env::var("OBKIT_TRADING_KEY")?)?;
    let market = std::env::var("OBKIT_MARKET").unwrap_or_else(|_| "NEAR-USDC".into());
    let mut mid: u64 = std::env::var("OBKIT_MID").ok().and_then(|s| s.parse().ok()).unwrap_or(42_500);
    let c = Client::new(&url, &account, signer);
    let mut feed = Feed::connect(&url).await?;
    feed.send(c.ws_auth()).await?;
    let mut cid = 0u64;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                // Perp: quote around the mark. Spot: random walk the reference a little.
                match c.perp(&market).await {
                    Ok(p) if p.mark > 0 => mid = p.mark,
                    _ => mid = (mid as i64 + (rand_step() * 10)) as u64,
                }
                c.cancel_all(Some(&market)).await?;
                for i in 1..=LEVELS {
                    for (side, px) in [("buy", mid - 10 * i), ("sell", mid + 10 * i)] {
                        cid += 1;
                        if let Err(e) = c.place(&Order::limit(&market, side, px, 20 * i).tif("post_only").client_id(cid)).await {
                            eprintln!("{side} {px}: {e}");
                        }
                    }
                }
            }
            msg = feed.next() => {
                let m = msg?;
                if m["type"] == "fill" {
                    println!("fill {} {} @ {} (fee {})", m["side"], m["qty"], m["price"], m["fee"]);
                }
            }
        }
    }
}

fn rand_step() -> i64 {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    (n % 3) as i64 - 1
}
