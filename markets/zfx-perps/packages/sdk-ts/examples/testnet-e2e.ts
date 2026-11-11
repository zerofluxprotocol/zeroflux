/**
 * NEAR testnet end to end through the public SDK: two traders open and then close a position on
 * the clearing contract via a locally running zfx-perps node and relayer, against a real
 * reporter-signed print of the ZFX daily model index. Every on-chain balance is compared with
 * reference math computed here from the order prices alone.
 *
 * Wallet actions (account creation, faucet mint, trading-key registration, deposit) are signed by
 * the traders' NEAR keys through near-api-js, using the SDK's wallet transaction builders. Trading
 * actions (orders, portfolio reads, withdrawal) go through `ZfxPerpsClearingClient` only.
 *
 *   pnpm dlx tsx examples/testnet-e2e.ts
 *
 * Env: ZFX_STACK (clearing stack file from `zfx-perps-near clearing-deploy`), ZFX_NODE_URL,
 * ZFX_ADMIN_TOKEN (to relay the print, an operator action), ZFX_PRINTS (the index publisher's
 * prints/latest.json), ZFX_INDEX_ID, ZFX_MARKET, ZFX_RPC, ZFX_KEYS (where trader keys are kept,
 * outside the repo), ZFX_OUT (results JSON), ZFX_RELAYER_LOG (the relayer's batch log).
 */
