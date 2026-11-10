/**
 * Live test against the real Rust node (target/release/obkit-node): the node verifies signatures
 * made by this TS SDK, trades match, feeds agree with REST. Build first:
 *   cargo build --release -p obkit-node
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { KeySigner, ObkitClient, ObkitError, ObkitFeed, type AccountEvent } from "../src/index.js";

const BIN = new URL("../../../target/release/obkit-node", import.meta.url).pathname;
const TOKEN = "ts-test-admin-token-xxxx";
const M = "NEAR-USDC";
const NEAR = "wrap.test.near";
const USDC = "usdc.test.near";
let proc: ChildProcess;
let url = "";

const freePort = () =>
  new Promise<number>((res) => {
    const s = createServer();
    s.listen(0, () => {
      const p = (s.address() as any).port;
      s.close(() => res(p));
    });
  });

async function admin(path: string, body?: unknown) {
  const r = await fetch(`${url}${path}`, { method: body ? "POST" : "GET", headers: { authorization: `Bearer ${TOKEN}`, "content-type": "application/json" }, body: body ? JSON.stringify(body) : undefined });
  return r.json();
}

const alice = KeySigner.generate();
const bob = KeySigner.generate();

beforeAll(async () => {
  if (!existsSync(BIN)) throw new Error(`${BIN} missing: cargo build --release -p obkit-node`);
  const dir = mkdtempSync(join(tmpdir(), "obkit-ts-"));
  const port = await freePort();
  url = `http://127.0.0.1:${port}`;
  writeFileSync(
    join(dir, "node.json"),
    JSON.stringify({
      listen: `127.0.0.1:${port}`,
      data_dir: join(dir, "data"),
      fsync: false,
      settlement_contract: "ob.test.near",
      fee_account: "fees.test.near",
      admin_token: TOKEN,
      batch_interval_ms: 3600000,
      assets: [
        { symbol: "NEAR", token: NEAR, decimals: 24 },
        { symbol: "USDC", token: USDC, decimals: 6 },
      ],
      insurance_account: "insurance.test.near",
      markets: [
        { symbol: M, base: "NEAR", quote: "USDC", base_lot: "10000000000000000000000", quote_tick: "1", maker_fee_bps: 2, taker_fee_bps: 5 },
        { symbol: "NEAR-PERP", base: "NEAR", quote: "USDC", base_lot: "1", quote_tick: "1", maker_fee_bps: 2, taker_fee_bps: 5, perp: {} },
      ],
    }),
  );
  proc = spawn(BIN, ["--config", join(dir, "node.json")], { stdio: "ignore" });
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${url}/v1/health`)).ok) break;
    } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  let seq = 0;
  const events: any[] = [];
  for (const [name, s] of [
    ["alice.test.near", alice],
    ["bob.test.near", bob],
  ] as const) {
    events.push({ type: "key_added", seq: String(++seq), account: name, public_key: s.publicKey });
    events.push({ type: "deposit", seq: String(++seq), account: name, token: NEAR, amount: (10n ** 30n).toString() });
    events.push({ type: "deposit", seq: String(++seq), account: name, token: USDC, amount: "100000000000" });
  }
  await admin("/internal/chain_events", { events });
});

afterAll(() => proc?.kill("SIGTERM"));

describe("TS SDK against the Rust node", () => {
  it("trades, streams and reads accounts with TS-made signatures", async () => {
    const a = new ObkitClient(url, "alice.test.near", alice);
    const b = new ObkitClient(url, "bob.test.near", bob);
    expect((await a.info()).markets[0].symbol).toBe(M);

    const fills: AccountEvent[] = [];
    const feed = new ObkitFeed(url, { onAccount: (e) => fills.push(e) });
    await feed.subscribeBook(M);
    await feed.authenticate(b);

    expect(await a.place({ market: M, side: "sell", price: 42_500, qty: 100, clientId: 1 })).toMatchObject({ type: "placed", status: "resting" });
    expect(await a.place({ market: M, side: "sell", price: 42_510, qty: 50, clientId: 2 })).toMatchObject({ type: "placed", status: "resting" });
    expect(await b.place({ market: M, side: "buy", price: 42_510, qty: 120, tif: "ioc" })).toMatchObject({ type: "placed", status: "filled", filled: 120 });

    const snap = await a.book(M);
    expect(snap.asks).toEqual([[42_510, 30]]);
    await feed.waitForSeq(M, snap.seq);
    expect(feed.books.get(M)!.topAsks()).toEqual(snap.asks);
    const trades = await a.trades(M);
    expect(trades.map((t) => [t.price, t.qty])).toEqual([
      [42_510, 20],
      [42_500, 100],
    ]);
    for (let i = 0; i < 50 && fills.filter((f) => f.type === "fill").length < 2; i++) await new Promise((r) => setTimeout(r, 20));
    expect(fills.map((f) => f.type)).toEqual(["accepted", "fill", "fill"]);

    // Engine rejects carry their code: a post-only buy at the remaining ask would cross.
    const err = await b.place({ market: M, side: "buy", price: 42_510, qty: 1, tif: "post_only" }).catch((e) => e);
    expect(err).toBeInstanceOf(ObkitError);
    expect([err.status, err.code]).toEqual([409, "post_only_would_cross"]);

    // Cancel by client id, then account view.
    expect(await a.cancelByClientId(M, 2)).toEqual({ type: "canceled", order_id: 2, remaining: 30 });
    const av = await a.accountView();
    expect(av.open_orders).toEqual([]);
    expect(av.balances.find((x) => x.token === USDC)!.total).toBe(String(100_000_000_000 + 100 * 42_500 + 20 * 42_510 - Math.ceil(100 * 42_500 * 2 / 10_000) - Math.ceil(20 * 42_510 * 2 / 10_000)));

    // Withdrawal signs the on-chain message with the contract id from /v1/info.
    expect(await b.withdraw(USDC, 1_000_000n)).toEqual({ type: "withdraw_queued", nonce: 1 });
    expect(await b.withdraw(USDC, 1n)).toEqual({ type: "withdraw_queued", nonce: 2 });

    // Errors carry the engine's reject code.
    const stranger = new ObkitClient(url, "alice.test.near", KeySigner.generate());
    const e2 = await stranger.place({ market: M, side: "buy", price: 1, qty: 1 }).catch((e) => e);
    expect([e2.status, e2.code]).toEqual([401, "key_not_registered"]);
    feed.close();
  });

  it("trades a perp: positions, margin, perp channel and liquidation events", async () => {
    const P = "NEAR-PERP";
    const a = new ObkitClient(url, "alice.test.near", alice);
    const b = new ObkitClient(url, "bob.test.near", bob);
    expect((await a.info()).markets.find((m) => m.symbol === P)!.kind).toBe("perp");
    await admin("/internal/mark", { market: P, mark: 40_000 });
    const perpEvents: any[] = [];
    const accountEvents: AccountEvent[] = [];
    const feed = new ObkitFeed(url, { onPerp: (e) => perpEvents.push(e), onAccount: (e) => accountEvents.push(e) });
    await feed.subscribePerp(P);
    await feed.authenticate(a);

    await b.place({ market: P, side: "sell", price: 40_000, qty: 10_000 });
    expect(await a.place({ market: P, side: "buy", price: 40_000, qty: 10_000, tif: "ioc" })).toMatchObject({ type: "placed", status: "filled" });
    await admin("/internal/mark", { market: P, mark: 41_000 });
    for (let i = 0; i < 50 && perpEvents.length === 0; i++) await new Promise((r) => setTimeout(r, 20));
    expect(perpEvents[0]).toMatchObject({ channel: "perp", type: "mark", market: P, mark: 41_000 });

    const av = await a.accountView();
    expect(av.positions).toEqual([{ market: P, size: 10_000, entry_notional: "400000000", entry_price: 40_000, mark: 41_000, unrealized_pnl: "10000000" }]);
    expect(BigInt(av.margin.equity)).toBeGreaterThan(0n);
    const pi = await a.perp(P);
    expect([pi.mark, pi.open_interest]).toEqual([41_000, 10_000]);

    // Margin: Alice can't open more than her equity supports.
    const e = await a.place({ market: P, side: "buy", price: 41_000, qty: 100_000_000 }).catch((x) => x);
    expect([e.status, e.code]).toEqual([409, "insufficient_margin"]);

    // Carol has $50 and goes 8x long; a 15% drop liquidates her. Bob's book is empty below, so
    // the insurance fund takes the position over at the mark.
    const carolKey = KeySigner.generate();
    const cursor = (await admin("/internal/status")).chain_cursor;
    await admin("/internal/chain_events", {
      events: [
        { type: "key_added", seq: String(cursor + 1), account: "carol.test.near", public_key: carolKey.publicKey },
        { type: "deposit", seq: String(cursor + 2), account: "carol.test.near", token: USDC, amount: "50000000" },
      ],
    });
    const c = new ObkitClient(url, "carol.test.near", carolKey);
    const carolEvents: AccountEvent[] = [];
    const cf = new ObkitFeed(url, { onAccount: (e) => carolEvents.push(e) });
    await cf.authenticate(c);
    await b.place({ market: P, side: "sell", price: 41_000, qty: 10_000 });
    await c.place({ market: P, side: "buy", price: 41_000, qty: 10_000, tif: "ioc" });
    await admin("/internal/mark", { market: P, mark: 34_850 });
    for (let i = 0; i < 100 && !carolEvents.some((x) => x.type === "insurance_takeover"); i++) await new Promise((r) => setTimeout(r, 20));
    const kinds = carolEvents.map((x) => x.type);
    expect(kinds).toContain("liquidated");
    expect(kinds).toContain("insurance_takeover");
    expect((await c.accountView()).positions).toEqual([]);
    cf.close();
    feed.close();
  });
});
