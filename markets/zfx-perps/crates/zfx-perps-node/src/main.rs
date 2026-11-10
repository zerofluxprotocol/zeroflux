//! zfx-perps-node: `zfx-perps-node --config node.json` or `zfx-perps-node --example-config`.

use zfx_perps_node::config::Config;

const EXAMPLE: &str = r#"{
  "listen": "127.0.0.1:8794",
  "data_dir": "./data/node",
  "fsync": true,
  "settlement_contract": "ob.test.near",
  "fee_account": "fees.test.near",
  "admin_token": "change-me-to-a-long-random-string",
  "batch_interval_ms": 1000,
  "assets": [
    { "symbol": "NEAR", "token": "wrap.test.near", "decimals": 24 },
    { "symbol": "USDC", "token": "usdc.test.near", "decimals": 6 }
  ],
  "markets": [
    { "symbol": "NEAR-USDC", "base": "NEAR", "quote": "USDC",
      "base_lot": "10000000000000000000000", "quote_tick": "1",
      "min_qty": 1, "max_qty": 100000000, "max_price": 100000000,
      "maker_fee_bps": 2, "taker_fee_bps": 5 }
  ]
}"#;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--example-config") {
        println!("{EXAMPLE}");
        return;
    }
    let Some(path) = args.iter().position(|a| a == "--config").and_then(|i| args.get(i + 1)) else {
        eprintln!("usage: zfx-perps-node --config <node.json> | --example-config");
        std::process::exit(2);
    };
    let cfg = match Config::load(std::path::Path::new(path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config: {e}");
            std::process::exit(2);
        }
    };
    let running = match zfx_perps_node::run(cfg).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("start: {e}");
            std::process::exit(1);
        }
    };
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("shutting down: flushing journal and writing a snapshot");
    running.shutdown().await;
}

#[cfg(test)]
mod tests {
    #[test]
    fn example_config_is_valid() {
        let c: zfx_perps_node::config::Config = serde_json::from_str(super::EXAMPLE).unwrap();
        c.validate().unwrap();
    }
}
