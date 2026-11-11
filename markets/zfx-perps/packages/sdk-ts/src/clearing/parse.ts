/**
 * JSON <-> typed conversions for the v1 clearing API. Integers arrive as base-10 strings (the
 * spec) or, from serde views, as JSON numbers; numbers that are not safe integers are rejected
 * instead of silently losing precision.
 */
import type {
  Book,
  BookLevel,
  Funding,
  Info,
  Market,
  MarketParams,
  OracleView,
  PlaceAck,
  Portfolio,
  PortfolioSide,
  Position,
  ServerEstimate,
  SignedOrder,
  SignedOrderJson,
} from "./types.js";
import { toBigInt } from "./wire.js";

type J = Record<string, any>;

export const big = (v: unknown, what = "integer"): bigint => toBigInt(v as string, what);
const bigOr = (v: unknown, d: bigint, what?: string) => (v === undefined || v === null ? d : big(v, what));
const num = (v: unknown, what = "number"): number => {
  const n = typeof v === "string" ? Number(v) : v;
  if (typeof n !== "number" || !Number.isSafeInteger(n)) throw new Error(`${what} is not an integer: ${JSON.stringify(v)}`);
  return n;
};
const numOr = (v: unknown, d: number, what?: string) => (v === undefined || v === null ? d : num(v, what));

export function parseInfo(j: J): Info {
  return { chainId: j.chain_id, clearingContract: j.clearing_contract, collateral: j.collateral, insuranceAccount: j.insurance_account };
}

export function parseMarketParams(j: J): MarketParams {
  return {
    marketId: j.market_id,
    marketType: j.market_type,
    indexId: j.index_id,
    indexVersion: j.index_version,
    valueScale: num(j.value_scale, "value_scale"),
    indexUnitsPerTick: big(j.index_units_per_tick, "index_units_per_tick"),
    tickValue: big(j.tick_value, "tick_value"),
    minQty: big(j.min_qty, "min_qty"),
    maxQty: big(j.max_qty, "max_qty"),
    maxPrice: big(j.max_price, "max_price"),
    initialMarginBps: num(j.initial_margin_bps, "initial_margin_bps"),
    maintenanceMarginBps: num(j.maintenance_margin_bps, "maintenance_margin_bps"),
    maxLeverage: num(j.max_leverage, "max_leverage"),
    maxOpenInterest: big(j.max_open_interest, "max_open_interest"),
    maxPosition: big(j.max_position, "max_position"),
    makerFeeBps: num(j.maker_fee_bps, "maker_fee_bps"),
    takerFeeBps: num(j.taker_fee_bps, "taker_fee_bps"),
    liqFeeBps: num(j.liq_fee_bps, "liq_fee_bps"),
    liqSlippageBps: num(j.liq_slippage_bps, "liq_slippage_bps"),
    priceBandBps: num(j.price_band_bps, "price_band_bps"),
    oracleMaxAgeSec: big(j.oracle_max_age_sec, "oracle_max_age_sec"),
    fundingIntervalSec: big(j.funding_interval_sec, "funding_interval_sec"),
    fundingDivisor: num(j.funding_divisor, "funding_divisor"),
    maxFundingPpm: num(j.max_funding_ppm, "max_funding_ppm"),
    printCloseOnlyFromSec: num(j.print_close_only_from_sec, "print_close_only_from_sec"),
    printLagDays: num(j.print_lag_days ?? 0, "print_lag_days"),
    markLimitBps: num(j.mark_limit_bps, "mark_limit_bps"),
    haltJumpBps: num(j.halt_jump_bps ?? 0, "halt_jump_bps"),
  };
}

export function parseMarket(j: J): Market {
  return {
    marketId: j.market_id,
    params: parseMarketParams(j.params),
    policyHash: j.policy_hash,
    status: j.status,
    effectiveStatus: j.effective_status ?? j.status,
    mark: bigOr(j.mark, 0n, "mark"),
    indexTicks: bigOr(j.index_ticks, 0n, "index_ticks"),
    markFresh: Boolean(j.mark_fresh),
    openInterest: bigOr(j.open_interest, 0n, "open_interest"),
    badDebt: bigOr(j.bad_debt, 0n, "bad_debt"),
  };
}

const levels = (v: unknown): BookLevel[] => (Array.isArray(v) ? v.map((l: any) => [big(l[0], "price"), big(l[1], "quantity")] as BookLevel) : []);

export function parseBook(j: J): Book {
  return { market: j.market, seq: num(j.seq, "seq"), bids: levels(j.bids), asks: levels(j.asks) };
}

