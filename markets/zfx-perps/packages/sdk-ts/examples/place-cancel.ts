/**
 * Place one post-only order and cancel it (relayed on-chain) with a trading key written by
 * `zfx-perps-near onboard-testnet`. Testnet only.
 *
 *   ZFX_TRADING_FILE=~/.config/zeroflux/trading-<account>.json ZFX_NODE_URL=http://127.0.0.1:8900 \
 *   ZFX_MARKET=CLAUDE-OPUS-5-5-0921 pnpm dlx tsx examples/place-cancel.ts
 */
import { readFileSync } from "node:fs";
import { KeySigner } from "../src/signer.js";
import { ZfxPerpsClearingClient, clearing } from "../src/index.js";

const k = JSON.parse(readFileSync(process.env.ZFX_TRADING_FILE!, "utf8"));
const node = process.env.ZFX_NODE_URL ?? "http://127.0.0.1:8900";
const market = process.env.ZFX_MARKET ?? "CLAUDE-OPUS-5-5-0921";
const c = new ZfxPerpsClearingClient(node, { accountId: k.account, signer: KeySigner.fromNearSecret(k.secret_key) });
const log = (step: string, d: Record<string, unknown>) => console.log(JSON.stringify({ step, ...d }, (_, v) => (typeof v === "bigint" ? v.toString() : v)));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

for (let i = 0; ; i++) {
  const p = await c.getPortfolio().catch(() => null);
  if (p && p.settled.free > 0n) {
    log("deposit_settled", { free: p.settled.free });
    break;
  }
  if (i > 60) throw new Error("deposit not credited");
  await sleep(2000);
}
const m = (await c.listMarkets()).find((x) => x.marketId === market)!;
const price = (m.mark * 95n) / 100n;
const placed = await c.placeOrder({ marketId: market, side: "BUY", price, quantity: 100n, postOnly: true });
log("placed", { order_hash: placed.orderHash, status: placed.status, price });
const pf = await c.getPortfolio();
log("open_orders", { count: pf.openOrders.length });
const r = await c.cancelOrder(placed.order, { relayOnChain: true });
log("canceled", { response: r });
const reader = new clearing.ClearingReader(process.env.ZFX_RPC ?? "https://rpc.testnet.fastnear.com", (await c.getInfo()).clearingContract);
for (let i = 0; ; i++) {
  const o = await reader.order(placed.orderHash);
  if (o?.canceled) {
    log("cancel_final_onchain", { order: o });
    break;
  }
  if (i > 60) throw new Error("cancel not on-chain");
  await sleep(2000);
}
