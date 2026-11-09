// ZeroFlux explorer: blocks, difficulty, miners, supply, network activity.
// Talks to the zfx-node v1 JSON API; no framework, no build-time deps beyond tsc.

interface Status {
  chain_id: number; profile: string; genesis: string; height: number; best_hash: string; target: string;
  difficulty_log2: number; next_target: string; cum_work_log2: number; tip_timestamp: number; peer_count: number;
  mempool: number; orphans: number; supply: string; reorgs: number; max_reorg_depth: number; invalid_blocks: number;
  signature_schemes: string[]; crypto: string;
}
interface BlockRow { height: number; hash: string; timestamp: number; txs: number; miner: string; reward: number; difficulty_log2: number; nonce: string; }

const UNIT = 1e8;
const nodeInput = document.getElementById("node") as HTMLInputElement;
const app = document.getElementById("app") as HTMLElement;
nodeInput.value = localStorage.getItem("zfx-node") ?? (location.port ? location.origin : "http://127.0.0.1:9441");
nodeInput.addEventListener("change", () => { localStorage.setItem("zfx-node", nodeInput.value); route(); });

async function api<T>(path: string): Promise<T> {
  const r = await fetch(nodeInput.value.replace(/\/$/, "") + path);
  const j = await r.json();
  if (!r.ok) throw new Error(j?.error ?? r.statusText);
  return j as T;
}

const esc = (s: unknown) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]!));
const short = (h: string, n = 10) => (h.length > 2 * n ? `${h.slice(0, n)}..${h.slice(-6)}` : h);
const zfx = (u: number | string) => (Number(u) / UNIT).toLocaleString(undefined, { maximumFractionDigits: 8 });
const ago = (ts: number) => { const s = Math.max(0, Math.round(Date.now() / 1000 - ts)); return s < 120 ? `${s}s ago` : s < 7200 ? `${Math.round(s / 60)}m ago` : `${Math.round(s / 3600)}h ago`; };
const card = (k: string, v: string) => `<div class="card"><div class="k">${k}</div><div class="v">${v}</div></div>`;

function spark(values: number[]): string {
  if (values.length < 2) return "";
  const min = Math.min(...values), max = Math.max(...values), span = max - min || 1;
  const pts = values.map((v, i) => `${(i / (values.length - 1)) * 1000},${55 - ((v - min) / span) * 50}`).join(" ");
  return `<svg class="spark" viewBox="0 0 1000 60" preserveAspectRatio="none"><polyline points="${pts}"/></svg>`;
}

async function home() {
  const [s, blocks] = await Promise.all([api<Status>("/v1/status"), api<BlockRow[]>("/v1/blocks?count=60")]);
  const asc = [...blocks].reverse();
  const intervals = asc.slice(1).map((b, i) => b.timestamp - asc[i].timestamp);
  const avg = intervals.length ? intervals.reduce((a, b) => a + b, 0) / intervals.length : 0;
  const miners = new Map<string, number>();
  blocks.forEach((b) => miners.set(b.miner, (miners.get(b.miner) ?? 0) + 1));
  app.innerHTML = `
    <div class="cards">
      ${card("Height", s.height.toLocaleString())}
      ${card("Difficulty", `2<sup>${s.difficulty_log2.toFixed(2)}</sup>`)}
      ${card("Avg block time (60)", `${avg.toFixed(1)} s`)}
      ${card("Supply", `${zfx(s.supply)} ZFX`)}
      ${card("Peers", String(s.peer_count))}
      ${card("Mempool", String(s.mempool))}
      ${card("Reorgs (max depth)", `${s.reorgs} (${s.max_reorg_depth})`)}
      ${card("Invalid blocks seen", String(s.invalid_blocks))}
    </div>
    <p class="muted">PoW ${esc(s.profile)} · chain ${s.chain_id.toString(16)} · signatures ${esc(s.signature_schemes.join(", "))} · tip ${ago(s.tip_timestamp)}</p>
    <h2>Difficulty (log2), last ${asc.length} blocks</h2>${spark(asc.map((b) => b.difficulty_log2))}
    <h2>Miners, last ${blocks.length} blocks</h2>
    <table><tr><th>Miner</th><th>Blocks</th><th>Share</th></tr>
      ${[...miners.entries()].sort((a, b) => b[1] - a[1]).map(([m, n]) => `<tr><td class="mono"><a href="#/address/${m}">${short(m, 14)}</a></td><td>${n}</td><td>${((100 * n) / blocks.length).toFixed(1)}%</td></tr>`).join("")}
    </table>
    <h2>Latest blocks</h2>
    <table><tr><th>Height</th><th>Hash</th><th>Age</th><th>Txs</th><th>Miner</th><th>Reward</th><th>Difficulty</th></tr>
      ${blocks.slice(0, 25).map((b) => `<tr><td><a href="#/block/${b.height}">${b.height}</a></td><td class="mono"><a href="#/block/${b.hash}">${short(b.hash)}</a></td>
        <td>${ago(b.timestamp)}</td><td>${b.txs}</td><td class="mono">${short(b.miner, 8)}</td><td>${zfx(b.reward)}</td><td>2<sup>${b.difficulty_log2.toFixed(2)}</sup></td></tr>`).join("")}
    </table>`;
}

