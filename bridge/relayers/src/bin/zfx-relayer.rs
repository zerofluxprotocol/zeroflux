//! zfx-relayer --config relayer.json   (one of the three committee witnesses)
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.iter().position(|a| a == "--config").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| {
        eprintln!("usage: zfx-relayer --config relayer.json");
        std::process::exit(2)
    });
    let cfg: zfx_bridge::config::RelayerConfig = zfx_bridge::config::load(&path).unwrap_or_else(|e| {
        eprintln!("fatal: {}", e);
        std::process::exit(1)
    });
    let r = std::sync::Arc::new(zfx_bridge::relayer::Relayer::new(cfg).unwrap_or_else(|e| {
        eprintln!("fatal: {}", e);
        std::process::exit(1)
    }));
    zfx_bridge::relayer::serve(r).unwrap();
}
