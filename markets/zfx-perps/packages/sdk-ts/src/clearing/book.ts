import type { Book, BookLevel } from "./types.js";
import { toBigInt } from "./wire.js";

export type BookApplyResult = "applied" | "stale" | "gap";

/** A WebSocket book message: `type` "snapshot" or "delta", levels `[price, total qty]`, qty 0 removes. */
export interface BookMessage {
  type: string;
  seq: number | string;
  market?: string;
  bids?: Array<[string | number | bigint, string | number | bigint]>;
  asks?: Array<[string | number | bigint, string | number | bigint]>;
}

/** Local L2 book (bigint ticks and lots) built from a snapshot plus sequenced deltas. */
export class ClearingBookState {
  seq = -1;
  /** False until the first snapshot, and again after a gap until the next snapshot. */
  synced = false;
  bids = new Map<bigint, bigint>();
  asks = new Map<bigint, bigint>();

  constructor(readonly market: string) {}

  /** On "gap" the book is marked unsynced; resubscribe to get a fresh snapshot. */
  apply(m: BookMessage): BookApplyResult {
    const seq = Number(m.seq);
    if (m.type === "snapshot") {
      this.seq = seq;
      this.bids = new Map((m.bids ?? []).map(([p, q]) => [toBigInt(p as string, "price"), toBigInt(q as string, "quantity")]));
      this.asks = new Map((m.asks ?? []).map(([p, q]) => [toBigInt(p as string, "price"), toBigInt(q as string, "quantity")]));
      this.synced = true;
      return "applied";
    }
    if (!this.synced) return "stale";
    if (seq <= this.seq) return "stale";
    if (seq !== this.seq + 1) {
      this.synced = false;
      return "gap";
    }
    for (const [side, lv] of [
      [this.bids, m.bids ?? []],
      [this.asks, m.asks ?? []],
    ] as const) {
      for (const [p, q] of lv) {
        const price = toBigInt(p as string, "price");
        const qty = toBigInt(q as string, "quantity");
        if (qty === 0n) side.delete(price);
        else side.set(price, qty);
      }
    }
    this.seq = seq;
    return "applied";
  }

  /** Bids best (highest) first. */
  topBids(n = 20): BookLevel[] {
    return [...this.bids.entries()].sort((a, b) => (a[0] < b[0] ? 1 : a[0] > b[0] ? -1 : 0)).slice(0, n);
  }

  /** Asks best (lowest) first. */
  topAsks(n = 20): BookLevel[] {
    return [...this.asks.entries()].sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)).slice(0, n);
  }

  bestBid(): bigint | undefined {
    return this.topBids(1)[0]?.[0];
  }

  bestAsk(): bigint | undefined {
    return this.topAsks(1)[0]?.[0];
  }

  snapshot(depth = 50): Book {
    return { market: this.market, seq: this.seq, bids: this.topBids(depth), asks: this.topAsks(depth) };
  }
}
