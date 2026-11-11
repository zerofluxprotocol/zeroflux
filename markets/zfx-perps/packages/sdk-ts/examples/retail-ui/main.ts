/**
 * Minimal retail page: markets, oracle provenance with a fail-closed trading switch, an order
 * ticket with a local margin estimate, and positions as pending (node) vs settled (on-chain).
 *
 * Query parameters: `?node=http://127.0.0.1:8796` (operator node) and optionally
 * `&rpc=https://rpc.testnet.near.org` to read the oracle reporter allowlist from the clearing
 * contract instead of trusting the node's copy.
 */
import { KeySigner } from "../../src/signer.js";
import {
  ClearingReader,
  ZfxPerpsClient,
  checkOracle,
  clearingTx,
  estimateMargin,
  type Market,
  type OracleCheck,
  type Portfolio,
  type Side,
} from "../../src/clearing/index.js";

const q = new URLSearchParams(location.search);
const NODE = q.get("node") ?? "http://127.0.0.1:8796";
const RPC = q.get("rpc");

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
function el(tag: string, text?: string, cls?: string): HTMLElement {
  const e = document.createElement(tag);
  if (text !== undefined) e.textContent = text;
  if (cls) e.className = cls;
  return e;
}
function row(cells: Array<string | HTMLElement>): HTMLTableRowElement {
  const tr = document.createElement("tr");
  for (const c of cells) {
    const td = document.createElement("td");
    if (typeof c === "string") td.textContent = c;
    else td.append(c);
    tr.append(td);
  }
  return tr;
}
const status = (msg: string, bad = false) => {
  const s = $("status");
  s.textContent = msg;
  s.className = bad ? "bad" : "";
};

// The trading key lives in this browser only. Register its public key once with your wallet.
const stored = localStorage.getItem("zfx-trading-key");
const signer = stored ? KeySigner.fromNearSecret(stored) : KeySigner.generate();
localStorage.setItem("zfx-trading-key", signer.nearSecret);
$("pubkey").textContent = signer.publicKey;

const accountInput = $<HTMLInputElement>("account");
accountInput.value = localStorage.getItem("zfx-account") ?? "";
let client = makeClient();
function makeClient() {
  const accountId = accountInput.value.trim() || undefined;
  return new ZfxPerpsClient(NODE, accountId ? { accountId, signer } : {});
}
accountInput.addEventListener("change", () => {
  localStorage.setItem("zfx-account", accountInput.value.trim());
  client = makeClient();
  resubscribe();
  void refresh();
});

$("register").addEventListener("click", async () => {
  const info = await client.getInfo();
  const tx = clearingTx.registerKey(info.clearingContract, signer.publicKey, { canWithdraw: false });
  const pre = $("tx");
  pre.hidden = false;
  pre.textContent = `// Sign with your NEAR wallet, e.g. wallet.signAndSendTransaction(tx)\n${JSON.stringify(tx, null, 2)}`;
});

let markets: Market[] = [];
const checks = new Map<string, OracleCheck>();
let portfolio: Portfolio | null = null;
let trust: { reporters?: string[]; quorum?: number } = {};
let trustSource = "node (not checked against chain)";

async function loadTrust() {
  if (!RPC) return;
  const info = await client.getInfo();
  const st = await new ClearingReader(RPC, info.clearingContract).state();
  trust = { reporters: st.reporters, quorum: st.quorum };
  trustSource = `clearing contract ${info.clearingContract}`;
}

function renderMarkets() {
  const body = $("markets").querySelector("tbody")!;
  body.replaceChildren(
    ...markets.map((m) =>
      row([
        m.marketId,
        el("span", m.effectiveStatus, m.effectiveStatus === "ACTIVE" ? "ok" : "bad"),
        m.mark.toString(),
        m.openInterest.toString(),
        `${m.params.initialMarginBps / 100}% / ${m.params.maintenanceMarginBps / 100}%`,
        `${m.params.makerFeeBps} / ${m.params.takerFeeBps} bps`,
      ]),
    ),
  );
  const sel = $<HTMLSelectElement>("market");
  const cur = sel.value;
  sel.replaceChildren(...markets.map((m) => Object.assign(document.createElement("option"), { value: m.marketId, textContent: m.marketId })));
  if (cur) sel.value = cur;
}

