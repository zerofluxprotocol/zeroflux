import { describe, expect, it } from "vitest";
import { BookState, depositTx, registerKeyTx, forcedWithdrawalTx } from "../src/index.js";

describe("BookState", () => {
  it("applies deltas in order, ignores stale ones, detects gaps", () => {
    const b = new BookState();
    b.apply({ type: "snapshot", seq: 5, bids: [[100, 3]], asks: [[105, 2]] });
    expect(b.apply({ type: "delta", seq: 5, bids: [[100, 0]], asks: [] })).toBe("stale");
    expect(b.apply({ type: "delta", seq: 6, bids: [[100, 0], [99, 7], [98, 1]], asks: [[104, 1]] })).toBe("applied");
    expect(b.topBids()).toEqual([[99, 7], [98, 1]]);
    expect(b.topAsks()).toEqual([[104, 1], [105, 2]]);
    expect(b.apply({ type: "delta", seq: 8, bids: [], asks: [] })).toBe("gap");
  });
});

describe("wallet transactions", () => {
  it("have the wallet-selector shape and the contract's deposits", () => {
    const k = registerKeyTx("ob.near", "ed25519:abc");
    expect(k).toEqual({ receiverId: "ob.near", actions: [{ type: "FunctionCall", params: { methodName: "register_key", args: { public_key: "ed25519:abc" }, gas: "30000000000000", deposit: "10000000000000000000000" } }] });
    const d = depositTx("ob.near", "usdc.near", 5_000_000n);
    expect(d.receiverId).toBe("usdc.near");
    expect(d.actions[0].params).toMatchObject({ methodName: "ft_transfer_call", deposit: "1", args: { receiver_id: "ob.near", amount: "5000000", msg: "" } });
    expect(forcedWithdrawalTx("ob.near", "usdc.near", 1n).actions[0].params.deposit).toBe("10000000000000000000000");
  });
});
