import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { ed25519 } from "@noble/curves/ed25519";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils";
import { base58, base64 } from "@scure/base";
import { KeySigner } from "../src/index.js";
import {
  ClearingBookState,
  ZfxPerpsClient,
  authMessage,
  cancelMessage,
  ceilBps,
  checkOracle,
  clearingTx,
  decodePrint,
  encodePrint,
  equity,
  estimateMargin,
  fee,
  initialMargin,
  liquidationPrice,
  maintenanceMargin,
  marginMessage,
  minNonceMessage,
  nextNonce,
  orderHash,
  orderMessage,
  parseMarket,
  printHash,
  sha256Hex,
  signedOrderToJson,
  verifyPrint,
  withdrawMessage,
  type MarketParams,
  type OracleView,
  type OraclePrint,
  type OrderFields,
} from "../src/clearing/index.js";

const v = JSON.parse(readFileSync(new URL("./clearing_vectors.json", import.meta.url), "utf8"));
const seed = hexToBytes(v.seed_hex);
const signer = KeySigner.fromSeed(seed);
const byName: Record<string, { message: string; sha256: string; signature_hex: string }> = Object.fromEntries(v.messages.map((m: any) => [m.name, m]));
const C = ["near:testnet", "perps.zeroflux.testnet", "alice.testnet"] as const;
const enc = (s: string) => new TextEncoder().encode(s);

const f = v.order_fields;
const limitOrder: OrderFields = {
  chainId: f.chainId,
  clearingContract: f.clearingContract,
  accountId: f.accountId,
  marketId: f.marketId,
  side: f.side,
  type: f.type,
  price: BigInt(f.price),
  quantity: BigInt(f.quantity),
  reduceOnly: f.reduceOnly,
  postOnly: f.postOnly,
  timeInForce: f.timeInForce,
  nonce: BigInt(f.nonce),
  expiryNs: BigInt(f.expiryNs),
  clientOrderId: BigInt(f.clientOrderId),
  maxFeeBps: f.maxFeeBps,
  policyHash: f.policyHash,
};
const U64 = (1n << 64n) - 1n;
const marketOrder: OrderFields = {
  ...limitOrder,
  side: "SELL",
  type: "MARKET",
  price: 1n,
  quantity: U64,
  reduceOnly: true,
  postOnly: false,
  timeInForce: "IOC",
  nonce: U64,
  expiryNs: U64,
  clientOrderId: 0n,
  maxFeeBps: 1000,
};

const built: Record<string, string> = {
  order_limit: orderMessage(limitOrder),
  order_market: orderMessage(marketOrder),
  cancel: cancelMessage(...C, v.order_hash, 1_800_000_000_000_000_000n),
  min_nonce: minNonceMessage(...C, 100n),
  withdraw: withdrawMessage(...C, "usdc.zeroflux.testnet", (1n << 128n) - 1n, 3n),
  margin_add: marginMessage(...C, "OPUS-PERP", 5_000_000n, 4n),
  margin_remove: marginMessage(...C, "OPUS-PERP", -5_000_000n, 5n),
  auth: authMessage(...C, 1_700_000_000_004n),
};

describe("clearing wire parity with Rust (clearing_vectors.json)", () => {
  it("the seed gives the vector public key", () => {
    expect(signer.publicKey).toBe(v.public_key);
  });

  it("covers every vector message", () => {
    expect(Object.keys(built).sort()).toEqual(Object.keys(byName).sort());
  });

  for (const name of Object.keys(built)) {
    it(`${name}: message, sha256 and signature are byte-identical`, () => {
      const want = byName[name];
      expect(built[name]).toBe(want.message);
      expect(sha256Hex(built[name])).toBe(want.sha256);
      const sig = signer.sign(enc(built[name]));
      expect(bytesToHex(sig)).toBe(want.signature_hex);
      const pk = base58.decode(v.public_key.slice(8));
      expect(ed25519.verify(sig, enc(want.message), pk)).toBe(true);
      expect(ed25519.verify(sig, enc(want.message + "x"), pk)).toBe(false);
    });
  }

  it("order hash", () => {
    expect(orderHash(limitOrder)).toBe(v.order_hash);
    expect(orderHash(limitOrder)).toBe(byName.order_limit.sha256);
  });

  it("SignedOrder JSON uses the spec field names and decimal strings", () => {
    const j = signedOrderToJson({ ...limitOrder, publicKey: v.public_key, signature: "x" });
    const { publicKey, signature, ...rest } = j;
    expect(rest).toEqual(v.order_fields);
    expect(publicKey).toBe(v.public_key);
    expect(signature).toBe("x");
  });

  it("rejects separators, bad identifiers, upper-case hashes and out-of-range numbers", () => {
    expect(() => orderMessage({ ...limitOrder, accountId: "alice|x.testnet" })).toThrow(/account/);
    expect(() => orderMessage({ ...limitOrder, chainId: "near|testnet" })).toThrow(/chain/);
    expect(() => orderMessage({ ...limitOrder, marketId: "opus-perp" })).toThrow(/market/);
    expect(() => orderMessage({ ...limitOrder, policyHash: "AB".repeat(32) })).toThrow(/policy/);
    expect(() => orderMessage({ ...limitOrder, price: U64 + 1n })).toThrow(/price/);
    expect(() => orderMessage({ ...limitOrder, maxFeeBps: 70_000 })).toThrow(/maxFeeBps/);
    expect(() => orderMessage({ ...limitOrder, side: "buy" as any })).toThrow(/side/);
    expect(() => withdrawMessage(...C, "usdc|x", 1n, 1n)).toThrow(/token/);
    expect(() => withdrawMessage(...C, "usdc.near", 1n << 128n, 1n)).toThrow(/amount/);
    expect(() => marginMessage(...C, "OPUS-PERP", -(1n << 127n) - 1n, 1n)).toThrow(/delta/);
    expect(() => cancelMessage(...C, "00", 1n)).toThrow(/order hash/);
  });
});

