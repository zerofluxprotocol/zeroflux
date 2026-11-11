/* Venue adapter for the ZFX Markets trade app: talks to a zfx-perps node (REST) with the bundled
 * @zeroflux/zfx-perps-sdk. Orders, cancels and account reads are signed in the browser with the
 * trader's testnet trading key; the key never leaves this page except as signatures.
 *
 * Exposes window.ZfxVenue.connect(url) -> adapter, and resolves window.ZfxVenueReady.
 * Prices are ticks (1 tick = $1 of index per day on these markets), sizes are lots, money is
 * tUSDC atoms (6 decimals). Display uses Number(); orders use the SDK's bigint fields.
 */
import { ZfxPerpsClient, KeySigner, clearing, signTsMs } from './zfx-perps-sdk.js';

const TRADING_KEY = 'zfx-trading-key';
const esc = (s) => String(s == null ? '' : s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const n = (v) => (v == null || v === '' ? null : Number(v));
const atoms = (v, dec) => (v == null ? null : Number(v) / 10 ** dec);
const usd = (v) => (v == null || isNaN(v) ? 'n/a' : (v < 0 ? '−$' : '$') + Math.abs(v).toLocaleString('en-US', { maximumFractionDigits: Math.abs(v) >= 100 ? 0 : 2 }));
const tdate = (ns) => { const d = new Date(Number(BigInt(ns) / 1000000n)); return d.toISOString().slice(5, 16).replace('T', ' '); };

async function connect(url) {
  const base = url.replace(/\/+$/, '');
  const pub = new ZfxPerpsClient(base, { reconnect: false });
  const info = await pub.getInfo();
  const raw = async (p) => { const r = await fetch(base + p); if (!r.ok) throw new Error(p + ': HTTP ' + r.status); return r.json(); };
  let first = await raw('/v1/markets');
  const dec = info.collateralDecimals ?? 6;

  const st = { label: info.clearingContract, marks: {}, account: null, positions: [], orders: [], fills: [], free: null, venue: {}, book: {}, error: null };
  const listeners = [];
  const emit = () => listeners.forEach((f) => f(st));
  let client = null, selected = null, acctNow = null;

  function boardId(permaslug) { return 'ZFX-OR-' + permaslug; }
  function applyMarkets(d) {
    st.venue = {};
    st.marks = {};
    for (const m of d.markets) {
      if (!m.permaslug || !(m.params && String(m.params.index_id || '').startsWith('or-model-spend-lic-7d/'))) continue;
      const id = boardId(m.permaslug);
      st.venue[id] = m;
      st.marks[id] = {
        mark: m.mark, venueId: m.market_id, status: m.effective_status || m.status,
        funding: m.funding ? n(m.funding.last_rate_ppm) / 1e6 : null,
        oi: atoms(m.open_interest_notional, dec),
      };
    }
  }
  applyMarkets(first);

  function loadKey() { try { return JSON.parse(localStorage.getItem(TRADING_KEY) || 'null'); } catch (e) { return null; } }
  function useKey(k) {
    const signer = KeySigner.fromNearSecret(k.secret);
    client = new ZfxPerpsClient(base, { accountId: k.account, signer, reconnect: false });
    acctNow = k.account;
    st.account = { label: k.account, id: k.account };
  }
  const saved = loadKey();
  if (saved) { try { useKey(saved); } catch (e) { st.error = 'saved trading key is invalid: ' + e.message; } }

  async function refresh() {
    try {
      applyMarkets(await raw('/v1/markets'));
      if (selected && st.marks[selected]) {
        const vid = st.marks[selected].venueId;
        st.book[selected] = await raw('/v1/book/' + encodeURIComponent(vid) + '?depth=12');
      }
      if (client) {
        const pf = await client.getPortfolio();
        const p = pf.pending;
        st.free = atoms(p.free, dec);
        st.positions = p.positions.filter((x) => x.size !== 0n).map((x) => {
          const bid = Object.keys(st.venue).find((k) => st.venue[k].market_id === x.market);
          return { market_id: bid || x.market, venue_market: x.market, size: x.size.toString(), margin: atoms(x.margin, dec), equity: atoms(x.equity, dec),
            unrealized_pnl: atoms(x.equity - x.margin, dec), mark: x.mark.toString(), liquidation_price: x.liquidationPrice == null ? null : x.liquidationPrice.toString(), entry_price: null };
        });
        st.orders = pf.openOrders || [];
        try { st.fills = (await signedGet('/v1/fills/' + encodeURIComponent(acctNow))).fills || []; } catch (e) { st.fills = []; }
        st.account.label = acctNow + (st.free != null ? ' · ' + usd(st.free) + ' free' : '');
      }
      st.error = null;
    } catch (e) { st.error = e.message; }
    emit();
  }
  // Signed account read for endpoints the SDK has no method for (fills). Same headers as the SDK's
  // portfolio read; the timestamp must be strictly newer than the last one or the node answers "replay".
  async function signedGet(path) {
    const ts = String(signTsMs());   // the SDK's per-page clock: strictly increasing across all signed calls
    const msg = clearing.authMessage(info.chainId, info.clearingContract, acctNow, ts);
    const sig = client.signer.sign(new TextEncoder().encode(msg));
    const b64 = btoa(String.fromCharCode(...sig));
    const r = await fetch(base + path, { headers: { 'x-zfx-perps-key': client.signer.publicKey, 'x-zfx-perps-ts': ts, 'x-zfx-perps-signature': b64 } });
    if (!r.ok) throw new Error(path + ': HTTP ' + r.status);
    return r.json();
  }
  setInterval(refresh, 4000);
  await refresh();

  /* ---------- rendering hooks used by app.js ---------- */
  function renderBook(m) {
    selected = m.marketId;
    const vm = st.marks[m.marketId];
    if (!vm) return '<div class="venue-off">This market is not listed on the connected venue.</div>';
    const b = st.book[m.marketId];
    if (!b) { refresh(); return '<div class="empty">Loading the book...</div>'; }
    const asks = (b.asks || []).slice(0, 10).reverse(), bids = (b.bids || []).slice(0, 10);
    const row = (l, c) => '<div class="brow"><span class="' + c + '">' + usd(Number(l[0])) + '</span><span class="mut">' + Number(l[1]).toLocaleString('en-US') + '</span></div>';
    return '<div style="padding:4px 0">' + (asks.length ? asks.map((l) => row(l, 'dn')).join('') : '<div class="empty">No asks</div>') +
      '<div class="spread"><span class="m" style="font-size:15px">' + usd(Number(vm.mark)) + '</span><span class="m fnt" style="font-size:10.5px">mark · ' + esc(String(vm.status).toLowerCase()) + '</span></div>' +
      (bids.length ? bids.map((l) => row(l, 'up')).join('') : '<div class="empty">No bids</div>') + '</div>';
  }

  const T = { side: 'long', type: 'limit', price: '', size: '10', msg: '' };
  function renderTicket(el, m, side) {
    selected = m.marketId;
    if (side && side !== T.lastSide) { T.side = side; T.lastSide = side; }
    const vm = st.marks[m.marketId];
    if (!vm) { el.innerHTML = '<div class="venue-off">This market is not listed on the connected venue.</div>'; return; }
    if (!client) {
      el.innerHTML = '<div class="venue-off">Import a testnet trading key to trade. Create one with <code>zfx-perps-near onboard-testnet</code> (see markets/zfx-perps/README.md), then press <i>Sign in</i>.</div><button type="button" class="submit" id="vSignIn">Sign in with a trading key</button>';
      el.querySelector('#vSignIn').onclick = signIn;
      return;
    }
    const isL = T.side === 'long', closeOnly = String(vm.status) !== 'ACTIVE';
    el.innerHTML =
      '<div class="sidesel" role="group" aria-label="Side"><button type="button" data-tside="long" class="' + (isL ? 'on-l' : '') + '">Long</button><button type="button" data-tside="short" class="' + (!isL ? 'on-s' : '') + '">Short</button></div>' +
      '<div class="otype" role="group" aria-label="Order type"><button type="button" data-otype="limit" aria-pressed="' + (T.type === 'limit') + '">Limit</button><button type="button" data-otype="market" aria-pressed="' + (T.type === 'market') + '">Market (IOC)</button></div>' +
      (T.type === 'limit' ? '<div class="field"><label class="lbl" for="vPx">Limit price, USD per day of index</label><div class="inp"><input id="vPx" inputmode="numeric" value="' + esc(T.price || vm.mark) + '"><span>$</span></div></div>' : '') +
      '<div class="field"><label class="lbl" for="vSz">Size</label><div class="inp"><input id="vSz" inputmode="numeric" value="' + esc(T.size) + '"><span>lots</span></div></div>' +
      '<div class="kv"><div><span>Mark</span><span class="m">' + usd(Number(vm.mark)) + '</span></div><div><span>Free collateral</span><span class="m">' + usd(st.free) + '</span></div><div><span>Max leverage</span><span class="m">' + esc(st.venue[m.marketId].params.max_leverage) + 'x</span></div><div><span>Status</span><span class="m">' + esc(String(vm.status).toLowerCase()) + '</span></div></div>' +
      '<div class="toast" role="status">' + esc(T.msg) + '</div>' +
      '<button type="submit" class="submit" style="background:var(--' + (isL ? 'long' : 'short') + ')"' + (closeOnly ? ' disabled' : '') + '>' + (closeOnly ? 'Market is ' + esc(String(vm.status).toLowerCase()) : (isL ? 'Long ' : 'Short ') + esc(T.size) + ' lots') + '</button>';
    el.onclick = (e) => {
      const s = e.target.closest('[data-tside]'), o = e.target.closest('[data-otype]');
      if (s) { T.side = s.getAttribute('data-tside'); T.msg = ''; renderTicket(el, m); }
      if (o) { T.type = o.getAttribute('data-otype'); renderTicket(el, m); }
    };
    el.oninput = (e) => {
      if (e.target.id === 'vPx') T.price = e.target.value.replace(/[^0-9]/g, '');
      if (e.target.id === 'vSz') T.size = e.target.value.replace(/[^0-9]/g, '');
    };
    el.onsubmit = async (e) => {
      e.preventDefault();
      const qty = BigInt(T.size || '0');
      if (qty <= 0n) { T.msg = 'Enter a size above 0.'; renderTicket(el, m); return; }
      const mark = BigInt(vm.mark), band = (mark * 3n) / 100n;   // market orders: IOC limit 3% through the mark
      const price = T.type === 'limit' ? BigInt(T.price || vm.mark) : (isL ? mark + band : mark - band);
      T.msg = 'Signing...'; renderTicket(el, m);
      try {
        const ack = await client.placeOrder({ marketId: vm.venueId, side: isL ? 'BUY' : 'SELL', type: 'LIMIT', price, quantity: qty,
          timeInForce: T.type === 'market' ? 'IOC' : 'GTC' });
        T.msg = (ack.status || 'placed') + ': filled ' + (ack.filled ?? 0) + ', remaining ' + (ack.remaining ?? 0) + '. Settles on NEAR in the next batch.';
      } catch (err) { T.msg = 'Refused: ' + err.message; }
      renderTicket(el, m); refresh();
    };
  }

  function renderAccount(tab) {
    if (!client) return '<div class="empty">Sign in with a testnet trading key to see orders and fills.</div>';
    if (tab === 'ord') {
      if (!st.orders.length) return '<div class="empty">No open orders.</div>';
      setTimeout(() => document.querySelectorAll('[data-vcancel]').forEach((b) => { b.onclick = async () => {
        const o = st.orders[+b.getAttribute('data-vcancel')];
        try { await client.cancelOrder({ orderHash: o.order_hash, expiryNs: o.expiry_ns }, { relayOnChain: true }); } catch (e) { alert('Cancel refused: ' + e.message); }
        refresh();
      }; }), 0);
      return '<table class="tbl"><thead><tr><th class="l">Market</th><th>Side</th><th>Price</th><th>Size</th><th>Remaining</th><th></th></tr></thead><tbody>' +
        st.orders.map((o, k) => '<tr><td class="l sans">' + esc(o.market) + '</td><td class="' + (o.side === 'BUY' ? 'up' : 'dn') + '">' + (o.side === 'BUY' ? 'Long' : 'Short') + '</td><td>' + usd(Number(o.price)) + '</td><td>' + esc(o.quantity) + '</td><td>' + esc(o.remaining) + '</td><td><button type="button" class="mini" data-vcancel="' + k + '">Cancel</button></td></tr>').join('') + '</tbody></table>';
    }
    if (!st.fills.length) return '<div class="empty">No fills since the venue started.</div>';
    return '<table class="tbl"><thead><tr><th class="l">Time (UTC)</th><th class="l">Market</th><th>Side</th><th>Price</th><th>Size</th><th>Fee</th><th>Role</th></tr></thead><tbody>' +
      st.fills.map((f) => '<tr><td class="l">' + esc(tdate(f.ts_ns)) + '</td><td class="sans" style="text-align:left">' + esc(f.market) + '</td><td class="' + (f.side === 'BUY' ? 'up' : 'dn') + '">' + (f.side === 'BUY' ? 'Buy' : 'Sell') + '</td><td>' + usd(Number(f.price)) + '</td><td>' + esc(f.qty) + '</td><td class="mut">' + usd(atoms(f.fee, dec)) + '</td><td class="fnt">' + esc(f.role) + '</td></tr>').join('') + '</tbody></table>';
  }

  function signIn() {
    const account = prompt('Your NEAR testnet account (the one you onboarded with zfx-perps-near onboard-testnet):');
    if (!account) return;
    const secret = prompt('Trading key secret for ' + account + ' (ed25519:...). Testnet only. It is stored in this browser.');
    if (!secret) return;
    try {
      const k = { account: account.trim(), secret: secret.trim() };
      KeySigner.fromNearSecret(k.secret);
      localStorage.setItem(TRADING_KEY, JSON.stringify(k));
      useKey(k); refresh();
    } catch (e) { alert('That is not a valid trading key: ' + e.message); }
  }

  return {
    state: () => st,
    onUpdate: (f) => listeners.push(f),
    renderBook, renderTicket, renderAccount,
    markHistory: () => [],
    accountAction: () => {
      if (!client) { signIn(); return; }
      if (confirm('Signed in as ' + acctNow + '. Deposits are NEAR transactions from your wallet (zfx-perps-near onboard-testnet --deposit N). Sign out of this browser?')) {
        localStorage.removeItem(TRADING_KEY); client = null; st.account = null; st.positions = []; st.orders = []; st.fills = []; emit();
      }
    },
  };
}

window.ZfxVenue = { connect };
if (window.__zfxVenueResolve) window.__zfxVenueResolve(window.ZfxVenue);
