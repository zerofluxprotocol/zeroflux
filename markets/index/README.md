# ZFX Markets model-demand index

`ZFX-OR-MODEL-SPEND-USD-v1`: an OpenRouter-implied daily billings proxy, in USD per UTC day,
for each of the top 50 priced text models on OpenRouter. It is a published, charted,
**paper-trading** index, **not settlement-grade**. It has two variants: the dataset index, from
OpenRouter's documented daily dataset (`datasets/rankings-daily`), and the web index, from
OpenRouter's public web endpoints (the ones openrouter.ai pages call). Markets reference the
dataset 7-day index. Method, sources and limits: [`docs/DATA_METHOD.md`](../../docs/DATA_METHOD.md).

```text
markets/index/
  crates/zfx-oracle-print/   frozen OraclePrint v1 codec + ed25519 verify (no_std, wasm32 OK)
  crates/zfx-index/          ingestor, ledger, calculator, checks, publisher, poller, API server
  formula/                   ZFX-OR-MODEL-SPEND-USD-v1.txt and -7D-v1.txt (SHA-256 = each print's formulaHash)
  vectors/                   oracle_print_v1.json golden vectors
  js/                        oracle-print.mjs reference codec + vector test (Node 18+, no deps)
  docs/ORACLE_PRINT_V1.md    canonical serialization and signature domain
  discovery/endpoints.json   what the openrouter.ai pages call (browser capture, 2026-11-10)
  tools/discover.mjs         the capture script
  keys/                      reporter public key (the private key is never in the repo)
  ledger/                    append-only raw responses + fetch/revision/market logs (committed snapshot)
  public/                    static JSON tree, same paths as the API (committed snapshot)
```

## Build and test

```sh
cd markets/index
cargo build --release
cargo test                          # Rust unit + integration + golden vector tests
node js/test-vectors.mjs            # JS codec against the same vectors
cargo build -p zfx-oracle-print --no-default-features --target wasm32-unknown-unknown
```

## Run

All commands run from `markets/index/`. Defaults: `--ledger ledger`, `--out public`,
`--key ~/.config/zeroflux/oracle-reporter.key`.

```sh
B=./target/release/zfx-index

$B keygen        # once: writes ~/.config/zeroflux/oracle-reporter.key (0600), prints the pubkey
$B backfill      # catalog + rankings + top-50 listing + per-model history (~155 requests, ~20 min)
$B dataset-backfill   # documented datasets/rankings-daily 2025-01-01..yesterday (API key; 366 days max per request)
$B engine        # daily, 7D, dataset daily and dataset 7D revisions; signs prints
$B freeze-lic-params  # one-time: derive LIC-v1 frozen shares (refuses to overwrite)
$B publish       # regenerate public/ from the ledger
$B verify        # check hash chains, every blob hash and every print signature
$B health        # print source-health JSON

# poller: fetch on schedule, then engine + publish. Holds the ledger lock only per pass.
nohup $B poll > ~/.local/state/zfx-index/poll.log 2>&1 & echo $! > ~/.local/state/zfx-index/poll.pid
kill "$(cat ~/.local/state/zfx-index/poll.pid)"   # stop it

# read-only API on an ephemeral port (the bound address is printed); pick a port with --addr
$B serve --addr 127.0.0.1:0
curl -s localhost:PORT/v1/markets/models | head
curl -s 'localhost:PORT/v1/indices/or-model-spend/anthropic/claude-opus-5.5-20260921?from=2026-11-01&to=2026-11-09'
curl -s localhost:PORT/v1/indices/or-model-spend/anthropic/claude-opus-5.5-20260921/latest
curl -s localhost:PORT/v1/indices/or-model-spend/anthropic/claude-opus-5.5-20260921/provenance/2026-11-09.r1
curl -s localhost:PORT/v1/indices/source-health
curl -s localhost:PORT/v1/indices/or-model-spend-7d/anthropic/claude-opus-5.5-20260921/latest       # web 7D
curl -s localhost:PORT/v1/indices/or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921/latest   # dataset 7D (recommended)
# WS stream of new revisions (prints and corrections); ?replay=N resends the last N
websocat 'ws://localhost:PORT/v1/indices/stream?replay=3'
```

