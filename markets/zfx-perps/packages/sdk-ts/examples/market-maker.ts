/**
 * Two-sided post-only ladder around the oracle mark on one clearing market.
 *
 * - Quotes `LEVELS` bids below and asks above the mark, `STEP_BPS` apart, `SIZE` lots each.
 * - Re-quotes when the mark changes: one signed `setMinNonce` kills every resting quote at once
 *   (here and on-chain), then a fresh ladder goes out with larger nonces.
 * - Before each quote, the local `estimateMargin` checks the free collateral it would lock; the
 *   ladder stops growing when the budget runs out.
 * - Fails closed: if the oracle print does not verify (quorum, hash, freshness, mark), it pulls
 *   all quotes and waits.
 *
 * Run: see examples/README.md.
 */
import { KeySigner } from "../src/signer.js";
import { ClearingReader, ZfxPerpsClient, checkOracle, estimateMargin, type Market, type Side } from "../src/clearing/index.js";

const env = (k: string, d?: string): string => {
  const v = process.env[k] ?? d;
  if (v === undefined) throw new Error(`set ${k}`);
  return v;
};

const NODE = env("ZFX_NODE_URL", "http://127.0.0.1:8796");
const ACCOUNT = env("ZFX_ACCOUNT");
const KEY = env("ZFX_TRADING_KEY"); // ed25519:<base58 secret>, registered with register_key
const MARKET = env("ZFX_MARKET", "OPUS-PERP");
const RPC = process.env.ZFX_NEAR_RPC; // optional: read the reporter allowlist from the contract
const LEVELS = Number(env("ZFX_LEVELS", "3"));
const STEP_BPS = BigInt(env("ZFX_STEP_BPS", "25"));
const SIZE = BigInt(env("ZFX_SIZE", "10"));
const POLL_MS = Number(env("ZFX_POLL_MS", "2000"));

const signer = KeySigner.fromNearSecret(KEY);
const client = new ZfxPerpsClient(NODE, { accountId: ACCOUNT, signer });

/** Price `bps` away from `mark` in ticks, rounded away from the mark so quotes never cross it. */
function offset(mark: bigint, bps: bigint, side: Side): bigint {
  const d = (mark * bps + 9_999n) / 10_000n;
  const step = d === 0n ? 1n : d;
  return side === "BUY" ? mark - step : mark + step;
}

async function reporters(): Promise<{ reporters?: string[]; quorum?: number }> {
  if (!RPC) return {};
  const info = await client.getInfo();
  const st = await new ClearingReader(RPC, info.clearingContract).state();
  return { reporters: st.reporters, quorum: st.quorum };
}

async function pullQuotes(reason: string) {
  const { minNonce } = await client.setMinNonce();
  console.log(`pulled quotes (${reason}); min nonce ${minNonce}`);
}

async function quote(market: Market, mark: bigint) {
  const portfolio = await client.getPortfolio();
  let free = portfolio.pending.free;
  const pos = portfolio.pending.positions.find((p) => p.market === MARKET);
  let placed = 0;
  for (let i = 1; i <= LEVELS; i++) {
    for (const side of ["BUY", "SELL"] as const) {
      const price = offset(mark, STEP_BPS * BigInt(i), side);
      if (price <= 0n || price > market.params.maxPrice) continue;
      // Worst case each quote fills alone against the current position.
      const est = estimateMargin({ params: market.params, mark }, pos, side, price, SIZE, { postOnly: true, free });
      if (!est.ok) {
        console.log(`skip ${side} ${SIZE}@${price}: ${est.violations.join(",")}`);
        continue;
      }
      free -= est.requiredFree;
      const ack = await client.placeOrder({ marketId: MARKET, side, price, quantity: SIZE, postOnly: true });
      placed++;
      console.log(`${side} ${SIZE}@${price} ${ack.status} ${ack.orderHash.slice(0, 12)}`);
    }
  }
  console.log(`mark ${mark}: ${placed} quotes, free budget left ${free}`);
}

async function main() {
  const trust = await reporters();
  const fills = client.subscribeOrders((e) => {
    if (e.type === "fill" || e.type === "fill_reverted" || e.type === "settled") console.log("account", JSON.stringify(e));
  }, { onError: (e) => console.error("ws", e) });
  await fills.ready;

  let quotedMark: bigint | null = null;
  let stopping = false;
  process.on("SIGINT", () => {
    stopping = true;
  });

  while (!stopping) {
    try {
      const markets = await client.listMarkets();
      const market = markets.find((m) => m.marketId === MARKET);
      if (!market) throw new Error(`no market ${MARKET}`);
      const oracle = await client.getOracle(MARKET);
      const check = checkOracle(oracle, { ...trust, market });
      if (!check.ok || market.effectiveStatus !== "ACTIVE") {
        if (quotedMark !== null) await pullQuotes(`oracle ${check.problems.join(",") || "ok"}, status ${market.effectiveStatus}`);
        quotedMark = null;
      } else if (oracle.mark !== quotedMark) {
        if (quotedMark !== null) await pullQuotes(`mark ${quotedMark} -> ${oracle.mark}`);
        await quote(market, oracle.mark);
        quotedMark = oracle.mark;
      }
    } catch (e) {
      console.error(e);
    }
    await new Promise((r) => setTimeout(r, POLL_MS));
  }
  await pullQuotes("shutdown");
  fills.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
