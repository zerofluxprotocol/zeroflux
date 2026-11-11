/**
 * Local margin math, a bigint port of `crates/zfx-perps-clearing/src/ledger.rs`
 * (`initial_margin`, `maintenance_margin`, `fee`, `equity`, `liquidation_price`, and the margin
 * rule in `trade_side`). Amounts are collateral atoms, prices ticks, sizes lots.
 *
 * A position's `margin` absorbs every cash flow (trade cash, fees, settled funding), so its equity
 * at mark `M` is `margin + size * M * tickValue - fundingOwed`. A fresh long's margin is negative
 * (it paid for the lots); its equity is still positive.
 */
import type { MarketParams, Side } from "./types.js";

type Params = Pick<MarketParams, "tickValue" | "initialMarginBps" | "maintenanceMarginBps">;

/** The parts of a position the math needs (a `Position` from the portfolio fits). */
export interface MarginPosition {
  size: bigint;
  margin: bigint;
  /** Accrued funding not yet settled into margin (positive = the position pays). Defaults to 0. */
  fundingOwed?: bigint;
}

const abs = (x: bigint) => (x < 0n ? -x : x);
const max = (a: bigint, b: bigint) => (a > b ? a : b);

/** `ceil(x * bps / 10_000)` for non-negative x. */
export function ceilBps(x: bigint, bps: number | bigint): bigint {
  const n = x * BigInt(bps);
  return (n + 9_999n) / 10_000n;
}

/** `lots * price * tickValue`, in collateral atoms. */
export function notional(params: Pick<MarketParams, "tickValue">, lots: bigint, price: bigint): bigint {
  return lots * price * params.tickValue;
}

export function initialMargin(params: Params, lots: bigint, mark: bigint): bigint {
  return ceilBps(notional(params, abs(lots), mark), params.initialMarginBps);
}

export function maintenanceMargin(params: Params, lots: bigint, mark: bigint): bigint {
  return ceilBps(notional(params, abs(lots), mark), params.maintenanceMarginBps);
}

/** Trading fee: `ceil(notional * bps / 10_000)`. */
export function fee(notionalAtoms: bigint, bps: number): bigint {
  return ceilBps(notionalAtoms, bps);
}

/** Funding owed since the position's funding index: `size * (cumFunding - fundingIndex)`. */
export function fundingOwed(size: bigint, cumFunding: bigint, fundingIndex: bigint): bigint {
  return size * (cumFunding - fundingIndex);
}

/** Equity of a position at `mark`, funding included. */
export function equity(params: Pick<MarketParams, "tickValue">, p: MarginPosition, mark: bigint): bigint {
  return p.margin + p.size * mark * params.tickValue - (p.fundingOwed ?? 0n);
}

/**
 * Display estimate of the mark at which equity equals maintenance margin; `null` for a flat
 * position. Integer division truncates toward zero like the Rust i128 code; floored at 0.
 */
export function liquidationPrice(params: Params, p: MarginPosition): bigint | null {
  if (p.size === 0n) return null;
  const tv = params.tickValue;
  const s = p.size;
  const mmr = BigInt(params.maintenanceMarginBps);
  const cash = p.margin - (p.fundingOwed ?? 0n);
  const denom = tv * (s * 10_000n - abs(s) * mmr);
  if (denom === 0n) return null;
  const px = (-cash * 10_000n) / denom;
  return max(px, 0n);
}

export type EstimateViolation =
  | "NO_MARK"
  | "INVALID_QTY"
  | "INVALID_PRICE"
  | "PRICE_OUT_OF_BAND"
  | "POSITION_LIMIT"
  | "REDUCE_ONLY_VIOLATED"
  | "NEGATIVE_EQUITY"
  | "INSUFFICIENT_FREE";

export interface MarginEstimate {
  /** Position size after the fill. */
  sizeAfter: bigint;
  /** The fill opens, grows or flips the position (the margin rule applies). */
  increasing: boolean;
  notional: bigint;
  /** Fee at the rate used (see `feeBps`). */
  fee: bigint;
  feeBps: number;
  /** Initial margin of the resulting position at the mark. */
  initialMargin: bigint;
  /** Loss marked immediately because the fill price is worse than the mark (0 if better). */
  adverseLoss: bigint;
  /** Position equity at the mark after the fill, before any top-up from free collateral. */
  equityAfter: bigint;
  /** Free collateral the fill moves into the position (0 when reducing). */
  requiredFree: bigint;
  /** Free collateral returned when the fill closes the position to zero. */
  releasedFree: bigint;
  /** Liquidation price of the resulting position after the top-up; null if flat. */
  liquidationPrice: bigint | null;
  violations: EstimateViolation[];
  /** No violations (and enough free collateral, if `free` was given). */
  ok: boolean;
}