import { readFileSync, writeFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { base58 } from "@scure/base";
import { connect, keyStores, KeyPair, type Account } from "near-api-js";
import { KeySigner } from "../src/signer.js";
import { ZfxPerpsClearingClient, clearing } from "../src/index.js";

const env = (k: string, d?: string) => {
  const v = process.env[k] ?? d;
  if (v === undefined) throw new Error(`set ${k}`);
  return v;
};
const STACK = JSON.parse(readFileSync(env("ZFX_STACK"), "utf8"));
const NODE = env("ZFX_NODE_URL", "http://127.0.0.1:8796");
const ADMIN = env("ZFX_ADMIN_TOKEN");
const RPC = env("ZFX_RPC", STACK.rpc_url);
const MARKET = env("ZFX_MARKET", "OPUS-PERP");
const INDEX_ID = env("ZFX_INDEX_ID", "or-model-spend/anthropic/claude-opus-5.5-20260921");
const KEYS = env("ZFX_KEYS");
const OUT = env("ZFX_OUT");
const DEPOSIT = 1_000_000_000n; // 1,000 tUSDC (6 decimals)
const QTY = 1_000n; // lots
const log: Record<string, unknown>[] = [];
const step = (name: string, data: Record<string, unknown> = {}) => {
  const row = { step: name, at: new Date().toISOString(), ...data };
  log.push(row);
  console.log(JSON.stringify(row, (_, v) => (typeof v === "bigint" ? v.toString() : v)));
};
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function waitFor<T>(what: string, secs: number, f: () => Promise<T | undefined | false>): Promise<T> {
  const t = Date.now();
  for (;;) {
    const v = await f().catch(() => undefined);
    if (v) return v as T;
    if (Date.now() - t > secs * 1000) throw new Error(`timed out waiting for ${what}`);
    await sleep(2000);
  }
}

const hexToB58 = (h: string) => base58.encode(Uint8Array.from(h.match(/../g)!.map((x) => parseInt(x, 16))));

async function main() {
  // ---------------- wallets ----------------
  const ks = new keyStores.InMemoryKeyStore();
  await ks.setKey("testnet", STACK.owner.account, KeyPair.fromString(STACK.owner.secret_key));
  const saved: Record<string, { near: string; trading: string }> = existsSync(KEYS) ? JSON.parse(readFileSync(KEYS, "utf8")) : {};
  const names = ["perps-maker", "perps-taker"].map((n) => `${n}.${STACK.owner.account}`);
  for (const n of names) {
    if (!saved[n]) saved[n] = { near: KeyPair.fromRandom("ed25519").toString(), trading: KeySigner.generate().nearSecret };
    await ks.setKey("testnet", n, KeyPair.fromString(saved[n].near as any));
  }
  mkdirSync(dirname(KEYS), { recursive: true });
  writeFileSync(KEYS, JSON.stringify(saved, null, 1), { mode: 0o600 });
  const near = await connect({ networkId: "testnet", nodeUrl: RPC, keyStore: ks });
  const owner = await near.account(STACK.owner.account);
  const send = (acct: Account) => async (tx: clearing.WalletTx) => {
    const a = tx.actions[0].params;
    const r: any = await acct.functionCall({ contractId: tx.receiverId, methodName: a.methodName, args: a.args, gas: BigInt(a.gas), attachedDeposit: BigInt(a.deposit) });
    const failure = r.status?.Failure ?? r.receipts_outcome?.find((o: any) => o.outcome.status?.Failure)?.outcome.status.Failure;
    if (failure) throw new Error(`${a.methodName} failed: ${JSON.stringify(failure)}`);
    return r.transaction.hash as string;
  };
  const traders: { id: string; acct: Account; signer: KeySigner; client: InstanceType<typeof ZfxPerpsClearingClient> }[] = [];
  for (const id of names) {
    const exists = await near.connection.provider.query({ request_type: "view_account", finality: "final", account_id: id }).then(() => true, () => false);
    if (!exists) {
      const pk = KeyPair.fromString(saved[id].near as any).getPublicKey();
      const r: any = await owner.createAccount(id, pk, 500_000_000_000_000_000_000_000n);
      step("create_account", { account: id, tx: r.transaction.hash });
    }
    const acct = await near.account(id);
    const signer = KeySigner.fromNearSecret(saved[id].trading);
    traders.push({ id, acct, signer, client: new ZfxPerpsClearingClient(NODE, { accountId: id, signer }) });
  }
  const [maker, taker] = traders;
  const info = await maker.client.getInfo();
  step("node_info", { ...info });

  // ---------------- wallet actions: mint, register key, deposit ----------------
  const reader = new clearing.ClearingReader(RPC, info.clearingContract);
  for (const t of traders) {
    const h1 = await send(t.acct)({ receiverId: info.collateral, actions: [{ type: "FunctionCall", params: { methodName: "mint", args: { account_id: t.id, amount: DEPOSIT.toString() }, gas: "30000000000000", deposit: "10000000000000000000000" } }] });
    step("faucet_mint", { account: t.id, amount: DEPOSIT, tx: h1 });
    const keys: any[] = await reader.keys(t.id).catch(() => []);
    if (!keys.some((k: any) => k.public_key === t.signer.publicKey)) {
      const h2 = await send(t.acct)(clearing.clearingTx.registerKey(info.clearingContract, t.signer.publicKey, { canWithdraw: true }));
      step("register_trading_key", { account: t.id, public_key: t.signer.publicKey, tx: h2 });
    }
    const before = (await t.client.getPortfolio()).settled.free;
    const d = await t.client.deposit(DEPOSIT, { send: send(t.acct) });
    step("deposit", { account: t.id, amount: DEPOSIT, tx: d.result });
    await waitFor(`${t.id} deposit credited on-chain`, 180, async () => (await t.client.getPortfolio()).settled.free === before + DEPOSIT);
  }
  const free0 = new Map<string, bigint>();
  for (const t of traders) free0.set(t.id, (await t.client.getPortfolio()).settled.free);
  step("deposits_credited", Object.fromEntries(free0));

  // ---------------- operator relays a real reporter-signed print ----------------
  const prints = JSON.parse(readFileSync(env("ZFX_PRINTS"), "utf8"));
  const p = prints.prints.find((x: any) => x.indexId === INDEX_ID);
  const sigs = p.reporterSignatures.map((s: string) => {
    const [, pk, sig] = s.split(":");
    return { public_key: `ed25519:${hexToB58(pk)}`, signature_hex: sig };
  });
  const r = await fetch(`${NODE}/internal/oracle`, { method: "POST", headers: { authorization: `Bearer ${ADMIN}`, "content-type": "application/json" }, body: JSON.stringify({ market: MARKET, print_hex: p.canonicalHex, signatures: sigs }) }).then((x) => x.json());
  step("relay_print", { index_id: INDEX_ID, value_usd: p.value, print_hash: p.printHash, published_at: p.publishedAt, valid_until: p.validUntil, node: r });
  const state = await reader.state();
  const oracle = await maker.client.getOracle(MARKET);
  const markets = await maker.client.listMarkets();
  const m = markets.find((x) => x.marketId === MARKET)!;
  const check = clearing.checkOracle(oracle, { reporters: state.reporters, quorum: state.quorum, market: m });
  step("oracle_checked_against_contract_allowlist", { ok: check.ok, problems: check.problems, mark: oracle.mark, effective_status: m.effectiveStatus });
  if (!check.ok || m.effectiveStatus !== "ACTIVE") throw new Error(`market not tradeable: ${check.problems} ${m.effectiveStatus}`);

  // ---------------- open ----------------
  const mark = oracle.mark;
  const pOpen = mark + mark / 1000n; // 0.1% over the index
  const est = await taker.client.estimateMargin({ marketId: MARKET, side: "BUY", price: pOpen, quantity: QTY });
  step("estimate_margin", { ...est });
  const ask = await maker.client.placeOrder({ marketId: MARKET, side: "SELL", price: pOpen, quantity: QTY, postOnly: true });
  step("maker_post_only_ask", { order_hash: ask.orderHash, status: ask.status, price: pOpen, qty: QTY });
  const bid = await taker.client.placeOrder({ marketId: MARKET, side: "BUY", price: pOpen, quantity: QTY, timeInForce: "IOC" });
  step("taker_ioc_buy", { order_hash: bid.orderHash, status: bid.status, filled: bid.filled });
  const chainPos = async (id: string) => {
    const a: any = await reader.account(id);
    return a.positions.find((x: any) => x.market === MARKET);
  };
  await waitFor("open settled on-chain", 180, async () => (await chainPos(taker.id))?.size === QTY.toString());
  step("open_settled", { taker: await chainPos(taker.id), maker: await chainPos(maker.id) });

  // ---------------- close ----------------
  const pClose = mark + (mark * 3n) / 1000n; // 0.3% over the index
  const bid2 = await maker.client.placeOrder({ marketId: MARKET, side: "BUY", price: pClose, quantity: QTY, postOnly: true });
  step("maker_post_only_bid", { order_hash: bid2.orderHash, status: bid2.status, price: pClose });
  const ask2 = await taker.client.placeOrder({ marketId: MARKET, side: "SELL", price: pClose, quantity: QTY, timeInForce: "IOC", reduceOnly: true });
  step("taker_reduce_only_sell", { order_hash: ask2.orderHash, status: ask2.status, filled: ask2.filled });
  await waitFor("close settled on-chain", 180, async () => !(await chainPos(taker.id)) && !(await chainPos(maker.id)));

  // ---------------- reference math (prices and fee rates only) ----------------
  const tv = m.params.tickValue;
  const fee = (n: bigint, bps: number) => (n * BigInt(bps) + 9_999n) / 10_000n;
  const n1 = QTY * pOpen * tv;
  const n2 = QTY * pClose * tv;
  const [mk, tk] = [m.params.makerFeeBps, m.params.takerFeeBps];
  const expect = new Map([
    [taker.id, free0.get(taker.id)! + (n2 - n1) - fee(n1, tk) - fee(n2, tk)],
    [maker.id, free0.get(maker.id)! - (n2 - n1) - fee(n1, mk) - fee(n2, mk)],
  ]);
  const results: Record<string, unknown> = {};
  for (const t of traders) {
    const onchain = BigInt(((await reader.account(t.id)) as any).free);
    results[t.id] = { expected_free: expect.get(t.id), onchain_free: onchain, match: onchain === expect.get(t.id) };
    if (onchain !== expect.get(t.id)) throw new Error(`${t.id}: on-chain free ${onchain} != reference ${expect.get(t.id)}`);
  }
  step("balances_match_reference_math", results);

  // ---------------- withdraw ----------------
  const ft = async (id: string) => BigInt(await clearing.viewCall<string>(RPC, info.collateral, "ft_balance_of", { account_id: id }));
  const w = expect.get(taker.id)!;
  const b0 = await ft(taker.id);
  const wr = await taker.client.withdraw(w);
  step("withdraw_signed", { amount: w, response: wr });
  await waitFor("withdrawal paid in tUSDC", 180, async () => (await ft(taker.id)) === b0 + w);
  step("withdrawal_paid", { account: taker.id, token_balance_delta: w });

  const relayer = process.env.ZFX_RELAYER_LOG && existsSync(process.env.ZFX_RELAYER_LOG) ? readFileSync(process.env.ZFX_RELAYER_LOG, "utf8").trim().split("\n").map((l) => JSON.parse(l)) : [];
  writeFileSync(OUT, JSON.stringify({ network: "testnet", contract: info.clearingContract, collateral: info.collateral, market: MARKET, steps: log, settlement_batches: relayer }, (_, v) => (typeof v === "bigint" ? v.toString() : v), 1) + "\n");
  console.log(`wrote ${OUT}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