export function parseOracle(j: J): OracleView {
  const p = j.print;
  return {
    market: j.market,
    indexId: j.index_id,
    indexVersion: j.index_version,
    mark: bigOr(j.mark, 0n, "mark"),
    markFresh: Boolean(j.mark_fresh),
    effectiveStatus: j.effective_status,
    reporters: Array.isArray(j.reporters) ? j.reporters : [],
    quorum: numOr(j.quorum, 0, "quorum"),
    print: p
      ? {
          hash: p.hash,
          canonicalHex: p.canonical_hex,
          signatures: (p.signatures ?? []).map((s: J) => ({ publicKey: s.public_key, signatureHex: s.signature_hex })),
          value: big(p.value, "print.value"),
          status: p.status,
          dataStatus: p.data_status,
          sourceStartMs: big(p.source_start_ms, "source_start_ms"),
          sourceEndMs: big(p.source_end_ms, "source_end_ms"),
          publishedAtMs: big(p.published_at_ms, "published_at_ms"),
          validUntilMs: big(p.valid_until_ms, "valid_until_ms"),
          revision: num(p.revision, "revision"),
        }
      : null,
  };
}

export function parseFunding(j: J): Funding {
  return {
    market: j.market,
    fundingIntervalSec: bigOr(j.funding_interval_sec, 0n),
    lastFundingNs: bigOr(j.last_funding_ns, 0n),
    nextFundingNs: bigOr(j.next_funding_ns, 0n),
    lastRatePpm: numOr(j.last_rate_ppm, 0),
    cumFunding: bigOr(j.cum_funding, 0n),
    windowVwapE6: bigOr(j.window_vwap_e6, 0n),
    history: (j.history ?? []).map((h: J) => ({ tsNs: big(h.ts_ns), ratePpm: num(h.rate_ppm), payPerLot: big(h.pay_per_lot) })),
  };
}

export function parseEstimate(j: J): ServerEstimate {
  return { initialMargin: big(j.initial_margin), fee: big(j.fee), requiredFree: big(j.required_free), free: bigOr(j.free, 0n), ok: Boolean(j.ok) };
}

export function parsePlaceAck(j: J): PlaceAck {
  return { orderId: String(j.order_id ?? ""), orderHash: j.order_hash, status: j.status, filled: bigOr(j.filled, 0n), remaining: bigOr(j.remaining, 0n) };
}

export function parsePosition(j: J): Position {
  return {
    market: j.market,
    size: big(j.size, "size"),
    margin: big(j.margin, "margin"),
    equity: bigOr(j.equity, 0n),
    initialMargin: bigOr(j.initial_margin, 0n),
    maintenanceMargin: bigOr(j.maintenance_margin, 0n),
    liquidationPrice: j.liquidation_price === null || j.liquidation_price === undefined ? null : big(j.liquidation_price),
    mark: bigOr(j.mark, 0n),
    fundingOwed: bigOr(j.funding_owed, 0n),
  };
}

const side = (j: J | undefined): PortfolioSide => ({ free: bigOr(j?.free, 0n), positions: (j?.positions ?? []).map(parsePosition) });

export function parsePortfolio(j: J): Portfolio {
  return {
    account: j.account,
    pending: side(j.pending),
    settled: side(j.settled),
    minNonce: bigOr(j.min_nonce, 0n),
    actionNonce: bigOr(j.action_nonce, 0n),
    openOrders: j.open_orders ?? [],
    lastSettledBatch: bigOr(j.last_settled_batch, 0n),
  };
}

export function signedOrderToJson(o: SignedOrder): SignedOrderJson {
  return {
    chainId: o.chainId,
    clearingContract: o.clearingContract,
    accountId: o.accountId,
    marketId: o.marketId,
    side: o.side,
    type: o.type,
    price: o.price.toString(),
    quantity: o.quantity.toString(),
    reduceOnly: o.reduceOnly,
    postOnly: o.postOnly,
    timeInForce: o.timeInForce,
    nonce: o.nonce.toString(),
    expiryNs: o.expiryNs.toString(),
    clientOrderId: o.clientOrderId.toString(),
    maxFeeBps: o.maxFeeBps,
    policyHash: o.policyHash,
    publicKey: o.publicKey,
    signature: o.signature,
  };
}

export function signedOrderFromJson(j: SignedOrderJson): SignedOrder {
  return {
    ...j,
    price: big(j.price, "price"),
    quantity: big(j.quantity, "quantity"),
    nonce: big(j.nonce, "nonce"),
    expiryNs: big(j.expiryNs, "expiryNs"),
    clientOrderId: big(j.clientOrderId, "clientOrderId"),
  };
}