// ------------------------------------------------------------------ OraclePrint

const pf = v.oracle_print.fields;
const vectorPrint: OraclePrint = {
  indexId: pf.indexId,
  version: pf.version,
  valueScale: pf.valueScale,
  value: BigInt(pf.value),
  sourceStartMs: BigInt(pf.sourceStartMs),
  sourceEndMs: BigInt(pf.sourceEndMs),
  observedAtMs: BigInt(pf.observedAtMs),
  publishedAtMs: BigInt(pf.publishedAtMs),
  validUntilMs: BigInt(pf.validUntilMs),
  sourceHash: pf.sourceHash,
  formulaHash: pf.formulaHash,
  status: pf.status,
  dataStatus: pf.dataStatus,
  finality: pf.finality,
  revision: pf.revision,
  prevPrintHash: pf.prevPrintHash,
};
const printBytes = hexToBytes(v.oracle_print.canonical_hex);

describe("OraclePrint v1", () => {
  it("encodes to the canonical bytes", () => {
    const b = encodePrint(vectorPrint);
    expect(bytesToHex(b)).toBe(v.oracle_print.canonical_hex);
    expect(b.length).toBe(19 + 4 + 37 + 4 + 25 + 1 + 8 + 40 + 64 + 3 + 4 + 32);
  });

  it("decodes the canonical bytes and round-trips", () => {
    expect(decodePrint(printBytes)).toEqual(vectorPrint);
    expect(bytesToHex(encodePrint(decodePrint(printBytes)))).toBe(v.oracle_print.canonical_hex);
  });

  it("hash and reporter signature match", () => {
    expect(printHash(printBytes)).toBe(v.oracle_print.print_hash);
    expect(bytesToHex(signer.sign(printBytes))).toBe(v.oracle_print.signature_hex);
  });

  it("decoding is strict", () => {
    const code = (b: Uint8Array) => {
      try {
        decodePrint(b);
        return "ok";
      } catch (e: any) {
        return e.code;
      }
    };
    const t = new Uint8Array([...printBytes, 0]);
    expect(code(t)).toBe("TRAILING_BYTES");
    const d = printBytes.slice();
    d[0] = "X".charCodeAt(0);
    expect(code(d)).toBe("BAD_DOMAIN");
    const e = printBytes.slice();
    e[e.length - 37] = 9; // finality
    expect(code(e)).toBe("BAD_ENUM");
    expect(code(printBytes.slice(0, 10))).toBe("TRUNCATED");
    expect(code(printBytes.slice(0, printBytes.length - 1))).toBe("TRUNCATED");
    const long = printBytes.slice();
    new DataView(long.buffer).setUint32(19, 257, true);
    expect(code(long)).toBe("STRING_TOO_LONG");
    expect(() => encodePrint({ ...vectorPrint, indexId: "x".repeat(257) })).toThrow();
    // Bad UTF-8 inside index_id.
    const u = printBytes.slice();
    u[23] = 0xff;
    expect(code(u)).toBe("BAD_UTF8");
  });

  const sigHex = v.oracle_print.signature_hex;
  const other = KeySigner.fromSeed(new Uint8Array(32).fill(9));
  const otherSig = bytesToHex(other.sign(printBytes));
  const third = KeySigner.fromSeed(new Uint8Array(32).fill(3));

  it("verifyPrint: one valid allowlisted reporter meets quorum 1", () => {
    const r = verifyPrint(printBytes, [{ publicKey: v.public_key, signatureHex: sigHex }], [v.public_key], 1);
    expect(r.ok).toBe(true);
    expect(r.signers).toHaveLength(1);
  });

  it("verifyPrint: a duplicate signer counts once", () => {
    const sigs = [
      { publicKey: v.public_key, signatureHex: sigHex },
      { publicKey: v.public_key, signatureHex: sigHex },
    ];
    const r = verifyPrint(printBytes, sigs, [v.public_key, other.publicKey], 2);
    expect(r.ok).toBe(false);
    expect(r.signers).toHaveLength(1);
    expect(r.rejected).toEqual([{ publicKey: v.public_key, reason: "DUPLICATE_SIGNER" }]);
  });

  it("verifyPrint: a valid signature from a non-allowlisted key does not count", () => {
    const sigs = [
      { publicKey: v.public_key, signatureHex: sigHex },
      { publicKey: other.publicKey, signatureHex: otherSig },
    ];
    const r = verifyPrint(printBytes, sigs, [v.public_key, third.publicKey], 2);
    expect(r.ok).toBe(false);
    expect(r.rejected).toEqual([{ publicKey: other.publicKey, reason: "NOT_ALLOWLISTED" }]);
    // With both allowlisted, quorum 2 is met. Hex public keys work too.
    const hexPk = bytesToHex(base58.decode(other.publicKey.slice(8)));
    expect(verifyPrint(printBytes, sigs, [v.public_key, hexPk], 2).ok).toBe(true);
  });

  it("verifyPrint: bad signatures, tampered bytes and quorum 0 fail", () => {
    const bad = sigHex.slice(0, -2) + (sigHex.endsWith("00") ? "01" : "00");
    expect(verifyPrint(printBytes, [{ publicKey: v.public_key, signatureHex: bad }], [v.public_key], 1).rejected[0].reason).toBe("BAD_SIGNATURE");
    const tampered = printBytes.slice();
    tampered[60] ^= 1;
    expect(verifyPrint(tampered, [{ publicKey: v.public_key, signatureHex: sigHex }], [v.public_key], 1).ok).toBe(false);
    expect(verifyPrint(printBytes, [{ publicKey: v.public_key, signatureHex: sigHex }], [v.public_key], 0).ok).toBe(false);
    expect(verifyPrint(printBytes, [{ public_key: v.public_key, signature_hex: sigHex }], [v.public_key], 1).ok).toBe(true);
  });

  const view = (over: Partial<OracleView> = {}): OracleView => ({
    market: "OPUS-PERP",
    indexId: vectorPrint.indexId,
    indexVersion: vectorPrint.version,
    mark: 42_500n,
    markFresh: true,
    effectiveStatus: "ACTIVE",
    reporters: [v.public_key],
    quorum: 1,
    print: {
      hash: v.oracle_print.print_hash,
      canonicalHex: v.oracle_print.canonical_hex,
      signatures: [{ publicKey: v.public_key, signatureHex: sigHex }],
      value: vectorPrint.value,
      status: vectorPrint.status,
      dataStatus: vectorPrint.dataStatus,
      sourceStartMs: vectorPrint.sourceStartMs,
      sourceEndMs: vectorPrint.sourceEndMs,
      publishedAtMs: vectorPrint.publishedAtMs,
      validUntilMs: vectorPrint.validUntilMs,
      revision: vectorPrint.revision,
    },
    ...over,
  });
  const nowMs = 1_767_340_900_000;

  it("checkOracle passes a consistent, signed, fresh print and fails closed otherwise", () => {
    expect(checkOracle(view(), { nowMs })).toMatchObject({ ok: true, problems: [] });
    expect(checkOracle(view(), { nowMs: 1_767_484_800_000 }).problems).toContain("EXPIRED");
    expect(checkOracle(view({ print: null })).problems).toEqual(["NO_PRINT"]);
    expect(checkOracle(view(), { nowMs, reporters: [other.publicKey] }).problems).toContain("QUORUM_NOT_MET");
    const p = view().print!;
    expect(checkOracle(view({ print: { ...p, value: 1n } }), { nowMs }).problems).toContain("FIELD_MISMATCH");
    expect(checkOracle(view({ print: { ...p, hash: "00".repeat(32) } }), { nowMs }).problems).toContain("HASH_MISMATCH");
    expect(checkOracle(view({ print: { ...p, canonicalHex: "00" } }), { nowMs }).problems).toEqual(["DECODE_FAILED"]);
    expect(checkOracle(view({ markFresh: false }), { nowMs }).problems).toContain("MARK_NOT_FRESH");
    const market = { ...testMarket, params: { ...testMarket.params, indexId: vectorPrint.indexId, indexVersion: vectorPrint.version, valueScale: 6, indexUnitsPerTick: 1000n } };
    expect(checkOracle(view(), { nowMs, market }).ok).toBe(true);
    expect(checkOracle(view({ mark: 1n }), { nowMs, market }).problems).toContain("MARK_MISMATCH");
    expect(checkOracle(view(), { nowMs, market: { ...market, params: { ...market.params, valueScale: 2 } } }).problems).toContain("SCALE_MISMATCH");
  });
});

