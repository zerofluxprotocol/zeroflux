/**
 * REST + WebSocket client for a zfx-perps operator node, v1 clearing API (`openapi.yaml`).
 * Uses the global `fetch` and `WebSocket` (browsers, Node 22).
 */
import { base64 } from "@scure/base";
import type { TradingSigner } from "../signer.js";
import { ClearingBookState, type BookMessage } from "./book.js";
import { clearingTx, type WalletTx } from "./near.js";
import { parseBook, parseEstimate, parseFunding, parseInfo, parseMarket, parseOracle, parsePlaceAck, parsePortfolio, signedOrderToJson } from "./parse.js";
import type { AccountEvent, Book, Funding, Info, Market, OracleView, OrderFields, OrderType, PlaceAck, Portfolio, ServerEstimate, Side, SignedOrder, TimeInForce } from "./types.js";
import { assertWellFormed, authMessage, cancelMessage, marginMessage, minNonceMessage, orderHash, orderMessage, toBigInt, U64_MAX, utf8, withdrawMessage, WireError } from "./wire.js";

type IntLike = bigint | number | string;

/** HTTP error from the node: status, the node's `error` code and `message`. */
export class ClearingApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(`${status} ${code}: ${message}`);
    this.name = "ClearingApiError";
  }
}

// ---------------------------------------------------------------------------------------------
// Nonces and client order ids: process-wide, strictly increasing, clock-seeded so restarts move
// forward too (Date.now() * 1000 leaves room for 1000 ids per millisecond before drifting ahead).
// ---------------------------------------------------------------------------------------------

let lastNonce = 0n;
let lastClientOrderId = 0n;
const clockSeed = () => BigInt(Date.now()) * 1000n;

/** Next order/action nonce. Never repeats within a process; at least `Date.now() * 1000`. */
export function nextNonce(): bigint {
  const t = clockSeed();
  lastNonce = t > lastNonce ? t : lastNonce + 1n;
  return lastNonce;
}

/** Next unique client order id within this process. */
export function nextClientOrderId(): bigint {
  const t = clockSeed();
  lastClientOrderId = t > lastClientOrderId ? t : lastClientOrderId + 1n;
  return lastClientOrderId;
}

const NS_PER_MS = 1_000_000n;
export const DEFAULT_ORDER_TTL_MS = 3_600_000;

/** What a caller supplies to `signOrder`; everything else is filled in. */
export interface OrderInput {
  marketId: string;
  side: Side;
  price: IntLike;
  quantity: IntLike;
  /** Default LIMIT. */
  type?: OrderType;
  /** Default GTC for LIMIT, IOC for MARKET. */
  timeInForce?: TimeInForce;
  postOnly?: boolean;
  reduceOnly?: boolean;
  nonce?: IntLike;
  /** Unix ns. Default now + `ttlMs`. */
  expiryNs?: IntLike;
  /** Default 1 hour. */
  ttlMs?: number;
  clientOrderId?: IntLike;
  /** Default: the market's maker fee for post-only orders, else max(maker, taker). */
  maxFeeBps?: number;
}

export interface SignedOrderResult {
  order: SignedOrder;
  orderHash: string;
}

export interface Subscription {
  /** Resolves once the server has acknowledged the subscription (book: first snapshot; orders: auth ok). */
  ready: Promise<void>;
  close(): void;
}

export interface BookSubscription extends Subscription {
  book: ClearingBookState;
  /** Number of re-subscribes after a sequence gap or a lagged notice. */
  readonly resyncs: number;
}

export interface SubscribeOptions {
  onError?: (err: unknown) => void;
}

export interface ClientOptions {
  accountId?: string;
  signer?: TradingSigner;
  /** Defaults to the global `fetch`. */
  fetch?: typeof fetch;
  /** Defaults to the global `WebSocket`. */
  WebSocket?: typeof WebSocket;
  /** Defaults to `baseUrl` with http(s) replaced by ws(s) plus `/v1/ws`. */
  wsUrl?: string;
  /** Reconnect the WebSocket after an unexpected close (default true). */
  reconnect?: boolean;
}

