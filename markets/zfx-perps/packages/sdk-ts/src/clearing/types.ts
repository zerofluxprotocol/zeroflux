/**
 * Types of the v1 clearing API (isolated-margin perpetuals settled on the NEAR clearing contract).
 *
 * Money, prices (market ticks), quantities (lots), nonces and timestamps are `bigint` here and
 * base-10 strings on the wire. Basis points, ppm rates, scales and small enums are `number`.
 */

export type Side = "BUY" | "SELL";
export type OrderType = "LIMIT" | "MARKET";
export type TimeInForce = "GTC" | "IOC" | "FOK";

export type MarketType = "Perpetual" | "ExpiringFuture";
export type MarketStatus = "PROPOSED" | "ACTIVE" | "CLOSE_ONLY" | "HALTED" | "SETTLING" | "CLOSED";
export type OracleStatus = "FRESH" | "STALE" | "DISPUTED" | "UNAVAILABLE";
export type DataStatus = "OK" | "NOT_REPORTED" | "UNPRICED" | "NO_VALID_CLOSE_PRICE";
export type Finality = "PROVISIONAL" | "FINAL" | "CORRECTED";
export type PlaceStatus = "resting" | "filled" | "canceled";

/** Every field of an order that the trading key signs (spec 5.2 `SignedOrder` minus key and signature). */
export interface OrderFields {
  chainId: string;
  clearingContract: string;
  accountId: string;
  marketId: string;
  side: Side;
  type: OrderType;
  /** Limit price in ticks; for MARKET the worst acceptable price. */
  price: bigint;
  /** Lots. */
  quantity: bigint;
  reduceOnly: boolean;
  postOnly: boolean;
  timeInForce: TimeInForce;
  /** Must be >= the account's on-chain `min_nonce` when filled. */
  nonce: bigint;
  /** Unix ns. Unfillable at or after this time. */
  expiryNs: bigint;
  clientOrderId: bigint;
  /** Highest fee rate accepted, bps of notional. */
  maxFeeBps: number;
  /** The market's policy hash (64 lower-case hex). Any parameter change invalidates the order. */
  policyHash: string;
}

export interface SignedOrder extends OrderFields {
  /** `ed25519:<base58>` trading key registered on the clearing contract. */
  publicKey: string;
  /** ed25519 signature over `orderMessage(...)`, base64 (64 bytes). */
  signature: string;
}

/** `SignedOrder` as JSON on the wire (`POST /v1/orders`). */
export interface SignedOrderJson {
  chainId: string;
  clearingContract: string;
  accountId: string;
  marketId: string;
  side: Side;
  type: OrderType;
  price: string;
  quantity: string;
  reduceOnly: boolean;
  postOnly: boolean;
  timeInForce: TimeInForce;
  nonce: string;
  expiryNs: string;
  clientOrderId: string;
  maxFeeBps: number;
  policyHash: string;
  publicKey: string;
  signature: string;
}

/** Spec 5.2 `MarketSpec` in integer units. Every field is covered by the policy hash. */
export interface MarketParams {
  marketId: string;
  marketType: MarketType;
  indexId: string;
  indexVersion: string;
  /** Decimal places of the oracle print value. */
  valueScale: number;
  /** Print value units per price tick: `markTicks = value / indexUnitsPerTick`. */
  indexUnitsPerTick: bigint;
  /** Collateral atoms per lot per tick: `notional = lots * ticks * tickValue`. */
  tickValue: bigint;
  minQty: bigint;
  maxQty: bigint;
  maxPrice: bigint;
  initialMarginBps: number;
  maintenanceMarginBps: number;
  maxLeverage: number;
  maxOpenInterest: bigint;
  maxPosition: bigint;
  makerFeeBps: number;
  takerFeeBps: number;
  liqFeeBps: number;
  liqSlippageBps: number;
  priceBandBps: number;
  oracleMaxAgeSec: bigint;
  fundingIntervalSec: bigint;
  fundingDivisor: number;
  maxFundingPpm: number;
  printCloseOnlyFromSec: number;
  maxJumpBps: number;
}

/** Alias: the spec calls the parameter set `MarketSpec`. */
export type MarketSpec = MarketParams;

export interface Market {
  marketId: string;
  params: MarketParams;
  policyHash: string;
  /** Admin-set status. */
  status: MarketStatus;
  /** Admin status made stricter by the oracle (stale print means CLOSE_ONLY, missing or disputed means HALTED). */
  effectiveStatus: MarketStatus;
  /** Mark in ticks (0 = none yet). */
  mark: bigint;
  markFresh: boolean;
  openInterest: bigint;
  badDebt: bigint;
}