// ------------------------------------------------------------------ margin math

const params: MarketParams = {
  marketId: "OPUS-PERP",
  marketType: "Perpetual",
  indexId: "idx",
  indexVersion: "v1",
  valueScale: 6,
  indexUnitsPerTick: 1n,
  tickValue: 1000n,
  minQty: 1n,
  maxQty: 10_000n,
  maxPrice: 1_000_000n,
  initialMarginBps: 5000,
  maintenanceMarginBps: 2500,
  maxLeverage: 2,
  maxOpenInterest: 1_000_000n,
  maxPosition: 500n,
  makerFeeBps: 2,
  takerFeeBps: 5,
  liqFeeBps: 50,
  liqSlippageBps: 500,
  priceBandBps: 2000,
  oracleMaxAgeSec: 172_800n,
  fundingIntervalSec: 3600n,
  fundingDivisor: 24,
  maxFundingPpm: 1000,
  printCloseOnlyFromSec: 0,
  printLagDays: 0,
  markLimitBps: 600,
  haltJumpBps: 0,
  printMoveCloseOnlyBps: 0,
  maxOpenInterestNotional: 0n,
};
const testMarket = { marketId: "OPUS-PERP", params, policyHash: "ab".repeat(32), status: "ACTIVE" as const, effectiveStatus: "ACTIVE" as const, mark: 1000n, indexTicks: 1000n, markFresh: true, openInterest: 0n, badDebt: 0n };