interface BookSub {
  market: string;
  state: ClearingBookState;
  onBook: (book: ClearingBookState) => void;
  onError?: (e: unknown) => void;
  resyncs: number;
  resolveReady: () => void;
}

interface AccountSub {
  onEvent: (e: AccountEvent) => void;
  onError?: (e: unknown) => void;
  resolveReady: () => void;
  rejectReady: (e: unknown) => void;
}

/** Client for one node and (optionally) one trading account. */
export class ZfxPerpsClient {
  readonly baseUrl: string;
  readonly accountId?: string;
  readonly signer?: TradingSigner;
  private readonly fetchImpl: typeof fetch;
  private readonly WS?: typeof WebSocket;
  private readonly wsUrl: string;
  private readonly reconnect: boolean;
  private info?: Promise<Info>;
  private marketCache = new Map<string, Market>();

  private ws?: WebSocket;
  private wsOpen?: Promise<WebSocket>;
  private reconnectDelay = 250;
  private bookSubs = new Map<string, BookSub>();
  private accountSubs = new Set<AccountSub>();
  private authed = false;

  constructor(baseUrl: string, opts: ClientOptions = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.accountId = opts.accountId;
    this.signer = opts.signer;
    this.fetchImpl = opts.fetch ?? globalThis.fetch.bind(globalThis);
    this.WS = opts.WebSocket ?? globalThis.WebSocket;
    this.wsUrl = opts.wsUrl ?? `${this.baseUrl.replace(/^http/, "ws")}/v1/ws`;
    this.reconnect = opts.reconnect ?? true;
  }

  // ------------------------------------------------------------------ REST