async function loadOracle(m: Market) {
  const view = await client.getOracle(m.marketId);
  const check = checkOracle(view, { ...trust, market: m });
  checks.set(m.marketId, check);
  return { view, check };
}

async function renderOracle() {
  const box = $("oracle");
  box.replaceChildren();
  for (const m of markets) {
    let line: HTMLElement;
    try {
      const { view, check } = await loadOracle(m);
      const p = check.print;
      line = el("div");
      line.append(el("strong", `${m.marketId}: `), el("span", check.ok ? "verified" : `NOT VERIFIED (${check.problems.join(", ")}), trading disabled`, check.ok ? "ok" : "bad"));
      const details = el("pre");
      details.textContent = [
        `index      ${view.indexId} ${view.indexVersion}`,
        `print      ${view.print?.hash ?? "none"}`,
        p ? `value      ${p.value} (scale ${p.valueScale}) ${p.status} / ${p.dataStatus} / ${p.finality} rev ${p.revision}` : "",
        p ? `window     ${new Date(Number(p.sourceStartMs)).toISOString()} .. ${new Date(Number(p.sourceEndMs)).toISOString()}` : "",
        p ? `published  ${new Date(Number(p.publishedAtMs)).toISOString()}, valid until ${new Date(Number(p.validUntilMs)).toISOString()}` : "",
        p ? `source     ${p.sourceHash}\nformula    ${p.formulaHash}` : "",
        `signers    ${check.verification?.signers.length ?? 0} valid of quorum ${trust.quorum ?? view.quorum}; allowlist from ${trustSource}`,
        ...(check.verification?.rejected ?? []).map((r) => `rejected   ${r.publicKey} ${r.reason}`),
      ]
        .filter(Boolean)
        .join("\n");
      line.append(details);
    } catch (e) {
      checks.delete(m.marketId);
      line = el("div", `${m.marketId}: oracle unavailable (${String(e)}), trading disabled`, "bad");
    }
    box.append(line);
  }
}

function renderPositions() {
  const body = $("positions").querySelector("tbody")!;
  if (!portfolio) {
    body.replaceChildren(row(["enter an account with a registered key", "", "", "", "", "", "", ""]));
    $("free").textContent = "";
    return;
  }
  const rows: HTMLTableRowElement[] = [];
  const ids = new Set([...portfolio.pending.positions, ...portfolio.settled.positions].map((p) => p.market));
  for (const id of ids) {
    for (const [label, side] of [
      ["pending", portfolio.pending],
      ["settled", portfolio.settled],
    ] as const) {
      const p = side.positions.find((x) => x.market === id);
      rows.push(row([id, label, p?.size.toString() ?? "0", p?.margin.toString() ?? "0", p?.equity.toString() ?? "0", p?.maintenanceMargin.toString() ?? "0", p?.liquidationPrice?.toString() ?? "-", p?.fundingOwed.toString() ?? "0"]));
    }
  }
  body.replaceChildren(...rows);
  $("free").textContent = `Free collateral: pending ${portfolio.pending.free}, settled ${portfolio.settled.free} (last settled batch ${portfolio.lastSettledBatch}). Pending includes fills not yet settled on NEAR.`;
}

function ticket(): { market?: Market; side: Side; price: bigint; qty: bigint; postOnly: boolean; reduceOnly: boolean } | null {
  try {
    const market = markets.find((m) => m.marketId === $<HTMLSelectElement>("market").value);
    return {
      market,
      side: $<HTMLSelectElement>("side").value as Side,
      price: BigInt($<HTMLInputElement>("price").value || "0"),
      qty: BigInt($<HTMLInputElement>("qty").value || "0"),
      postOnly: $<HTMLInputElement>("postOnly").checked,
      reduceOnly: $<HTMLInputElement>("reduceOnly").checked,
    };
  } catch {
    return null;
  }
}