describe("margin math (port of ledger.rs)", () => {
  it("initial and maintenance margin", () => {
    // 100 lots * 1000 ticks * 1000 atoms = 100_000_000 notional; 50% IM, 25% MM.
    expect(initialMargin(params, 100n, 1000n)).toBe(50_000_000n);
    expect(maintenanceMargin(params, 100n, 1000n)).toBe(25_000_000n);
    expect(initialMargin(params, -100n, 1000n)).toBe(50_000_000n);
  });

  it("fees round up", () => {
    expect(fee(101_000_000n, 5)).toBe(50_500n);
    expect(fee(10_001n, 5)).toBe(6n); // 5.0005 -> 6
    expect(fee(10_000n, 5)).toBe(5n);
    expect(fee(0n, 5)).toBe(0n);
    expect(ceilBps(1n, 1)).toBe(1n);
  });

  it("equity includes unrealized PnL and funding", () => {
    // Long 100 bought at 1000: margin = 50_000_000 deposited - 100_000_000 paid.
    const p = { size: 100n, margin: -50_000_000n };
    expect(equity(params, p, 1000n)).toBe(50_000_000n);
    expect(equity(params, p, 900n)).toBe(40_000_000n);
    expect(equity(params, { ...p, fundingOwed: 1_000n }, 1000n)).toBe(49_999_000n);
  });

  it("liquidation price, truncating toward zero like i128", () => {
    // -cash*1e4 / (tv*(s*1e4 - |s|*mmr)) = 5e7*1e4 / (1000*750_000) = 666.67 -> 666
    expect(liquidationPrice(params, { size: 100n, margin: -50_000_000n })).toBe(666n);
    // Short 100 sold at 1000 with 5e7 margin: 1.5e8*1e4 / (1000*1_250_000) = 1200 exactly.
    expect(liquidationPrice(params, { size: -100n, margin: 150_000_000n })).toBe(1200n);
    expect(liquidationPrice(params, { size: 0n, margin: 5n })).toBeNull();
    // A long with more cash than notional can't be liquidated: floored at 0.
    expect(liquidationPrice(params, { size: 1n, margin: 5_000_000n })).toBe(0n);
  });

  it("estimateMargin: open a long above the mark", () => {
    // Buy 100 @ 1010, mark 1000. Notional 101_000_000, fee max(2,5)=5 bps = 50_500,
    // IM at mark 50_000_000, adverse loss 10 ticks * 100 * 1000 = 1_000_000.
    const e = estimateMargin(testMarket, undefined, "BUY", 1010n, 100n);
    expect(e.notional).toBe(101_000_000n);
    expect(e.fee).toBe(50_500n);
    expect(e.initialMargin).toBe(50_000_000n);
    expect(e.adverseLoss).toBe(1_000_000n);
    expect(e.equityAfter).toBe(-1_050_500n);
    expect(e.requiredFree).toBe(51_050_500n);
    expect(e.requiredFree).toBe(e.initialMargin + e.adverseLoss + e.fee);
    expect(e.ok).toBe(true);
    expect(estimateMargin(testMarket, undefined, "BUY", 1010n, 100n, { free: 51_050_499n }).violations).toEqual(["INSUFFICIENT_FREE"]);
    expect(estimateMargin(testMarket, undefined, "BUY", 1010n, 100n, { postOnly: true }).fee).toBe(20_200n);
  });

  it("estimateMargin: reducing needs no free collateral and releases margin on close", () => {
    const pos = { size: 100n, margin: -50_000_000n };
    const e = estimateMargin(testMarket, pos, "SELL", 1000n, 100n, { reduceOnly: true });
    expect(e.increasing).toBe(false);
    expect(e.requiredFree).toBe(0n);
    // -50_000_000 + 100_000_000 - fee 50_000
    expect(e.releasedFree).toBe(49_950_000n);
    expect(e.liquidationPrice).toBeNull();
    expect(e.ok).toBe(true);
    expect(estimateMargin(testMarket, pos, "BUY", 1000n, 1n, { reduceOnly: true }).violations).toContain("REDUCE_ONLY_VIOLATED");
  });

  it("estimateMargin: a flip is increasing; existing equity counts", () => {
    // Long 10 with equity 5_000_000 at mark 1000, sell 30 @ 1000 -> short 20.
    const pos = { size: 10n, margin: -5_000_000n };
    const e = estimateMargin(testMarket, pos, "SELL", 1000n, 30n, { feeBps: 0 });
    expect(e.sizeAfter).toBe(-20n);
    expect(e.increasing).toBe(true);
    expect(e.initialMargin).toBe(10_000_000n);
    expect(e.equityAfter).toBe(5_000_000n);
    expect(e.requiredFree).toBe(5_000_000n);
  });

  it("estimateMargin: band, limits and missing mark", () => {
    expect(estimateMargin(testMarket, undefined, "BUY", 1300n, 1n).violations).toContain("PRICE_OUT_OF_BAND");
    expect(estimateMargin(testMarket, undefined, "BUY", 1000n, 501n).violations).toContain("POSITION_LIMIT");
    expect(estimateMargin(testMarket, undefined, "BUY", 1000n, 0n).violations).toContain("INVALID_QTY");
    expect(estimateMargin({ ...testMarket, mark: 0n }, undefined, "BUY", 1000n, 1n).violations).toContain("NO_MARK");
  });
});