export interface EstimateOptions {
  /** Fee rate in bps. Defaults to the maker fee for post-only orders, else max(maker, taker). */
  feeBps?: number;
  postOnly?: boolean;
  reduceOnly?: boolean;
  /** Account free collateral; adds the INSUFFICIENT_FREE check. */
  free?: bigint;
}

/** Market fields `estimateMargin` reads. A `Market` from `listMarkets()` fits. */
export interface EstimateMarket {
  params: Pick<MarketParams, "tickValue" | "initialMarginBps" | "maintenanceMarginBps" | "makerFeeBps" | "takerFeeBps" | "minQty" | "maxQty" | "maxPrice" | "maxPosition" | "priceBandBps">;
  mark: bigint;
}

/**
 * What one fill of `qty` lots at `price` would do to an isolated position, following the
 * ledger's `trade_side`: settle funding, pay trade cash and fee, then for an increasing fill pull
 * `max(0, IM(new size) - equity)` from free collateral. `requiredFree` is therefore
 * `IM + adverse loss + fee - existing equity` (floored at 0).
 */
export function estimateMargin(market: EstimateMarket, position: MarginPosition | undefined, side: Side, price: bigint, qty: bigint, opts: EstimateOptions = {}): MarginEstimate {
  const pp = market.params;
  const mark = market.mark;
  const violations: EstimateViolation[] = [];
  if (mark <= 0n) violations.push("NO_MARK");
  if (qty <= 0n || qty < pp.minQty || qty > pp.maxQty) violations.push("INVALID_QTY");
  if (price <= 0n || price > pp.maxPrice) violations.push("INVALID_PRICE");
  if (mark > 0n && abs(price - mark) * 10_000n > mark * BigInt(pp.priceBandBps)) violations.push("PRICE_OUT_OF_BAND");

  const old = position?.size ?? 0n;
  const delta = side === "BUY" ? qty : -qty;
  const sizeAfter = old + delta;
  const increasing = abs(sizeAfter) > abs(old) || (old !== 0n && sizeAfter !== 0n && old > 0n !== sizeAfter > 0n);
  if (opts.reduceOnly) {
    const reduces = (old > 0n && side === "SELL") || (old < 0n && side === "BUY");
    if (!reduces || qty > abs(old)) violations.push("REDUCE_ONLY_VIOLATED");
  }

  const feeBps = opts.feeBps ?? (opts.postOnly ? pp.makerFeeBps : Math.max(pp.makerFeeBps, pp.takerFeeBps));
  const n = notional(pp, qty, price);
  const f = fee(n, feeBps);
  const cash = delta * price * pp.tickValue;
  // touch(): settle funding into margin, then trade cash and fee.
  const marginAfter = (position?.margin ?? 0n) - (position?.fundingOwed ?? 0n) - cash - f;
  const after: MarginPosition = { size: sizeAfter, margin: marginAfter };
  const equityAfter = equity(pp, after, mark);
  const im = initialMargin(pp, sizeAfter, mark);
  const adverseLoss = max(side === "BUY" ? (price - mark) * qty * pp.tickValue : (mark - price) * qty * pp.tickValue, 0n);

  let requiredFree = 0n;
  let releasedFree = 0n;
  if (increasing) {
    if (abs(sizeAfter) > pp.maxPosition) violations.push("POSITION_LIMIT");
    requiredFree = max(im - equityAfter, 0n);
  } else {
    if (equityAfter < 0n) violations.push("NEGATIVE_EQUITY");
    if (sizeAfter === 0n) releasedFree = max(marginAfter, 0n);
  }
  if (opts.free !== undefined && opts.free < requiredFree) violations.push("INSUFFICIENT_FREE");

  return {
    sizeAfter,
    increasing,
    notional: n,
    fee: f,
    feeBps,
    initialMargin: im,
    adverseLoss,
    equityAfter,
    requiredFree,
    releasedFree,
    liquidationPrice: liquidationPrice(pp, { size: sizeAfter, margin: marginAfter + requiredFree }),
    violations,
    ok: violations.length === 0,
  };
}
