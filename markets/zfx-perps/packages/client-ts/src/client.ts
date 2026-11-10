import type { Ack, AccountView, BookSnapshot, Info, PerpInfo, Trade } from "./types.js";
import { authMessage, cancelAllMessage, cancelMessage, orderMessage, withdrawMessage, type Side, type Stp, type Tif } from "./wire.js";
import { signB64, type TradingSigner } from "./signer.js";

export class ObkitError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(`${status} ${code}: ${message}`);
  }
}

export interface OrderParams {
  market: string;
  side: Side;
  /** Price in ticks. */
  price: number | bigint;
  /** Quantity in lots. */
  qty: number | bigint;
  tif?: Tif;
  stp?: Stp;
  /** Make every order message unique; also lets you cancel by client id. Defaults to a counter. */
  clientId?: number | bigint;
}

type Fetch = typeof fetch;

/** Trading client for one account. Works in browsers and Node 18+. */
export class ObkitClient {
  readonly baseUrl: string;
  private nextClientId: bigint;

  constructor(
    baseUrl: string,
    readonly account: string,
    readonly signer: TradingSigner,
    private readonly fetchImpl: Fetch = globalThis.fetch.bind(globalThis),
  ) {
    this.baseUrl = baseUrl.replace(/\/$/, "");
    // Start client ids from the clock so restarts don't reuse ids within the replay window.
    this.nextClientId = BigInt(Date.now()) * 1000n;
  }

  private async req<T>(method: string, path: string, body?: unknown, headers: Record<string, string> = {}): Promise<T> {
    const r = await this.fetchImpl(`${this.baseUrl}${path}`, {
      method,
      headers: { ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await r.text();
    const v = text ? JSON.parse(text) : {};
    if (!r.ok) throw new ObkitError(r.status, v.error ?? "http_error", v.message ?? text);
    return v as T;
  }

  info(): Promise<Info> {
    return this.req("GET", "/v1/info");
  }

  health(): Promise<{ ok: boolean; seq: number; durable_seq: number }> {
    return this.req("GET", "/v1/health");
  }

  book(market: string, depth = 50): Promise<BookSnapshot> {
    return this.req("GET", `/v1/book/${market}?depth=${depth}`);
  }

  /** Mark, index, open interest, funding and oracle status of a perp market. */
  perp(market: string): Promise<PerpInfo> {
    return this.req("GET", `/v1/perp/${market}`);
  }

  async trades(market: string, limit = 100): Promise<Trade[]> {
    return (await this.req<{ trades: Trade[] }>("GET", `/v1/trades/${market}?limit=${limit}`)).trades;
  }

  /** Signed order body without sending it. */
  async signedOrder(p: OrderParams, tsMs = Date.now()) {
    const tif = p.tif ?? "gtc";
    const stp = p.stp ?? "cancel_taker";
    const clientId = p.clientId ?? this.nextClientId++;
    const msg = orderMessage({ account: this.account, market: p.market, side: p.side, price: p.price, qty: p.qty, tif, stp, clientId, tsMs });
    return {
      account: this.account,
      market: p.market,
      side: p.side,
      price: Number(p.price),
      qty: Number(p.qty),
      tif,
      stp,
      client_id: Number(clientId),
      ts: tsMs,
      public_key: this.signer.publicKey,
      signature: await signB64(this.signer, msg),
    };
  }

  async place(p: OrderParams): Promise<Ack> {
    return this.req("POST", "/v1/orders", await this.signedOrder(p));
  }

  async cancel(market: string, orderId: number): Promise<Ack> {
    return this.cancelInner(market, orderId, 0);
  }

  async cancelByClientId(market: string, clientId: number): Promise<Ack> {
    return this.cancelInner(market, 0, clientId);
  }

  private async cancelInner(market: string, orderId: number, clientId: number): Promise<Ack> {
    const ts = Date.now();
    const signature = await signB64(this.signer, cancelMessage(this.account, market, orderId, clientId, ts));
    return this.req("POST", "/v1/orders/cancel", { account: this.account, market, order_id: orderId, client_id: clientId, ts, public_key: this.signer.publicKey, signature });
  }

  async cancelAll(market?: string): Promise<Ack> {
    const ts = Date.now();
    const signature = await signB64(this.signer, cancelAllMessage(this.account, market ?? null, ts));
    return this.req("POST", "/v1/orders/cancel_all", { account: this.account, market: market ?? null, ts, public_key: this.signer.publicKey, signature });
  }

  /** Withdraw `amount` (smallest unit) of `token` back to this account on NEAR. */
  async withdraw(token: string, amount: bigint | string, opts: { nonce?: number; contract?: string } = {}): Promise<Ack> {
    const contract = opts.contract ?? (await this.info()).settlement_contract;
    const nonce = opts.nonce ?? (await this.accountView()).withdraw_nonce + 1;
    const amt = BigInt(amount).toString();
    const signature = await signB64(this.signer, withdrawMessage(contract, this.account, token, amt, nonce));
    return this.req("POST", "/v1/withdraw", { account: this.account, token, amount: amt, nonce, public_key: this.signer.publicKey, signature });
  }

  async accountView(): Promise<AccountView> {
    const ts = Date.now();
    const signature = await signB64(this.signer, authMessage(this.account, ts));
    return this.req("GET", `/v1/account/${this.account}`, undefined, { "x-obkit-key": this.signer.publicKey, "x-obkit-ts": String(ts), "x-obkit-signature": signature });
  }

  /** WebSocket auth message for the private account channel. */
  async wsAuth() {
    const ts = Date.now();
    return { op: "auth", account: this.account, ts, public_key: this.signer.publicKey, signature: await signB64(this.signer, authMessage(this.account, ts)) };
  }
}