// ------------------------------------------------------------------ book, nonces, wallet txs, client

describe("ClearingBookState", () => {
  it("applies string levels in order, ignores stale deltas, detects gaps", () => {
    const b = new ClearingBookState("OPUS-PERP");
    expect(b.apply({ type: "delta", seq: 1, bids: [], asks: [] })).toBe("stale"); // before snapshot
    b.apply({ type: "snapshot", seq: 5, bids: [["100", "3"]], asks: [["105", "2"]] });
    expect(b.apply({ type: "delta", seq: 5, bids: [["100", "0"]] })).toBe("stale");
    expect(b.apply({ type: "delta", seq: 6, bids: [["100", "0"], ["99", "7"], ["98", "1"]], asks: [["104", "1"]] })).toBe("applied");
    expect(b.topBids()).toEqual([[99n, 7n], [98n, 1n]]);
    expect(b.topAsks()).toEqual([[104n, 1n], [105n, 2n]]);
    expect(b.apply({ type: "delta", seq: 8 })).toBe("gap");
    expect(b.synced).toBe(false);
    expect(b.apply({ type: "delta", seq: 9 })).toBe("stale");
  });
});

describe("nonces", () => {
  it("strictly increase and start at Date.now()*1000", () => {
    const t = BigInt(Date.now()) * 1000n;
    const xs = Array.from({ length: 5000 }, () => nextNonce());
    expect(xs[0] >= t).toBe(true);
    for (let i = 1; i < xs.length; i++) expect(xs[i] > xs[i - 1]).toBe(true);
  });
});

describe("clearing wallet transactions", () => {
  it("match the contract's methods, args and deposits", () => {
    expect(clearingTx.registerKey("perps.near", "ed25519:abc", { expiresNs: 5n, canWithdraw: false })).toEqual({
      receiverId: "perps.near",
      actions: [{ type: "FunctionCall", params: { methodName: "register_key", args: { public_key: "ed25519:abc", expires_ns: "5", can_withdraw: false }, gas: "30000000000000", deposit: "10000000000000000000000" } }],
    });
    expect(clearingTx.registerKey("perps.near", "ed25519:abc").actions[0].params.args).toEqual({ public_key: "ed25519:abc" });
    const d = clearingTx.deposit("perps.near", "usdc.near", 5_000_000n);
    expect(d.receiverId).toBe("usdc.near");
    expect(d.actions[0].params).toMatchObject({ methodName: "ft_transfer_call", deposit: "1", args: { receiver_id: "perps.near", amount: "5000000", msg: "" } });
    expect(clearingTx.deposit("perps.near", "usdc.near", 1n, "bob.near").actions[0].params.args.msg).toBe("bob.near");
    expect(clearingTx.requestForcedWithdrawal("perps.near", 9n).actions[0].params).toMatchObject({ methodName: "request_forced_withdrawal", args: { amount: "9" }, deposit: "10000000000000000000000" });
    expect(clearingTx.cancelOrder("perps.near", v.order_hash, 7n).actions[0].params.args).toEqual({ order_hash: v.order_hash, expiry_ns: "7" });
    expect(clearingTx.setMinNonce("perps.near", 100n).actions[0].params.args).toEqual({ min_nonce: "100" });
    expect(clearingTx.escapeScan("perps.near", 50).actions[0].params.args).toEqual({ limit: 50 });
    expect(clearingTx.reclaimDeposit("perps.near", 3).actions[0].params.args).toEqual({ deposit_id: "3" });
    for (const t of [clearingTx.removeKey("p.near", "ed25519:a"), clearingTx.freeze("p.near"), clearingTx.escapeWithdraw("p.near")]) expect(t.actions[0].params.deposit).toBe("0");
  });
});