/** Trading is allowed only on a verified oracle and an ACTIVE market (CLOSE_ONLY: reduce-only orders). */
function tradeBlock(): string | null {
  const t = ticket();
  if (!t || !t.market) return "enter a market, an integer price and quantity";
  if (!client.accountId) return "enter your account";
  const check = checks.get(t.market.marketId);
  if (!check?.ok) return `oracle not verified (${check?.problems.join(", ") ?? "unavailable"})`;
  const st = t.market.effectiveStatus;
  if (st === "CLOSE_ONLY" && !t.reduceOnly) return "market is CLOSE_ONLY: only reduce-only orders";
  if (st !== "ACTIVE" && st !== "CLOSE_ONLY") return `market is ${st}`;
  return null;
}

function renderEstimate() {
  const out = $("estimate");
  const t = ticket();
  const block = tradeBlock();
  $<HTMLButtonElement>("submit").disabled = block !== null;
  if (!t?.market || t.price <= 0n || t.qty <= 0n) {
    out.replaceChildren(el("span", block ?? "", "bad"));
    return;
  }
  const pos = portfolio?.pending.positions.find((p) => p.market === t.market!.marketId);
  const e = estimateMargin(t.market, pos, t.side, t.price, t.qty, { postOnly: t.postOnly, reduceOnly: t.reduceOnly, free: portfolio?.pending.free });
  const pre = el("pre");
  pre.textContent = [
    `notional        ${e.notional}`,
    `fee             ${e.fee} (${e.feeBps} bps, rounded up)`,
    `initial margin  ${e.initialMargin} for size ${e.sizeAfter}`,
    `loss vs mark    ${e.adverseLoss}`,
    `needs free      ${e.requiredFree}${portfolio ? ` of ${portfolio.pending.free}` : ""}`,
    `liq. price      ${e.liquidationPrice ?? "-"}`,
    e.violations.length ? `problems        ${e.violations.join(", ")}` : "",
  ]
    .filter(Boolean)
    .join("\n");
  out.replaceChildren(pre, ...(block ? [el("span", block, "bad")] : []));
}

for (const id of ["market", "side", "price", "qty", "postOnly", "reduceOnly"]) {
  $(id).addEventListener("input", renderEstimate);
  $(id).addEventListener("change", renderEstimate);
}

$("ticket").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const t = ticket();
  const block = tradeBlock();
  if (block || !t?.market) return status(block ?? "invalid ticket", true);
  try {
    const ack = await client.placeOrder({ marketId: t.market.marketId, side: t.side, price: t.price, quantity: t.qty, postOnly: t.postOnly, reduceOnly: t.reduceOnly });
    status(`order ${ack.orderHash.slice(0, 16)} ${ack.status}, filled ${ack.filled}, remaining ${ack.remaining}`);
    await refresh();
  } catch (e) {
    status(String(e), true);
  }
});

async function refresh() {
  try {
    markets = await client.listMarkets();
    renderMarkets();
    await renderOracle();
    portfolio = client.accountId ? await client.getPortfolio().catch(() => null) : null;
    renderPositions();
    renderEstimate();
    status(`connected to ${NODE}`);
  } catch (e) {
    status(`node ${NODE}: ${String(e)}. Trading disabled.`, true);
    $<HTMLButtonElement>("submit").disabled = true;
  }
}

let ordersSub: { close(): void } | null = null;
/** Account events (fills, reverts, settlements) trigger a refresh of positions and estimates. */
function resubscribe() {
  ordersSub?.close();
  ordersSub = null;
  if (!client.accountId) return;
  const sub = client.subscribeOrders(
    (e) => {
      if (["fill", "fill_reverted", "settled", "position", "free"].includes(e.type)) void refresh();
    },
    { onError: (e) => status(`account stream: ${String(e)}`, true) },
  );
  sub.ready.catch((e) => status(`account stream: ${String(e)}`, true));
  ordersSub = sub;
}

async function start() {
  await loadTrust().catch((e) => status(`could not read the contract allowlist: ${e}`, true));
  await refresh();
  resubscribe();
  setInterval(refresh, 10_000);
}
void start();