export interface Info {
  chainId: string;
  clearingContract: string;
  /** NEP-141 collateral token contract. */
  collateral: string;
  insuranceAccount: string;
}

/** `[price ticks, total lots]`. */
export type BookLevel = [price: bigint, quantity: bigint];

export interface Book {
  market: string;
  seq: number;
  /** Best first. */
  bids: BookLevel[];
  /** Best first. */
  asks: BookLevel[];
}

/** Decoded OraclePrint v1 (`ZFX-oracle-print-v1`). */
export interface OraclePrint {
  indexId: string;
  version: string;
  valueScale: number;
  value: bigint;
  sourceStartMs: bigint;
  sourceEndMs: bigint;
  observedAtMs: bigint;
  publishedAtMs: bigint;
  validUntilMs: bigint;
  /** 64 lower-case hex. */
  sourceHash: string;
  formulaHash: string;
  status: OracleStatus;
  dataStatus: DataStatus;
  finality: Finality;
  revision: number;
  prevPrintHash: string;
}

export interface PrintSignature {
  /** `ed25519:<base58>` or 64 hex chars. */
  publicKey: string;
  /** 128 hex chars. */
  signatureHex: string;
}

/** `GET /v1/oracle/{market}`: the last accepted print with provenance. */
export interface OracleView {
  market: string;
  indexId: string;
  indexVersion: string;
  mark: bigint;
  markFresh: boolean;
  effectiveStatus: MarketStatus;
  /** Reporter allowlist as reported by the node. Cross-check against the contract's `get_state`. */
  reporters: string[];
  quorum: number;
  print: OraclePrintView | null;
}

export interface OraclePrintView {
  hash: string;
  canonicalHex: string;
  signatures: PrintSignature[];
  value: bigint;
  status: OracleStatus;
  dataStatus: DataStatus;
  sourceStartMs: bigint;
  sourceEndMs: bigint;
  publishedAtMs: bigint;
  validUntilMs: bigint;
  revision: number;
}

export interface FundingApplication {
  tsNs: bigint;
  ratePpm: number;
  payPerLot: bigint;
}

export interface Funding {
  market: string;
  fundingIntervalSec: bigint;
  lastFundingNs: bigint;
  nextFundingNs: bigint;
  lastRatePpm: number;
  /** Cumulative funding paid by one long lot since listing (negative = received). */
  cumFunding: bigint;
  windowVwapE6: bigint;
  history: FundingApplication[];
}

/** `POST /v1/estimate` response. */
export interface ServerEstimate {
  initialMargin: bigint;
  fee: bigint;
  requiredFree: bigint;
  free: bigint;
  ok: boolean;
}

export interface PlaceAck {
  orderId: string;
  orderHash: string;
  status: PlaceStatus;
  filled: bigint;
  remaining: bigint;
}

/** One isolated position, valued at the mark. */
export interface Position {
  market: string;
  /** Signed lots: positive long, negative short. */
  size: bigint;
  /** Collateral in the position, net of trade cash (can be negative for a long). */
  margin: bigint;
  equity: bigint;
  initialMargin: bigint;
  maintenanceMargin: bigint;
  /** Mark (ticks) at which equity equals maintenance margin; null when flat. */
  liquidationPrice: bigint | null;
  mark: bigint;
  /** Funding accrued and not yet settled into margin (positive = the position pays). */
  fundingOwed: bigint;
}

export interface PortfolioSide {
  free: bigint;
  positions: Position[];
}

export interface Portfolio {
  account: string;
  /** Predicted state, including fills not yet settled on NEAR. */
  pending: PortfolioSide;
  /** State confirmed by the clearing contract (last settled batch). */
  settled: PortfolioSide;
  minNonce: bigint;
  actionNonce: bigint;
  /** Open orders as the node returns them (the spec leaves the shape open). */
  openOrders: Record<string, unknown>[];
  lastSettledBatch: bigint;
}

export type AccountEventType = "accepted" | "fill" | "canceled" | "rejected" | "position" | "free" | "fill_reverted" | "settled";

/**
 * Private account channel event. The spec names the event types but not their fields, so the
 * payload is passed through as received (integers stay strings; convert with `toBigInt`).
 */
export interface AccountEvent {
  channel: "account";
  type: AccountEventType | (string & {});
  [field: string]: unknown;
}