/** Minimal in-memory fake of the node's REST API. */
function fakeNode() {
  const calls: Array<{ method: string; path: string; body: any; headers: Record<string, string> }> = [];
  const market = {
    market_id: "OPUS-PERP",
    params: {
      market_id: "OPUS-PERP", market_type: "Perpetual", index_id: "idx", index_version: "v1", value_scale: 6, index_units_per_tick: "1", tick_value: "1000",
      min_qty: "1", max_qty: "10000", max_price: "1000000", initial_margin_bps: 5000, maintenance_margin_bps: 2500, max_leverage: 2, max_open_interest: "1000000",
      max_position: "500", maker_fee_bps: 2, taker_fee_bps: 5, liq_fee_bps: 50, liq_slippage_bps: 500, price_band_bps: 2000, oracle_max_age_sec: "172800",
      funding_interval_sec: "3600", funding_divisor: 24, max_funding_ppm: 1000, print_close_only_from_sec: 0, print_lag_days: 0, mark_limit_bps: 600, halt_jump_bps: 0,
    },
    policy_hash: "ab".repeat(32), status: "ACTIVE", effective_status: "ACTIVE", mark: "1000", mark_fresh: true, open_interest: "0", bad_debt: "0",
  };
  const fetchImpl = (async (url: string, init: RequestInit = {}) => {
    const u = new URL(url);
    const body = init.body ? JSON.parse(String(init.body)) : undefined;
    calls.push({ method: init.method ?? "GET", path: u.pathname, body, headers: (init.headers ?? {}) as Record<string, string> });
    const json = (x: unknown, status = 200) => new Response(JSON.stringify(x), { status });
    if (u.pathname === "/v1/info") return json({ chain_id: "near:testnet", clearing_contract: "perps.zeroflux.testnet", collateral: "usdc.zeroflux.testnet", insurance_account: "ins.testnet" });
    if (u.pathname === "/v1/markets") return json({ markets: [market] });
    if (u.pathname === "/v1/orders") return json({ order_id: "1", order_hash: body.signature === "tamper" ? "00".repeat(32) : undefined, status: "resting", filled: "0", remaining: body.quantity });
    if (u.pathname === "/v1/portfolio/alice.testnet")
      return json({ account: "alice.testnet", pending: { free: "10", positions: [{ market: "OPUS-PERP", size: "-3", margin: "5", equity: "4", initial_margin: "1", maintenance_margin: "1", liquidation_price: null, mark: "1000", funding_owed: "0" }] }, settled: { free: "10", positions: [] }, min_nonce: "0", action_nonce: "2", open_orders: [], last_settled_batch: "9" });
    if (u.pathname === "/v1/estimate") return json({ initial_margin: "50000000", fee: "50500", required_free: "51050500", free: "0", ok: false });
    if (u.pathname === "/v1/withdraw" && body.amount === "0") return json({ error: "zero_amount", message: "amount must be > 0" }, 409);
    return json({ ok: true });
  }) as typeof fetch;
  return { calls, fetchImpl };
}

