import type { AccountEvent, Level, PerpEvent, Trade } from "./types.js";
import type { ObkitClient } from "./client.js";

/** Local L2 book from a snapshot plus sequenced deltas. */
export class BookState {
  seq = 0;
  bids = new Map<number, number>();
  asks = new Map<number, number>();

  /** Returns "applied" | "stale" | "gap". On "gap", resubscribe to get a fresh snapshot. */
  apply(m: { type: string; seq: number; bids: Level[]; asks: Level[] }): "applied" | "stale" | "gap" {
    if (m.type === "snapshot") {
      this.seq = m.seq;
      this.bids = new Map(m.bids);
      this.asks = new Map(m.asks);
      return "applied";
    }
    if (m.seq <= this.seq) return "stale";
    if (m.seq !== this.seq + 1) return "gap";
    for (const [side, levels] of [
      [this.bids, m.bids],
      [this.asks, m.asks],
    ] as const) {
      for (const [p, q] of levels) {
        if (q === 0) side.delete(p);
        else side.set(p, q);
      }
    }
    this.seq = m.seq;
    return "applied";
  }

  /** Bids best first. */
  topBids(n = 20): Level[] {
    return [...this.bids.entries()].sort((a, b) => b[0] - a[0]).slice(0, n);
  }

  /** Asks best first. */
  topAsks(n = 20): Level[] {
    return [...this.asks.entries()].sort((a, b) => a[0] - b[0]).slice(0, n);
  }
}

export interface FeedHandlers {
  onBook?: (market: string, book: BookState) => void;
  onTrade?: (market: string, trade: Trade) => void;
  onAccount?: (event: AccountEvent) => void;
  onPerp?: (event: PerpEvent) => void;
  onError?: (err: unknown) => void;
}

/**
 * One WebSocket connection: book feeds (gap-checked, auto-resync), trades, and the private account
 * stream. Uses the global `WebSocket` (browsers, Node 22+), or pass a constructor.
 */
export class ObkitFeed {
  readonly books = new Map<string, BookState>();
  resyncs = 0;
  private ws: WebSocket;
  private ready: Promise<void>;

  constructor(
    baseUrl: string,
    private readonly h: FeedHandlers = {},
    WS: typeof WebSocket = globalThis.WebSocket,
  ) {
    this.ws = new WS(`${baseUrl.replace(/\/$/, "").replace(/^http/, "ws")}/v1/ws`);
    this.ready = new Promise((res, rej) => {
      this.ws.addEventListener("open", () => res(), { once: true });
      this.ws.addEventListener("error", (e) => rej(e), { once: true });
    });
    this.ws.addEventListener("message", (e) => this.onMessage(String((e as MessageEvent).data)));
  }

  private send(v: unknown) {
    this.ws.send(JSON.stringify(v));
  }

  async subscribeBook(market: string) {
    await this.ready;
    this.books.set(market, this.books.get(market) ?? new BookState());
    this.send({ op: "subscribe", channel: "book", market });
  }

  /** Mark, funding and socialized-loss updates of a perp market. */
  async subscribePerp(market: string) {
    await this.ready;
    this.send({ op: "subscribe", channel: "perp", market });
  }

  async subscribeTrades(market: string) {
    await this.ready;
    this.send({ op: "subscribe", channel: "trades", market });
  }

  /** Authenticate the private account channel. Resolves when the server confirms. */
  async authenticate(client: ObkitClient) {
    await this.ready;
    const ok = new Promise<void>((res, rej) => {
      const f = (e: Event) => {
        const m = JSON.parse(String((e as MessageEvent).data));
        if (m.op === "auth") {
          this.ws.removeEventListener("message", f);
          m.ok ? res() : rej(new Error(`auth failed: ${m.error}`));
        }
      };
      this.ws.addEventListener("message", f);
    });
    this.send(await client.wsAuth());
    return ok;
  }

  /** Resolve once the market's local book has reached at least `seq`. */
  async waitForSeq(market: string, seq: number, timeoutMs = 10_000) {
    const t0 = Date.now();
    while ((this.books.get(market)?.seq ?? 0) < seq) {
      if (Date.now() - t0 > timeoutMs) throw new Error(`book ${market} did not reach seq ${seq}`);
      await new Promise((r) => setTimeout(r, 10));
    }
  }

  close() {
    this.ws.close();
  }

  private onMessage(text: string) {
    let m: any;
    try {
      m = JSON.parse(text);
    } catch (e) {
      this.h.onError?.(e);
      return;
    }
    // Subscription and auth acknowledgements carry `op`; they are not data.
    if (m.op) return;
    if (m.channel === "book") {
      const b = this.books.get(m.market);
      if (!b) return;
      const r = b.apply(m);
      if (r === "gap") {
        this.resyncs++;
        this.send({ op: "subscribe", channel: "book", market: m.market });
      } else if (r === "applied") this.h.onBook?.(m.market, b);
    } else if (m.channel === "trades") this.h.onTrade?.(m.market, m.trade);
    else if (m.channel === "account") this.h.onAccount?.(m as AccountEvent);
    else if (m.channel === "perp") this.h.onPerp?.(m as PerpEvent);
    else if (m.type === "lagged") this.h.onError?.(new Error(`feed lagged, ${m.dropped} messages dropped; books resynced`));
    else if (m.error) this.h.onError?.(new Error(`${m.error}: ${m.message ?? ""}`));
  }
}