API key: `OPENROUTER_API_KEY` from the environment, else `~/.config/zeroflux/keys.env`. It is
sent only as the Authorization header and is never logged or stored. With a key the poller
fetches the documented dataset (one 30-day request, hourly until yesterday appears, then once
after the grace window; at most 450/day) and a daily documented price snapshot per market. The
dataset indices (`or-model-spend-lic`, `or-model-spend-lic-7d`) are built from those only.

## On-chain oracle (NEAR)

```sh
# contract (own workspace); unit tests, wasm build
(cd contracts && cargo test && cd zfx-oracle && cargo near build non-reproducible-wasm --no-abi)
# sandbox test: deploy, attacks, gas and storage measurement (needs near-sandbox: scripts/install-near-sandbox.sh)
cargo test -p zfx-oracle-near --test sandbox -- --nocapture
N=./target/release/zfx-oracle-near
$N create-account --parent zeroflux.testnet --parent-key-file ~/.near-credentials/testnet/zeroflux.testnet.json \
   --name oracle --fund-near 4.8 --out-key-file ~/.config/zeroflux/oracle.zeroflux.testnet.json
$N deploy --account oracle.zeroflux.testnet --key-file ~/.config/zeroflux/oracle.zeroflux.testnet.json \
   --wasm contracts/target/near/zfx_oracle/zfx_oracle.wasm --reporter <pubkey hex> --quorum 1 --min-free-near 0.2   # add --no-init for a code upgrade
$N set-indices --contract oracle.zeroflux.testnet --account oracle.zeroflux.testnet --key-file ... \
   --models ~/.local/share/zfx-index/public/v1/markets/models.json --markets <v2-markets.json>
$B push-near --ledger ~/.local/share/zfx-index/ledger --contract oracle.zeroflux.testnet \
   --signer oracle-relayer.zeroflux.testnet --key-file ~/.config/zeroflux/oracle-relayer.zeroflux.testnet.json [--from-date D] [--dry-run]
$N status --contract oracle.zeroflux.testnet
```

The poller pushes automatically when `~/.config/zeroflux/oracle-push.json` exists with
`"enabled": true` (fields: contract, signer, key_file, rpc, files, from_date). Results are appended to
`<ledger>/../near-push.jsonl`. Deployment record: `TESTNET_ORACLE.md`.

## Request budget

`RateBudget::default()`: at least 6 s between requests plus 0 to 4 s random jitter, and at
most 400 requests in any rolling 24 h (counted from the ledger, so restarts do not reset it).
With 50 markets the poller sends about 160 to 200 requests per day. User-Agent:
`zfx-index/0.1 (+https://github.com/zerofluxprotocol/zeroflux/tree/main/markets/index; public-data index research)`.
No cookies, no browser emulation, no challenge solving.

## Output contract for the trade UI

- `public/v1/markets/models.json`: markets, model metadata (author, Hugging Face id if listed),
  lifecycle (`ACTIVE`, `CLOSE_ONLY`, `HALTED`, `PROPOSED`; trading is always `PAPER_ONLY`) and the latest point.
- `public/v1/indices/or-model-spend/<permaslug>/series.json`: one point per UTC day with tokens
  (prompt, completion, cached, reasoning, requests, BYOK), observed mix, effective prices and
  price basis, value, data status, finality, revision, confidence, check codes, as-of times, print hash.
- `.../latest.json` and `.../provenance/<revision_id>.json`: full revision record including
  input response hashes, URLs, fetch and source times, the canonical print bytes and signature.
- `public/v1/indices/or-model-spend-7d/<permaslug>/series.json`, `latest.json`, `provenance/`:
  the traded trailing 7-day index (`ZFX-OR-MODEL-SPEND-USD-7D-v1`) with `valid_days`,
  `window_sum_usd`, `manipulation_cost_usd_per_pct`, `max_open_interest_usd`, `change_vs_prev`,
  `clamped` and per-day `lifecycle`. `models.json` carries the latest of these per market.
- `public/v1/indices/source-health.json`: fetch stats per endpoint, rate budget use,
  publication latency rows, top-N coverage, spend-share cross-check, revision counts.
- `public/v1/indices/prints/latest.json`, `public/v1/oracle/reporter.json`.

Every value carries `Source: OpenRouter (openrouter.ai/rankings), as of <timestamp>`.