async function block(key: string) {
  const b = await api<any>(`/v1/block/${key}`);
  const h = b.header;
  app.innerHTML = `<h2>Block ${b.height}</h2><dl>
    <dt>Hash</dt><dd class="mono">${b.hash}</dd>
    <dt>Status</dt><dd>${esc(b.status)}${b.in_best_chain ? "" : " (side chain)"}</dd>
    <dt>Parent</dt><dd class="mono"><a href="#/block/${h.parent}">${h.parent}</a></dd>
    <dt>Timestamp</dt><dd>${new Date(h.timestamp * 1000).toISOString()} (${ago(h.timestamp)})</dd>
    <dt>Target</dt><dd class="mono">${h.target}</dd>
    <dt>PoW digest</dt><dd class="mono">${b.pow_digest}</dd>
    <dt>Nonce</dt><dd class="mono">${h.nonce}</dd>
    <dt>Tx root</dt><dd class="mono">${h.tx_root}</dd>
    <dt>Coinbase</dt><dd><a class="mono" href="#/address/${b.coinbase.to}">${b.coinbase.to}</a> ${zfx(b.coinbase.amount)} ZFX</dd>
    <dt>Size</dt><dd>${b.size.toLocaleString()} bytes</dd>
    <dt>Cumulative work</dt><dd>2<sup>${b.cum_work_log2.toFixed(2)}</sup></dd></dl>
    <p><a href="#/block/${b.height - 1}">previous</a> · <a href="#/block/${b.height + 1}">next</a></p>
    <h2>Transactions (${b.txs.length})</h2>
    <table><tr><th>Txid</th><th>From</th><th>To</th><th>Amount</th><th>Fee</th><th>Scheme</th><th>Bytes</th></tr>
    ${b.txs.map((t: any) => `<tr><td class="mono"><a href="#/tx/${t.txid}">${short(t.txid)}</a></td><td class="mono"><a href="#/address/${t.from}">${short(t.from, 8)}</a></td>
      <td class="mono"><a href="#/address/${t.to}">${short(t.to, 8)}</a></td><td>${zfx(t.amount)}</td><td>${zfx(t.fee)}</td><td>${esc(t.scheme)}</td><td>${t.bytes}</td></tr>`).join("")}</table>`;
}

async function address(a: string) {
  const r = await api<any>(`/v1/account/${a}`);
  app.innerHTML = `<h2>Address</h2><dl><dt>Address</dt><dd class="mono">${esc(r.address)}</dd>
    <dt>Balance</dt><dd>${zfx(r.balance)} ZFX</dd><dt>Nonce</dt><dd>${r.nonce} (next ${r.next_nonce})</dd>
    <dt>Pending</dt><dd>${r.pending.length}</dd></dl>
    <p class="muted">P2MR address: hash of a Merkle root of SLH-DSA spend leaves. Public keys stay hidden until a leaf is spent.</p>`;
}

async function tx(id: string) {
  const r = await api<any>(`/v1/tx/${id}`);
  const t = r.tx;
  app.innerHTML = `<h2>Transaction</h2><dl><dt>Txid</dt><dd class="mono">${esc(id)}</dd><dt>Status</dt><dd>${esc(r.status)}${r.confirmations ? `, ${r.confirmations} confirmations` : ""}</dd>
    ${r.block ? `<dt>Block</dt><dd class="mono"><a href="#/block/${r.block}">${r.height}</a></dd>` : ""}
    <dt>From</dt><dd class="mono"><a href="#/address/${t.from}">${t.from}</a></dd><dt>To</dt><dd class="mono"><a href="#/address/${t.to}">${t.to}</a></dd>
    <dt>Amount</dt><dd>${zfx(t.amount)} ZFX</dd><dt>Fee</dt><dd>${zfx(t.fee)} ZFX</dd><dt>Nonce</dt><dd>${t.nonce}</dd>
    <dt>Spend leaf scheme</dt><dd>${t.witness.leaf.scheme === 1 ? "SLH-DSA-SHA2-128s" : t.witness.leaf.scheme === 2 ? "SLH-DSA-SHA2-192s" : t.witness.leaf.scheme}</dd>
    <dt>P2MR path depth</dt><dd>${t.witness.path.length}</dd><dt>Signature</dt><dd>${t.witness.signature.length / 2} bytes</dd></dl>`;
}

async function route() {
  const [, kind, arg] = location.hash.split("/");
  try {
    if (kind === "block") await block(arg);
    else if (kind === "address") await address(arg);
    else if (kind === "tx") await tx(arg);
    else await home();
  } catch (e) {
    app.innerHTML = `<p class="err">${esc((e as Error).message)}</p><p class="muted">Is a zfx-node reachable at ${esc(nodeInput.value)}?</p>`;
  }
}

document.getElementById("search")!.addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const q = (document.getElementById("q") as HTMLInputElement).value.trim();
  if (/^\d+$/.test(q)) location.hash = `#/block/${q}`;
  else if (q.startsWith("zfx1")) location.hash = `#/address/${q}`;
  else if (/^[0-9a-f]{64}$/.test(q)) {
    try { await api(`/v1/block/${q}`); location.hash = `#/block/${q}`; } catch { location.hash = `#/tx/${q}`; }
  }
});
window.addEventListener("hashchange", route);
route();
setInterval(() => { if (!location.hash || location.hash === "#/") route(); }, 3000);
