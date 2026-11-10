export interface AssetInfo {
  id: number;
  symbol: string;
  token: string;
  decimals: number;
}

export interface MarketInfo {
  id: number;
  symbol: string;
  base: string;
  quote: string;
  base_token: string;
  quote_token: string;
  /** Base atoms per lot (decimal string). */
  base_lot: string;
  /** Quote atoms per lot per price tick (decimal string). */
  quote_tick: string;
  min_qty: number;
  max_qty: number;
  max_price: number;
  maker_fee_bps: number;
  taker_fee_bps: number;
  max_open_orders: number;
  kind?: "spot" | "perp";
  perp?: PerpParams | null;
}

export interface PerpParams {
  imr_bps: number;
  mmr_bps: number;
  liq_fee_bps: number;
  price_band_bps: number;
  liq_slippage_bps: number;
  funding_divisor: number;
  max_funding_ppm: number;
}

export interface PerpInfo {
  market: string;
  mark: number;
  index: number;
  open_interest: number;
  last_funding_rate_ppm: number;
  cum_funding_per_lot: string;
  funding_interval_ms: number;
  params: PerpParams;
  oracle: { price: number | null; sources: Array<{ source: string; price?: number; error?: string }>; at_ms: number } | null;
}

export interface PositionView {
  market: string;
  /** Signed lots: positive long, negative short. */
  size: number;
  entry_notional: string;
  /** Average entry price in ticks. */
  entry_price: number;
  mark: number;
  unrealized_pnl: string;
}

/** Cross-margin summary in collateral atoms (decimal strings, may be negative). */
export interface MarginView {
  equity: string;
  initial: string;
  maintenance: string;
  free: string;
  perp_cash: string;
  settled_perp_value: string;
}

export interface Info {
  settlement_contract: string;
  fee_account: string;
  assets: AssetInfo[];
  markets: MarketInfo[];
}

export type Level = [price: number, qty: number];

export interface BookSnapshot {
  market: string;
  seq: number;
  bids: Level[];
  asks: Level[];
}

export interface Trade {
  trade_id: number;
  price: number;
  qty: number;
  taker_side: "buy" | "sell";
  ts_ns: number;
}

export type Ack =
  | { type: "ok" }
  | { type: "duplicate" }
  | { type: "placed"; order_id: number; status: "resting" | "filled" | "canceled"; filled: number; remaining: number }
  | { type: "canceled"; order_id: number; remaining: number }
  | { type: "canceled_all"; count: number }
  | { type: "withdraw_queued"; nonce: number }
  | { type: "batches_cut"; first: number; count: number };

export interface BalanceView {
  token: string;
  symbol: string;
  total: string;
  held: string;
  available: string;
}

export interface OpenOrder {
  order_id: number;
  market: string;
  side: "buy" | "sell";
  price: number;
  remaining: number;
  client_id: number;
}

export interface AccountView {
  account: string;
  balances: BalanceView[];
  open_orders: OpenOrder[];
  withdraw_nonce: number;
  positions: PositionView[];
  margin: MarginView;
}

export type PerpEvent =
  | { channel: "perp"; type: "mark"; market: string; mark: number; index: number }
  | { channel: "perp"; type: "funding"; market: string; rate_ppm: number; payment_per_lot: string }
  | { channel: "perp"; type: "socialized_loss"; amount: string; accounts: number };

/** Private stream events (after WebSocket auth). */
export type AccountEvent =
  | { channel: "account"; type: "accepted"; order_id: number; market: string; side: string; price: number; qty: number; client_id: number; tif: string }
  | { channel: "account"; type: "fill"; market: string; order_id: number; trade_id: number; role: "maker" | "taker"; side: string; price: number; qty: number; fee: string; remaining: number | null; ts_ns: number }
  | { channel: "account"; type: "canceled"; order_id: number; market: string; remaining: number; reason: string }
  | { channel: "account"; type: "rejected"; client_id: number; reason: string }
  | { channel: "account"; type: "deposited" | "credited"; token: string; amount: string }
  | { channel: "account"; type: "withdraw_queued"; token: string; amount: string; nonce: number }
  | { channel: "account"; type: "forced_withdrawal_queued"; request_id: number; token: string; amount: string }
  | { channel: "account"; type: "position"; market: string; size: number; entry_notional: string }
  | { channel: "account"; type: "liquidated"; equity: string; maintenance: string }
  | { channel: "account"; type: "insurance_takeover"; market: string; size: number; price: number }
  | { channel: "account"; type: "bankruptcy"; deficit: string };
