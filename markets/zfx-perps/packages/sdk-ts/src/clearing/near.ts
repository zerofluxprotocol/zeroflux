/**
 * Wallet transactions and views for the NEAR clearing contract (`contracts/zfx-perps-clearing`).
 *
 * Transactions use the `@near-wallet-selector` shape, as in `src/near.ts`:
 *   await wallet.signAndSendTransaction(clearingTx.registerKey(contract, signer.publicKey));
 * No keys live here: the user's wallet signs.
 */
import type { FunctionCallAction, WalletTx } from "../near.js";
import { toBigInt } from "./wire.js";

export type { FunctionCallAction, WalletTx };

const TGAS = 10n ** 12n;
/** 0.01 NEAR: storage deposit for a trading key (refunded on `remove_key`) and for a forced withdrawal request. */
export const CLEARING_KEY_DEPOSIT = (10n ** 22n).toString();
export const CLEARING_FORCE_DEPOSIT = CLEARING_KEY_DEPOSIT;

type IntLike = bigint | number | string;
const s = (v: IntLike, what: string) => {
  const b = toBigInt(v, what);
  if (b < 0n) throw new Error(`${what} must be non-negative`);
  return b.toString();
};

function call(receiverId: string, methodName: string, args: Record<string, unknown>, gasTgas: number, deposit = "0"): WalletTx {
  return { receiverId, actions: [{ type: "FunctionCall", params: { methodName, args, gas: (BigInt(gasTgas) * TGAS).toString(), deposit } }] };
}

/** Builders for every user-facing clearing contract call. */
export const clearingTx = {
  /** Register an `ed25519:<base58>` trading key for the signing account. `expiresNs` 0 or omitted = no expiry; `canWithdraw: false` makes a trading-only key. */
  registerKey(contract: string, publicKey: string, opts: { expiresNs?: IntLike; canWithdraw?: boolean } = {}): WalletTx {
    const args: Record<string, unknown> = { public_key: publicKey };
    if (opts.expiresNs !== undefined) args.expires_ns = s(opts.expiresNs, "expiresNs");
    if (opts.canWithdraw !== undefined) args.can_withdraw = opts.canWithdraw;
    return call(contract, "register_key", args, 30, CLEARING_KEY_DEPOSIT);
  },
  /** Revoke a key; refunds the 0.01 NEAR deposit. */
  removeKey: (contract: string, publicKey: string) => call(contract, "remove_key", { public_key: publicKey }, 30),
  /** Cancel one order on-chain from the wallet. Final for every relayer. */
  cancelOrder: (contract: string, orderHash: string, expiryNs: IntLike) => call(contract, "cancel_order", { order_hash: orderHash, expiry_ns: s(expiryNs, "expiryNs") }, 30),
  /** Raise the nonce floor on-chain from the wallet. */
  setMinNonce: (contract: string, minNonce: IntLike) => call(contract, "set_min_nonce", { min_nonce: s(minNonce, "minNonce") }, 30),
  /** File a forced withdrawal of free collateral. If the operator ignores it past the timeout, anyone can `freeze`. */
  requestForcedWithdrawal: (contract: string, amount: IntLike) => call(contract, "request_forced_withdrawal", { amount: s(amount, "amount") }, 30, CLEARING_FORCE_DEPOSIT),
  freeze: (contract: string) => call(contract, "freeze", {}, 30),
  /** After a freeze: value up to `limit` positions per call until the scan reports done. */
  escapeScan: (contract: string, limit = 100) => call(contract, "escape_scan", { limit }, 300),
  /** After the scan: withdraw free collateral plus position claims. */
  escapeWithdraw: (contract: string) => call(contract, "escape_withdraw", {}, 100),
  /** Take back a deposit the operator never credited. */
  reclaimDeposit: (contract: string, depositId: IntLike) => call(contract, "reclaim_deposit", { deposit_id: s(depositId, "depositId") }, 100),
  /**
   * Deposit collateral: `ft_transfer_call` on the token with the clearing contract as receiver.
   * Credits the signer, or `beneficiary` if given. Pending until the operator's next batch.
   */
  deposit: (contract: string, token: string, amount: IntLike, beneficiary?: string) =>
    call(token, "ft_transfer_call", { receiver_id: contract, amount: s(amount, "amount"), msg: beneficiary ?? "" }, 100, "1"),
};

/** `call_function` view over NEAR JSON-RPC. */
export async function viewCall<T>(rpcUrl: string, contract: string, method: string, args: Record<string, unknown>, fetchImpl: typeof fetch = globalThis.fetch.bind(globalThis)): Promise<T> {
  const r = await fetchImpl(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: "zfx-perps",
      method: "query",
      params: { request_type: "call_function", finality: "final", account_id: contract, method_name: method, args_base64: btoa(JSON.stringify(args)) },
    }),
  });
  const v: any = await r.json();
  if (v.error || v.result?.error) throw new Error(`view ${method}: ${JSON.stringify(v.error ?? v.result.error)}`);
  return JSON.parse(new TextDecoder().decode(new Uint8Array(v.result.result)));
}

export interface ClearingState {
  owner: string;
  operators: string[];
  chain_id: string;
  contract_id: string;
  collateral: string;
  insurance_account: string;
  fee_balance: string;
  total_collateral: string;
  /** `ed25519:<base58>` oracle reporters. Use these, not the node's list, in `checkOracle`. */
  reporters: string[];
  quorum: number;
  last_op_ns: string;
  settle_grace_ns: string;
  max_order_ttl_ns: string;
  markets: string[];
  last_batch_id: string;
  frozen: boolean;
  force_timeout_ns: string;
  escape: { cursor: number; done: boolean; claims: string; deficits: string; shortfall: string };
  [k: string]: unknown;
}

export interface ClearingAccountView {
  account: string;
  free: string;
  min_nonce: string;
  action_nonce: string;
  positions: Array<{ market: string; size: string; margin: string; funding_index: string; funding_owed: string; equity: string; mark: string; initial_margin: string; maintenance_margin: string; liquidation_price: string | null }>;
}

/** Read-only views of the clearing contract (the settled, on-chain truth). */
export class ClearingReader {
  constructor(
    readonly rpcUrl: string,
    readonly contract: string,
    private readonly fetchImpl: typeof fetch = globalThis.fetch.bind(globalThis),
  ) {}

  private v<T>(method: string, args: Record<string, unknown> = {}) {
    return viewCall<T>(this.rpcUrl, this.contract, method, args, this.fetchImpl);
  }
  state() {
    return this.v<ClearingState>("get_state");
  }
  /** Raw market JSON (parse with `parseMarket`). */
  market(market: string) {
    return this.v<Record<string, unknown> | null>("get_market", { market });
  }
  markets() {
    return this.v<Record<string, unknown>[]>("get_markets");
  }
  account(account: string) {
    return this.v<ClearingAccountView>("get_account", { account });
  }
  order(orderHash: string) {
    return this.v<{ account: string; filled: string; canceled: boolean; expiry_ns: string } | null>("get_order", { order_hash: orderHash });
  }
  /** Events with seq in `[from, from + limit)` (limit capped at 200 by the contract). */
  events(from: IntLike, limit = 100) {
    return this.v<Array<[string, Record<string, unknown>]>>("get_events", { from: s(from, "from"), limit });
  }
  keys(account: string) {
    return this.v<Array<{ public_key: string; expires_ns: string; can_withdraw: boolean }>>("get_keys", { account });
  }
}