describe("ZfxPerpsClient (REST against a fake node)", () => {
  const mk = () => {
    const n = fakeNode();
    return { ...n, c: new ZfxPerpsClient("http://node.test/", { accountId: "alice.testnet", signer, fetch: n.fetchImpl, WebSocket: undefined }) };
  };

  it("signOrder fills deployment, policy, nonce, expiry and fee and signs the canonical message", async () => {
    const { c, calls } = mk();
    const before = BigInt(Date.now());
    const { order, orderHash: h } = await c.signOrder({ marketId: "OPUS-PERP", side: "BUY", price: 1250n, quantity: 12n, postOnly: true });
    expect(order).toMatchObject({ chainId: "near:testnet", clearingContract: "perps.zeroflux.testnet", accountId: "alice.testnet", policyHash: "ab".repeat(32), type: "LIMIT", timeInForce: "GTC", maxFeeBps: 2, publicKey: v.public_key });
    expect(order.expiryNs >= (before + 3_600_000n) * 1_000_000n).toBe(true);
    expect(order.expiryNs <= (BigInt(Date.now()) + 3_600_000n) * 1_000_000n).toBe(true);
    expect(h).toBe(orderHash(order));
    expect(ed25519.verify(base64.decode(order.signature), enc(orderMessage(order)), base58.decode(v.public_key.slice(8)))).toBe(true);
    const o2 = (await c.signOrder({ marketId: "OPUS-PERP", side: "SELL", price: 1, quantity: 1, type: "MARKET" })).order;
    expect(o2.timeInForce).toBe("IOC");
    expect(o2.maxFeeBps).toBe(5);
    expect(o2.nonce > order.nonce).toBe(true);
    expect(o2.clientOrderId).not.toBe(order.clientOrderId);
    // info and markets fetched once each
    expect(calls.filter((x) => x.path === "/v1/info")).toHaveLength(1);
    expect(calls.filter((x) => x.path === "/v1/markets")).toHaveLength(1);
    await expect(c.signOrder({ marketId: "OPUS-PERP", side: "BUY", price: 1, quantity: 20_000 })).rejects.toThrow(/quantity/);
    await expect(c.signOrder({ marketId: "OPUS-PERP", side: "BUY", price: 1, quantity: 1, type: "MARKET", timeInForce: "GTC" })).rejects.toThrow(/MARKET/);
    await expect(c.signOrder({ marketId: "NOPE-PERP", side: "BUY", price: 1, quantity: 1 })).rejects.toThrow(/unknown market/);
  });

  it("placeOrder posts the spec JSON and checks the node's order hash", async () => {
    const { c, calls } = mk();
    const ack = await c.placeOrder({ marketId: "OPUS-PERP", side: "BUY", price: 1000, quantity: 3 });
    const post = calls.find((x) => x.path === "/v1/orders")!;
    expect(Object.keys(post.body).sort()).toEqual(["accountId", "chainId", "clearingContract", "clientOrderId", "expiryNs", "marketId", "maxFeeBps", "nonce", "policyHash", "postOnly", "price", "publicKey", "quantity", "reduceOnly", "side", "signature", "timeInForce", "type"]);
    expect(post.body.price).toBe("1000");
    expect(ack).toMatchObject({ status: "resting", remaining: 3n, orderHash: orderHash(ack.order) });
    const { order } = await c.signOrder({ marketId: "OPUS-PERP", side: "BUY", price: 1000, quantity: 3 });
    await expect(c.placeOrder({ ...order, signature: "tamper" })).rejects.toThrow(/order hash/);
  });

  it("cancel, min nonce, withdraw and margin sign the canonical messages", async () => {
    const { c, calls } = mk();
    const pk = base58.decode(v.public_key.slice(8));
    const ok = (msg: string, sig: string) => ed25519.verify(base64.decode(sig), enc(msg), pk);

    await c.cancelOrder({ orderHash: v.order_hash, expiryNs: 1_800_000_000_000_000_000n }, { relayOnChain: true });
    const cb = calls.at(-1)!.body;
    expect(cb).toMatchObject({ accountId: "alice.testnet", orderHash: v.order_hash, expiryNs: "1800000000000000000", relayOnChain: true, publicKey: v.public_key });
    expect(cb.signature).toBe(base64.encode(hexToBytes(byName.cancel.signature_hex)));

    const { minNonce } = await c.setMinNonce();
    const mb = calls.at(-1)!.body;
    expect(mb.minNonce).toBe(minNonce.toString());
    expect(ok(minNonceMessage(...C, minNonce), mb.signature)).toBe(true);
    const after = (await c.signOrder({ marketId: "OPUS-PERP", side: "BUY", price: 1000, quantity: 1 })).order;
    expect(after.nonce > minNonce).toBe(true);

    await c.withdraw(3n);
    const wb = calls.at(-1)!.body;
    expect(calls.at(-1)!.path).toBe("/v1/withdraw");
    expect(ok(withdrawMessage(...C, "usdc.zeroflux.testnet", 3n, BigInt(wb.nonce)), wb.signature)).toBe(true);
    await expect(c.withdraw(0n)).rejects.toMatchObject({ status: 409, code: "zero_amount" });

    await c.adjustMargin("OPUS-PERP", -5_000_000n, { nonce: 5n });
    const ab = calls.at(-1)!.body;
    expect(ab).toMatchObject({ marketId: "OPUS-PERP", delta: "-5000000", nonce: "5" });
    expect(ab.signature).toBe(base64.encode(hexToBytes(byName.margin_remove.signature_hex)));
  });

  it("getPortfolio sends auth headers and parses bigints; estimate and deposit", async () => {
    const { c, calls } = mk();
    const p = await c.getPortfolio();
    const h = calls.at(-1)!.headers;
    expect(h["x-zfx-perps-key"]).toBe(v.public_key);
    expect(ed25519.verify(base64.decode(h["x-zfx-perps-signature"]), enc(authMessage(...C, h["x-zfx-perps-ts"])), base58.decode(v.public_key.slice(8)))).toBe(true);
    expect(p.pending.positions[0]).toMatchObject({ size: -3n, liquidationPrice: null });
    expect(p.actionNonce).toBe(2n);
    const e = await c.estimateMargin({ marketId: "OPUS-PERP", side: "BUY", price: 1010n, quantity: 100n });
    expect(calls.at(-1)!.body).toEqual({ marketId: "OPUS-PERP", side: "BUY", price: "1010", quantity: "100", accountId: "alice.testnet" });
    expect(e.requiredFree).toBe(51_050_500n);
    let sent: unknown;
    const d = await c.deposit(7n, { send: async (tx) => ((sent = tx), "hash") });
    expect(d.tx.receiverId).toBe("usdc.zeroflux.testnet");
    expect(d.tx.actions[0].params.args).toEqual({ receiver_id: "perps.zeroflux.testnet", amount: "7", msg: "" });
    expect(d.result).toBe("hash");
    expect(sent).toBe(d.tx);
  });

  it("parseMarket accepts serde numbers but rejects unsafe ones", () => {
    const m = parseMarket({ market_id: "X-PERP", params: { ...fakeParamsJson(), tick_value: 1000 }, policy_hash: "ab".repeat(32), status: "ACTIVE", mark: 5 });
    expect(m.params.tickValue).toBe(1000n);
    expect(() => parseMarket({ market_id: "X-PERP", params: { ...fakeParamsJson(), tick_value: 2 ** 60 }, policy_hash: "", status: "ACTIVE" })).toThrow(/tick_value/);
  });
});