  private async req(method: string, path: string, body?: unknown, headers: Record<string, string> = {}): Promise<any> {
    const r = await this.fetchImpl(`${this.baseUrl}${path}`, {
      method,
      headers: { ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await r.text();
    let v: any = {};
    if (text) {
      try {
        v = JSON.parse(text);
      } catch {
        if (r.ok) throw new ClearingApiError(r.status, "bad_json", text.slice(0, 200));
        v = { message: text };
      }
    }
    if (!r.ok) throw new ClearingApiError(r.status, v.error ?? "http_error", v.message ?? text);
    return v;
  }

  private need(): { accountId: string; signer: TradingSigner } {
    if (!this.accountId || !this.signer) throw new Error("this call needs a client constructed with { accountId, signer }");
    return { accountId: this.accountId, signer: this.signer };
  }

  private async sign(message: string): Promise<string> {
    return base64.encode(await this.need().signer.sign(utf8(message)));
  }

  /** Deployment identity (chain id, clearing contract, collateral token). Cached after the first call. */
  getInfo(): Promise<Info> {
    this.info ??= this.req("GET", "/v1/info").then(parseInfo, (e) => {
      this.info = undefined;
      throw e;
    });
    return this.info;
  }

  /** Every market with its spec, policy hash, status and mark. Refreshes the cache `signOrder` uses. */
  async listMarkets(): Promise<Market[]> {
    const v = await this.req("GET", "/v1/markets");
    const ms: Market[] = (v.markets ?? []).map(parseMarket);
    this.marketCache = new Map(ms.map((m) => [m.marketId, m]));
    return ms;
  }

  private async market(marketId: string): Promise<Market> {
    let m = this.marketCache.get(marketId);
    if (!m) {
      await this.listMarkets();
      m = this.marketCache.get(marketId);
    }
    if (!m) throw new Error(`unknown market ${marketId}`);
    return m;
  }

  /** L2 snapshot; `seq` matches WebSocket deltas. */
  async getBook(market: string, depth = 50): Promise<Book> {
    return parseBook(await this.req("GET", `/v1/book/${encodeURIComponent(market)}?depth=${depth}`));
  }

  /** Last accepted oracle print with provenance. Verify it with `checkOracle` before trusting the mark. */
  async getOracle(market: string): Promise<OracleView> {
    return parseOracle(await this.req("GET", `/v1/oracle/${encodeURIComponent(market)}`));
  }

  async getFunding(market: string): Promise<Funding> {
    return parseFunding(await this.req("GET", `/v1/funding/${encodeURIComponent(market)}`));
  }

  /** Free collateral, isolated positions (pending and settled) and open orders. Signed read. */
  async getPortfolio(accountId?: string): Promise<Portfolio> {
    const { signer } = this.need();
    const account = accountId ?? this.accountId!;
    const info = await this.getInfo();
    const ts = String(Date.now());
    const signature = await this.sign(authMessage(info.chainId, info.clearingContract, account, ts));
    const v = await this.req("GET", `/v1/portfolio/${encodeURIComponent(account)}`, undefined, {
      "x-zfx-perps-key": signer.publicKey,
      "x-zfx-perps-ts": ts,
      "x-zfx-perps-signature": signature,
    });
    return parsePortfolio(v);
  }

  /** Margin and fee the node computes for an order (no signature, no side effects). See also the local `estimateMargin`. */
  async estimateMargin(p: { marketId: string; side: Side; price: IntLike; quantity: IntLike; accountId?: string }): Promise<ServerEstimate> {
    const body: Record<string, string> = { marketId: p.marketId, side: p.side, price: toBigInt(p.price, "price").toString(), quantity: toBigInt(p.quantity, "quantity").toString() };
    const acct = p.accountId ?? this.accountId;
    if (acct) body.accountId = acct;
    return parseEstimate(await this.req("POST", "/v1/estimate", body));
  }

  /**
   * Build and sign an order. Fills chain id and clearing contract from `/v1/info`, the policy hash
   * from the market, a fresh nonce, expiry now + 1 h and a unique client order id unless given.
   * Checks the order against the market's limits before signing.
   */
  async signOrder(input: OrderInput): Promise<SignedOrderResult> {
    const { accountId, signer } = this.need();
    const [info, m] = await Promise.all([this.getInfo(), this.market(input.marketId)]);
    const type = input.type ?? "LIMIT";
    const postOnly = input.postOnly ?? false;
    const fields: OrderFields = {
      chainId: info.chainId,
      clearingContract: info.clearingContract,
      accountId,
      marketId: input.marketId,
      side: input.side,
      type,
      price: toBigInt(input.price, "price"),
      quantity: toBigInt(input.quantity, "quantity"),
      reduceOnly: input.reduceOnly ?? false,
      postOnly,
      timeInForce: input.timeInForce ?? (type === "MARKET" ? "IOC" : "GTC"),
      nonce: input.nonce !== undefined ? toBigInt(input.nonce, "nonce") : nextNonce(),
      expiryNs: input.expiryNs !== undefined ? toBigInt(input.expiryNs, "expiryNs") : BigInt(Date.now() + (input.ttlMs ?? DEFAULT_ORDER_TTL_MS)) * NS_PER_MS,
      clientOrderId: input.clientOrderId !== undefined ? toBigInt(input.clientOrderId, "clientOrderId") : nextClientOrderId(),
      maxFeeBps: input.maxFeeBps ?? (postOnly ? m.params.makerFeeBps : Math.max(m.params.makerFeeBps, m.params.takerFeeBps)),
      policyHash: m.policyHash,
    };
    assertWellFormed(fields);
    const p = m.params;
    if (p.marketType !== "Perpetual") throw new WireError(`market ${m.marketId} is ${p.marketType}, not tradeable in v1`);
    if (fields.quantity < p.minQty || fields.quantity > p.maxQty) throw new WireError(`quantity ${fields.quantity} outside [${p.minQty}, ${p.maxQty}]`);
    if (fields.price > p.maxPrice) throw new WireError(`price ${fields.price} above max ${p.maxPrice}`);
    if (fields.nonce > U64_MAX) throw new WireError("nonce out of range");
    const message = orderMessage(fields);
    const signature = base64.encode(await signer.sign(utf8(message)));
    return { order: { ...fields, publicKey: signer.publicKey, signature }, orderHash: orderHash(fields) };
  }

  /**
   * Place an order: either an already signed `SignedOrder` or an `OrderInput` to sign first.
   * Throws if the node reports a different order hash than the one computed locally.
   */
  async placeOrder(o: SignedOrder | OrderInput): Promise<PlaceAck & { order: SignedOrder }> {
    const order = "signature" in o ? o : (await this.signOrder(o)).order;
    const local = orderHash(order);
    const ack = parsePlaceAck(await this.req("POST", "/v1/orders", signedOrderToJson(order)));
    if (ack.orderHash && ack.orderHash !== local) throw new Error(`node order hash ${ack.orderHash} != local ${local}`);
    return { ...ack, orderHash: local, order };
  }

  /** Cancel one order by hash. `relayOnChain` also makes the cancel final on-chain in the next batch. */
  async cancelOrder(target: SignedOrder | { orderHash: string; expiryNs: IntLike }, opts: { relayOnChain?: boolean } = {}): Promise<Record<string, unknown>> {
    const { accountId, signer } = this.need();
    const info = await this.getInfo();
    const h = "signature" in target ? orderHash(target) : target.orderHash;
    const expiryNs = toBigInt(target.expiryNs, "expiryNs").toString();
    const signature = await this.sign(cancelMessage(info.chainId, info.clearingContract, accountId, h, expiryNs));
    return this.req("POST", "/v1/orders/cancel", { accountId, orderHash: h, expiryNs, publicKey: signer.publicKey, signature, relayOnChain: opts.relayOnChain ?? false });
  }

  /**
   * Raise the nonce floor: every order with a smaller nonce is canceled here and on-chain.
   * Default floor is a fresh `nextNonce()`, so every order signed before this call dies and every
   * order signed after it (which gets a larger nonce) stays valid.
   */
  async setMinNonce(minNonce?: IntLike): Promise<{ minNonce: bigint; response: Record<string, unknown> }> {
    const { accountId, signer } = this.need();
    const info = await this.getInfo();
    const floor = minNonce === undefined ? nextNonce() : toBigInt(minNonce, "minNonce");
    const signature = await this.sign(minNonceMessage(info.chainId, info.clearingContract, accountId, floor));
    const response = await this.req("POST", "/v1/orders/min_nonce", { accountId, minNonce: floor.toString(), publicKey: signer.publicKey, signature });
    return { minNonce: floor, response };
  }

  /** Withdraw free collateral to the account itself. The key must allow withdrawals. */
  async withdraw(amount: IntLike, opts: { nonce?: IntLike } = {}): Promise<Record<string, unknown> & { nonce: string }> {
    const { accountId, signer } = this.need();
    const info = await this.getInfo();
    const amt = toBigInt(amount, "amount").toString();
    const nonce = (opts.nonce !== undefined ? toBigInt(opts.nonce, "nonce") : nextNonce()).toString();
    const signature = await this.sign(withdrawMessage(info.chainId, info.clearingContract, accountId, info.collateral, amt, nonce));
    const r = await this.req("POST", "/v1/withdraw", { accountId, amount: amt, nonce, publicKey: signer.publicKey, signature });
    return { ...r, nonce };
  }

  /** Move collateral into (`delta > 0`) or out of (`delta < 0`) an isolated position. */
  async adjustMargin(marketId: string, delta: IntLike, opts: { nonce?: IntLike } = {}): Promise<Record<string, unknown> & { nonce: string }> {
    const { accountId, signer } = this.need();
    const info = await this.getInfo();
    const d = toBigInt(delta, "delta").toString();
    const nonce = (opts.nonce !== undefined ? toBigInt(opts.nonce, "nonce") : nextNonce()).toString();
    const signature = await this.sign(marginMessage(info.chainId, info.clearingContract, accountId, marketId, d, nonce));
    const r = await this.req("POST", "/v1/margin", { accountId, marketId, delta: d, nonce, publicKey: signer.publicKey, signature });
    return { ...r, nonce };
  }

  /**
   * Deposit collateral. Returns the wallet transaction (`ft_transfer_call` on the collateral
   * token); if `send` is given (e.g. `wallet.signAndSendTransaction`), also submits it.
   */
  async deposit<R = unknown>(amount: IntLike, opts: { beneficiary?: string; send?: (tx: WalletTx) => Promise<R> } = {}): Promise<{ tx: WalletTx; result?: R }> {
    const info = await this.getInfo();
    const tx = clearingTx.deposit(info.clearingContract, info.collateral, amount, opts.beneficiary);
    return opts.send ? { tx, result: await opts.send(tx) } : { tx };
  }

  // ------------------------------------------------------------------ WebSocket

  /**
   * Keep a local L2 book from snapshot + deltas. A sequence gap (or a lagged notice) triggers a
   * re-subscribe and a fresh snapshot; `onBook` only fires on a consistent book.
   */
  subscribeBook(market: string, onBook: (book: ClearingBookState) => void, opts: SubscribeOptions = {}): BookSubscription {
    if (this.bookSubs.has(market)) throw new Error(`already subscribed to book ${market}`);
    let resolveReady!: () => void;
    const ready = new Promise<void>((r) => (resolveReady = r));
    const sub: BookSub = { market, state: new ClearingBookState(market), onBook, onError: opts.onError, resyncs: 0, resolveReady };
    this.bookSubs.set(market, sub);
    this.socket().then(
      (ws) => this.sendOn(ws, { op: "subscribe", channel: "book", market }),
      (e) => opts.onError?.(e),
    );
    return {
      book: sub.state,
      ready,
      get resyncs() {
        return sub.resyncs;
      },
      close: () => {
        if (this.bookSubs.get(market) !== sub) return;
        this.bookSubs.delete(market);
        if (this.ws?.readyState === 1) this.sendOn(this.ws, { op: "unsubscribe", channel: "book", market });
        this.maybeCloseSocket();
      },
    };
  }

  /** Authenticate the account channel and deliver its events (accepted, fill, canceled, rejected, position, free, fill_reverted, settled). */
  subscribeOrders(onEvent: (e: AccountEvent) => void, opts: SubscribeOptions = {}): Subscription {
    this.need();
    let resolveReady!: () => void;
    let rejectReady!: (e: unknown) => void;
    const ready = new Promise<void>((res, rej) => {
      resolveReady = res;
      rejectReady = rej;
    });
    ready.catch(() => {});
    const sub: AccountSub = { onEvent, onError: opts.onError, resolveReady, rejectReady };
    this.accountSubs.add(sub);
    if (this.authed) resolveReady();
    else
      this.socket().then(
        (ws) => this.sendAuth(ws),
        (e) => {
          rejectReady(e);
          opts.onError?.(e);
        },
      );
    return {
      ready,
      close: () => {
        this.accountSubs.delete(sub);
        this.maybeCloseSocket();
      },
    };
  }

  private sendOn(ws: WebSocket, v: unknown) {
    ws.send(JSON.stringify(v));
  }

  private async sendAuth(ws: WebSocket) {
    const { accountId, signer } = this.need();
    const info = await this.getInfo();
    const ts = Date.now();
    const signature = await this.sign(authMessage(info.chainId, info.clearingContract, accountId, ts));
    this.sendOn(ws, { op: "auth", account: accountId, ts, public_key: signer.publicKey, signature });
  }

  private socket(): Promise<WebSocket> {
    if (this.wsOpen) return this.wsOpen;
    if (!this.WS) return Promise.reject(new Error("no WebSocket implementation (pass opts.WebSocket)"));
    const ws = new this.WS(this.wsUrl);
    this.ws = ws;
    this.authed = false;
    this.wsOpen = new Promise((res, rej) => {
      ws.addEventListener("open", () => {
        this.reconnectDelay = 250;
        res(ws);
      }, { once: true });
      ws.addEventListener("error", (e) => rej(e), { once: true });
    });
    this.wsOpen.catch(() => {});
    ws.addEventListener("message", (e) => this.onMessage(ws, String((e as MessageEvent).data)));
    ws.addEventListener("close", () => this.onClose(ws));
    return this.wsOpen;
  }

  private maybeCloseSocket() {
    if (this.bookSubs.size === 0 && this.accountSubs.size === 0 && this.ws) {
      const ws = this.ws;
      this.ws = undefined;
      this.wsOpen = undefined;
      this.authed = false;
      ws.close();
    }
  }

  private onClose(ws: WebSocket) {
    if (this.ws !== ws) return; // closed on purpose
    this.ws = undefined;
    this.wsOpen = undefined;
    this.authed = false;
    for (const s of this.bookSubs.values()) s.state.synced = false;
    if (!this.reconnect || (this.bookSubs.size === 0 && this.accountSubs.size === 0)) return;
    const delay = this.reconnectDelay;
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, 10_000);
    setTimeout(() => {
      if (this.ws || (this.bookSubs.size === 0 && this.accountSubs.size === 0)) return;
      this.socket().then(
        (w) => {
          for (const s of this.bookSubs.values()) this.sendOn(w, { op: "subscribe", channel: "book", market: s.market });
          if (this.accountSubs.size > 0) this.sendAuth(w).catch((e) => this.emitError(e));
        },
        () => {}, // the failed socket's close event schedules the next attempt
      );
    }, delay);
  }

  private emitError(e: unknown) {
    for (const s of this.bookSubs.values()) s.onError?.(e);
    for (const s of this.accountSubs) s.onError?.(e);
  }

  private resync(ws: WebSocket, sub: BookSub) {
    sub.resyncs++;
    sub.state.synced = false;
    this.sendOn(ws, { op: "subscribe", channel: "book", market: sub.market });
  }

  private onMessage(ws: WebSocket, text: string) {
    let m: any;
    try {
      m = JSON.parse(text);
    } catch (e) {
      this.emitError(e);
      return;
    }
    if (m.op === "auth") {
      if (m.ok) {
        this.authed = true;
        for (const s of this.accountSubs) s.resolveReady();
      } else {
        const err = new Error(`auth failed: ${m.error ?? "unknown"}`);
        for (const s of this.accountSubs) {
          s.rejectReady(err);
          s.onError?.(err);
        }
      }
      return;
    }
    if (m.op) return; // subscribe / unsubscribe acknowledgements
    if (m.channel === "book") {
      // Deltas may omit `market`; with one book on the socket it is unambiguous.
      const sub = m.market ? this.bookSubs.get(m.market) : this.bookSubs.size === 1 ? [...this.bookSubs.values()][0] : undefined;
      if (!sub) return;
      let r;
      try {
        r = sub.state.apply(m as BookMessage);
      } catch (e) {
        sub.onError?.(e);
        this.resync(ws, sub);
        return;
      }
      if (r === "gap") this.resync(ws, sub);
      else if (r === "applied") {
        if (m.type === "snapshot") sub.resolveReady();
        sub.onBook(sub.state);
      }
    } else if (m.channel === "account") {
      for (const s of this.accountSubs) s.onEvent(m as AccountEvent);
    } else if (m.type === "lagged") {
      for (const s of this.bookSubs.values()) this.resync(ws, s);
      this.emitError(new Error(`feed lagged, ${m.dropped ?? "?"} messages dropped; books resynced`));
    } else if (m.error) {
      this.emitError(new Error(`${m.error}: ${m.message ?? ""}`));
    }
  }
}
