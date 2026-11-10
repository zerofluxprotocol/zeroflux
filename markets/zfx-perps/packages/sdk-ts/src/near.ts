/**
 * NEAR-side helpers for apps. These build transactions for the user's wallet to sign (no keys
 * here) and read the settlement contract over JSON-RPC.
 *
 * Transactions use the `@near-wallet-selector` shape:
 *   const tx = registerKeyTx(contract, signer.publicKey);
 *   await wallet.signAndSendTransaction(tx);
 * For other wallet libraries, `tx.actions[i].params` has methodName/args/gas/deposit.
 */

const TGAS = 10n ** 12n;
/** 0.01 NEAR, the contract's storage deposit for keys and forced-withdrawal requests. */
export const KEY_DEPOSIT = (10n ** 22n).toString();
export const FORCE_DEPOSIT = KEY_DEPOSIT;

export interface FunctionCallAction {
  type: "FunctionCall";
  params: { methodName: string; args: Record<string, unknown>; gas: string; deposit: string };
}

export interface WalletTx {
  receiverId: string;
  actions: FunctionCallAction[];
}

const call = (receiverId: string, methodName: string, args: Record<string, unknown>, gasTgas: number, deposit = "0"): WalletTx => ({
  receiverId,
  actions: [{ type: "FunctionCall", params: { methodName, args, gas: (BigInt(gasTgas) * TGAS).toString(), deposit } }],
});

/** Register a trading key (`ed25519:<base58>`) for the signing account. Costs 0.01 NEAR, refunded on removal. */
export const registerKeyTx = (contract: string, publicKey: string) => call(contract, "register_key", { public_key: publicKey }, 30, KEY_DEPOSIT);

export const removeKeyTx = (contract: string, publicKey: string) => call(contract, "remove_key", { public_key: publicKey }, 30);

/**
 * Deposit a NEP-141 token. The signing account must already hold the token. Credits the signer,
 * or `beneficiary` if given. Unlisted tokens are refunded by the contract.
 */
export const depositTx = (contract: string, token: string, amount: bigint | string, beneficiary?: string) =>
  call(token, "ft_transfer_call", { receiver_id: contract, amount: BigInt(amount).toString(), msg: beneficiary ?? "" }, 100, "1");

/** Register an account with a token contract so it can receive withdrawals (NEP-145). */
export const tokenStorageDepositTx = (token: string, account: string, deposit = (125n * 10n ** 19n).toString()) =>
  call(token, "storage_deposit", { account_id: account, registration_only: true }, 30, deposit);

/** File a forced withdrawal on-chain. If the operator ignores it past the timeout, anyone can freeze. */
export const forcedWithdrawalTx = (contract: string, token: string, amount: bigint | string) =>
  call(contract, "request_forced_withdrawal", { token, amount: BigInt(amount).toString() }, 30, FORCE_DEPOSIT);

export const freezeTx = (contract: string) => call(contract, "freeze", {}, 30);

/** After a freeze: withdraw the signer's whole settled balance of `token`. */
export const escapeWithdrawTx = (contract: string, token: string) => call(contract, "escape_withdraw", { token }, 100);

// ---------- views ----------

async function view<T>(rpcUrl: string, contract: string, method: string, args: Record<string, unknown>, fetchImpl: typeof fetch = fetch): Promise<T> {
  const argsB64 = btoa(JSON.stringify(args));
  const r = await fetchImpl(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: "zfx-perps", method: "query", params: { request_type: "call_function", finality: "final", account_id: contract, method_name: method, args_base64: argsB64 } }),
  });
  const v: any = await r.json();
  if (v.error || v.result?.error) throw new Error(`view ${method}: ${JSON.stringify(v.error ?? v.result.error)}`);
  return JSON.parse(new TextDecoder().decode(new Uint8Array(v.result.result)));
}

export interface ContractState {
  owner: string;
  operator: string;
  tokens: string[];
  last_batch_id: string;
  last_journal_seq: string;
  last_journal_hash: string;
  first_event: string;
  next_event: string;
  next_forced: string;
  forced_cursor: string;
  force_timeout_ns: string;
  frozen: boolean;
}

export class SettlementReader {
  constructor(
    readonly rpcUrl: string,
    readonly contract: string,
    private readonly fetchImpl: typeof fetch = globalThis.fetch.bind(globalThis),
  ) {}

  state() {
    return view<ContractState>(this.rpcUrl, this.contract, "get_state", {}, this.fetchImpl);
  }
  /** Settled (on-chain) balance; the engine's balance includes trades not yet settled. */
  async balance(account: string, token: string): Promise<bigint> {
    return BigInt(await view<string>(this.rpcUrl, this.contract, "get_balance", { account, token }, this.fetchImpl));
  }
  async balances(account: string): Promise<Array<[string, bigint]>> {
    const v = await view<Array<[string, string]>>(this.rpcUrl, this.contract, "get_balances", { account }, this.fetchImpl);
    return v.map(([t, b]) => [t, BigInt(b)]);
  }
  keys(account: string) {
    return view<string[]>(this.rpcUrl, this.contract, "get_keys", { account }, this.fetchImpl);
  }
  async withdrawNonce(account: string): Promise<number> {
    return Number(await view<string>(this.rpcUrl, this.contract, "get_withdraw_nonce", { account }, this.fetchImpl));
  }
}