function fakeParamsJson() {
  return {
    market_id: "X-PERP", market_type: "Perpetual", index_id: "i", index_version: "v", value_scale: 6, index_units_per_tick: 1, tick_value: "1", min_qty: 1, max_qty: 2, max_price: 3,
    initial_margin_bps: 1, maintenance_margin_bps: 1, max_leverage: 1, max_open_interest: 1, max_position: 1, maker_fee_bps: 0, taker_fee_bps: 0, liq_fee_bps: 0, liq_slippage_bps: 0,
    price_band_bps: 0, oracle_max_age_sec: 1, funding_interval_sec: 0, funding_divisor: 1, max_funding_ppm: 0, print_close_only_from_sec: 0, print_lag_days: 0, mark_limit_bps: 1, halt_jump_bps: 0,
  };
}

// ------------------------------------------------------------------ WebSocket (fake)

class FakeWS extends EventTarget {
  static last: FakeWS;
  readyState = 0;
  sent: any[] = [];
  constructor(readonly url: string) {
    super();
    FakeWS.last = this;
    queueMicrotask(() => {
      this.readyState = 1;
      this.dispatchEvent(new Event("open"));
    });
  }
  send(s: string) {
    this.sent.push(JSON.parse(s));
  }
  close() {
    this.readyState = 3;
    this.dispatchEvent(new Event("close"));
  }
  push(m: unknown) {
    const e = new Event("message") as any;
    e.data = JSON.stringify(m);
    this.dispatchEvent(e);
  }
}
const tick = () => new Promise((r) => setTimeout(r, 0));

describe("ZfxPerpsClient WebSocket", () => {
  it("subscribeBook keeps a book and re-subscribes on a gap", async () => {
    const n = fakeNode();
    const c = new ZfxPerpsClient("http://node.test", { fetch: n.fetchImpl, WebSocket: FakeWS as any });
    const seen: number[] = [];
    const sub = c.subscribeBook("OPUS-PERP", (b) => seen.push(b.seq));
    await tick();
    const ws = FakeWS.last;
    expect(ws.url).toBe("ws://node.test/v1/ws");
    expect(ws.sent).toEqual([{ op: "subscribe", channel: "book", market: "OPUS-PERP" }]);
    ws.push({ channel: "book", type: "snapshot", market: "OPUS-PERP", seq: 10, bids: [["999", "4"]], asks: [["1001", "2"]] });
    await sub.ready;
    ws.push({ channel: "book", type: "delta", market: "OPUS-PERP", seq: 11, bids: [["998", "1"]], asks: [] });
    ws.push({ channel: "book", type: "delta", market: "OPUS-PERP", seq: 13, bids: [], asks: [] });
    expect(sub.resyncs).toBe(1);
    expect(ws.sent.at(-1)).toEqual({ op: "subscribe", channel: "book", market: "OPUS-PERP" });
    ws.push({ channel: "book", type: "delta", market: "OPUS-PERP", seq: 14, bids: [], asks: [] }); // ignored until snapshot
    ws.push({ channel: "book", type: "snapshot", market: "OPUS-PERP", seq: 20, bids: [["990", "1"]], asks: [] });
    expect(seen).toEqual([10, 11, 20]);
    expect(sub.book.topBids()).toEqual([[990n, 1n]]);
    sub.close();
    expect(ws.readyState).toBe(3);
  });

  it("subscribeOrders authenticates and delivers account events", async () => {
    const n = fakeNode();
    const c = new ZfxPerpsClient("http://node.test", { accountId: "alice.testnet", signer, fetch: n.fetchImpl, WebSocket: FakeWS as any });
    const events: any[] = [];
    const sub = c.subscribeOrders((e) => events.push(e));
    for (let i = 0; i < 5; i++) await tick();
    const ws = FakeWS.last;
    const auth = ws.sent[0];
    expect(auth).toMatchObject({ op: "auth", account: "alice.testnet", public_key: v.public_key });
    expect(ed25519.verify(base64.decode(auth.signature), enc(authMessage(...C, auth.ts)), base58.decode(v.public_key.slice(8)))).toBe(true);
    ws.push({ op: "auth", ok: true });
    await sub.ready;
    ws.push({ channel: "account", type: "fill", market: "OPUS-PERP", qty: "1" });
    ws.push({ channel: "account", type: "settled", batch: "4" });
    expect(events.map((e) => e.type)).toEqual(["fill", "settled"]);
    sub.close();
  });

  it("subscribeOrders rejects ready on auth failure", async () => {
    const n = fakeNode();
    const c = new ZfxPerpsClient("http://node.test", { accountId: "alice.testnet", signer, fetch: n.fetchImpl, WebSocket: FakeWS as any, reconnect: false });
    const sub = c.subscribeOrders(() => {});
    for (let i = 0; i < 5; i++) await tick();
    FakeWS.last.push({ op: "auth", ok: false, error: "bad_signature" });
    await expect(sub.ready).rejects.toThrow(/bad_signature/);
    sub.close();
  });
});
