/* ZFX Markets trade app.
 *
 * Index data: the published ZFX Markets index (static JSON, same paths as the read-only API):
 *   <INDEX>/trade/board.json                       compact board, built with the site
 *   <INDEX>/v1/indices/or-model-spend-7d/<slug>/series.json
 *   <INDEX>/v1/indices/or-model-spend/<slug>/series.json
 * Venue data (mark, book, positions, orders): a zfx-perps node, through window.ZfxVenue
 * (venue.js). Without a venue the page shows the index only and says so; it never invents
 * prices, fills or balances.
 *
 * Display only: values are formatted from decimal strings with Number(). No money is computed here;
 * order sizing and margin come from the venue SDK, which uses fixed-point strings.
 */
(function () {
  'use strict';
  var DAY = 864e5;
  var MON = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  var Q = new URLSearchParams(location.search);
  var INDEX = (Q.get('index') || '').replace(/\/$/, '');
  var MIN_TRADABLE_CAP = 10000; // markets with a smaller manipulation-bounded OI cap are shown as index only
  function $(s) { return document.querySelector(s); }
  function esc(s) { return String(s == null ? '' : s).replace(/[&<>"]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]; }); }
  function n(v) { return v == null || v === '' ? null : Number(v); }
  function num(v, d) { return v.toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d }); }
  function usd(v) {
    if (v == null) return 'n/a';
    var a = Math.abs(v), s = v < 0 ? '−$' : '$';
    if (a >= 1e6) return s + num(a / 1e6, 2) + 'M';
    if (a >= 1e3) return s + num(a / 1e3, a >= 1e5 ? 0 : 1) + 'K';
    return s + num(a, 0);
  }
  function tok(v) { if (v == null) return 'n/a'; var a = Math.abs(v); return a >= 1e12 ? num(a / 1e12, 2) + 'T' : a >= 1e9 ? num(a / 1e9, 1) + 'B' : a >= 1e6 ? num(a / 1e6, 1) + 'M' : num(a, 0); }
  function pct(x, d) { if (x == null || isNaN(x)) return 'n/a'; return (x >= 0 ? '+' : '−') + Math.abs(x).toFixed(d == null ? 2 : d) + '%'; }
  function cls(x) { return x == null ? 'na' : x >= 0 ? 'up' : 'dn'; }
  function pad2(v) { return (v < 10 ? '0' : '') + v; }
  function hms(ms) { var s = Math.max(0, Math.floor(ms / 1000)); return pad2(Math.floor(s / 3600)) + ':' + pad2(Math.floor(s % 3600 / 60)) + ':' + pad2(s % 60); }
  function dLabel(iso) { var d = new Date(iso + 'T00:00:00Z'); return MON[d.getUTCMonth()] + ' ' + d.getUTCDate(); }
  function getJSON(p) { return fetch(INDEX + p, { cache: 'no-cache' }).then(function (r) { if (!r.ok) throw new Error(p + ': HTTP ' + r.status); return r.json(); }); }

  var B = null;          // board.json
  var M = [];            // markets
  var V = null;          // venue adapter (window.ZfxVenue) once connected
  var VS = {};           // venue state: { marks: {market_id: {mark, funding, oi}}, account, positions, orders }
  var S = { view: 'board', sel: 0, filter: 'all', sort: 'index', series: 'both', range: 30, acct: 'pos', sideTab: 'data', hover: -1 };
  var SERIES = {};       // slug -> { d7: points, d: points }
  var USAGE = {};        // slug -> [[date, tokens]] from OpenRouter's documented dataset
  var LABS = null;       // labs.json

  /* ---------- load ---------- */
  function load() {
    return getJSON('/trade/board.json').then(function (b) {
      B = b;
      M = b.markets.map(function (m, i) {
        var tradable = m.lifecycle === 'ACTIVE' && n(m.max_open_interest_usd) >= MIN_TRADABLE_CAP;
        return { i: i, slug: m.permaslug, id: m.ticker, name: m.name, org: m.author, ow: m.open_weights, idx7: n(m.index_7d_usd), day: n(m.latest_day_usd),
          dayDate: m.latest_day, chg: m.change_7d == null ? null : Number(m.change_7d) * 100, tokens: n(m.tokens_total), cost: n(m.manipulation_cost_usd_per_pct),
          cap: n(m.max_open_interest_usd), life: m.lifecycle, lifeReason: m.lifecycle_reason, spark: (m.spark_7d || []).map(Number), tradable: tradable,
          status7: m.status_7d, conf: m.confidence, printHash: m.print_hash_7d, marketId: m.market_id, source: m.index_source || 'licensed', perps: m.perps_market || null };
      });
      $('#srcNote').innerHTML = esc(b.index_note || '') + ' ' + esc(b.attribution) + '. <a href="/docs/data_method.html">How the index is built</a>.';
    });
  }
  function loadSeries(m) {
    if (SERIES[m.slug]) return Promise.resolve(SERIES[m.slug]);
    return Promise.all([
      getJSON('/v1/indices/' + (m.source === 'web' ? 'or-model-spend-7d/' : 'or-model-spend-lic-7d/') + m.slug + '/series.json'),
      getJSON('/v1/indices/' + (m.source === 'web' ? 'or-model-spend/' : 'or-model-spend-lic/') + m.slug + '/series.json'),
      getJSON('/v1/datasets/rankings-daily/models/' + m.slug + '.json').catch(function () { return { points: [] }; })
    ]).then(function (r) { SERIES[m.slug] = { d7: r[0].points, d: r[1].points }; USAGE[m.slug] = r[2].points || []; return SERIES[m.slug]; });
  }

  /* ---------- venue ---------- */
  function venueMark(m) { var x = VS.marks && VS.marks[m.marketId]; return x ? x : null; }
  function setVenueStatus() {
    var on = !!V;
    $('#vStatus').textContent = on ? 'Testnet · venue ' + (VS.label || 'connected') : 'Index updated daily · on NEAR: oracle.zeroflux.testnet';
    $('#ctaBtn').hidden = !(on || Q.get('venue'));
    $('#vDot').style.background = on ? 'var(--long)' : 'var(--faint)';
    $('#bal').textContent = on && VS.account ? VS.account.label : 'not connected';
    $('#ctaBtn').textContent = on ? (VS.account ? 'Deposit' : 'Sign in') : 'Connect venue';
  }
  function connectVenue(url) {
    (window.ZfxVenueReady || Promise.reject(new Error('venue support is not loaded'))).then(function (ZV) { return ZV.connect(url); }).then(function (v) {
      V = v; try { localStorage.setItem('zfx-venue', url); } catch (e) {}
      V.onUpdate(function (st) { VS = st; setVenueStatus(); render(); });
      VS = V.state(); setVenueStatus(); render();
    }).catch(function (e) { alert('Could not connect to the venue at ' + url + ': ' + e.message); });
  }
  $('#ctaBtn').addEventListener('click', function () {
    if (!V) {
      var saved = ''; try { saved = localStorage.getItem('zfx-venue') || ''; } catch (e) {}
      var url = prompt('zfx-perps node URL (testnet). See the venue panel on any market for how to run one.', saved || 'http://127.0.0.1:8900');
      if (url) connectVenue(url.replace(/\/$/, ''));
    } else if (V.accountAction) V.accountAction();
  });

  /* ---------- routing ---------- */
  function go(view, i) {
    if (i != null) S.sel = i;
    S.view = view;
    var h = view === 'board' ? '#board' : view === 'labs' ? '#labs' : '#m-' + M[S.sel].slug;
    if (location.hash !== h) { try { history.replaceState(null, '', h); } catch (e) { location.hash = h; } }
    render();
    if (view === 'trade') loadSeries(M[S.sel]).then(function () { if (S.view === 'trade') { renderTrade(); draw(); } }).catch(function (e) { $('#readout').textContent = e.message; });
  }
  function fromHash() {
    var h = decodeURIComponent(location.hash.slice(1));
    if (h === 'labs') { S.view = 'labs'; return; }
    if (h.indexOf('m-') === 0) { var f = M.filter(function (m) { return m.slug === h.slice(2); })[0]; if (f) { S.sel = f.i; S.view = 'trade'; return; } }
    S.view = 'board';
  }
  window.addEventListener('hashchange', function () { fromHash(); go(S.view); });
  $('#navBoard').addEventListener('click', function () { go('board'); });
  $('#navTrade').addEventListener('click', function () { go('trade'); });
  $('#navLabs').addEventListener('click', function () { go('labs'); });

  /* ---------- board ---------- */
  function spark(m) {
    var pts = m.spark.filter(function (v) { return v > 0; });
    if (pts.length < 2) return '<span class="fnt">n/a</span>';
    var lo = Math.min.apply(null, pts), hi = Math.max.apply(null, pts), d = '';
    pts.forEach(function (v, k) { d += (k ? 'L' : 'M') + (k * 100 / (pts.length - 1)).toFixed(1) + ' ' + (26 - (v - lo) / (hi - lo || 1) * 22).toFixed(1); });
    var up = pts[pts.length - 1] >= pts[0];
    return '<svg viewBox="0 0 100 28" width="80" height="28" aria-hidden="true" style="display:block;margin-left:auto;overflow:visible"><path d="' + d + '" fill="none" stroke="var(--' + (up ? 'long' : 'short') + ')" stroke-width="1.2" vector-effect="non-scaling-stroke"/></svg>';
  }
  function lifePill(m) {
    if (m.source === 'web') return ' <span class="pill" title="Not in the licensed top 50 every day: shown with the web-endpoint index">web index</span>';
    if (m.life === 'ACTIVE' && m.tradable) return '';
    if (m.life === 'ACTIVE') return ' <span class="pill" title="OI cap below $10K: index only">index only</span>';
    return ' <span class="pill warn" title="' + esc(m.lifeReason || '') + '">' + esc(m.life.replace('_', '-').toLowerCase()) + '</span>';
  }
  function renderBoard() {
    if (!B) return;
    var tI = 0, tD = 0, tC = 0, tr = 0, oi = 0;
    M.forEach(function (m) { tI += m.idx7 || 0; tD += m.day || 0; if (m.tradable) { tC += m.cap || 0; tr++; } var vm = venueMark(m); if (vm && vm.oi) oi += Number(vm.oi); });
    $('#sIdx').textContent = usd(tI); $('#sDay').textContent = usd(tD); $('#sDayLbl').textContent = 'Latest day · ' + dLabel(B.latest_day);
    var onChain = M.filter(function (m) { return m.perps; }).length, tks = M.reduce(function (a, m) { return a + (m.tokens || 0); }, 0);
    $('#sTrad').textContent = onChain + ' of ' + M.length; $('#sCap').textContent = usd(tC);
    $('#sOiLbl').textContent = V ? 'Venue open interest' : 'Tokens per day, tracked models'; $('#sOi').textContent = V ? usd(oi) : tok(tks);
    var F = [['all', 'All'], ['tradable', 'Tradable'], ['closed', 'Closed weights'], ['open', 'Open weights']];
    $('#filters').innerHTML = F.map(function (f) { return '<button type="button" class="chip" data-filter="' + f[0] + '" aria-pressed="' + (S.filter === f[0]) + '">' + f[1] + '</button>'; }).join('');
    var SO = [['index', 'Size'], ['chg', '7D change'], ['cost', 'Hardest to move']];
    $('#sorts').innerHTML = SO.map(function (f) { return '<button type="button" class="chip" data-sort="' + f[0] + '" aria-pressed="' + (S.sort === f[0]) + '">' + f[1] + '</button>'; }).join('');
    var rows = M.filter(function (m) { return S.filter === 'all' || (S.filter === 'tradable' ? m.tradable : S.filter === 'open' ? m.ow : !m.ow); }).sort(function (a, b) {
      return S.sort === 'chg' ? Math.abs(b.chg || 0) - Math.abs(a.chg || 0) : S.sort === 'cost' ? (b.cost || 0) - (a.cost || 0) : (b.idx7 || 0) - (a.idx7 || 0); });
    document.querySelector('.lb').classList.toggle('no-venue', !V);
    $('#lbBody').innerHTML = rows.map(function (m, k) {
      var vm = venueMark(m), canTrade = !!V && m.tradable;
      return '<tr data-open="' + m.i + '">' +
        '<td class="l fnt">' + (k + 1) + '</td>' +
        '<td class="l"><button type="button" class="name" data-open="' + m.i + '"><span>' + esc(m.name) + lifePill(m) + '</span><span>' + esc(m.org) + (m.ow ? ' · open weights' : '') + ' · ' + esc(m.id) + '-PERP</span></button></td>' +
        '<td>' + spark(m) + '</td>' +
        '<td>' + usd(m.idx7) + '</td>' +
        '<td class="mut c-day">' + usd(m.day) + '</td>' +
        '<td class="' + cls(m.chg) + '">' + pct(m.chg) + '</td>' +
        '<td class="mut">' + tok(m.tokens) + '</td>' +
        '<td class="mut">' + usd(m.cost) + '</td>' +
        '<td class="mut">' + usd(m.cap) + '</td>' +
        '<td class="c-mark ' + (vm ? '' : 'na') + '">' + (vm ? usd(Number(vm.mark)) : 'n/a') + '</td>' +
        '<td class="c-trade" style="text-align:center"><span class="ls"><button type="button" class="bl" data-side="long" data-i="' + m.i + '"' + (canTrade ? '' : ' disabled') + ' aria-label="Long ' + esc(m.name) + '">Long</button><button type="button" class="bs" data-side="short" data-i="' + m.i + '"' + (canTrade ? '' : ' disabled') + ' aria-label="Short ' + esc(m.name) + '">Short</button></span></td></tr>';
    }).join('');
    renderPositions($('#bPos'), $('#bPnl'));
  }
  $('#vBoard').addEventListener('click', function (e) {
    var b = e.target.closest('[data-side]'); if (b) { if (!b.disabled) { S.side = b.getAttribute('data-side'); go('trade', +b.getAttribute('data-i')); } return; }
    var f = e.target.closest('[data-filter]'); if (f) { S.filter = f.getAttribute('data-filter'); renderBoard(); return; }
    var so = e.target.closest('[data-sort]'); if (so) { S.sort = so.getAttribute('data-sort'); renderBoard(); return; }
    var o = e.target.closest('[data-open]'); if (o) go('trade', +o.getAttribute('data-open'));
  });
  function renderPositions(el, pnlEl) {
    if (!V) { el.innerHTML = '<div class="empty">Connect a testnet venue to see positions.</div>'; pnlEl.textContent = ''; return; }
    var P = VS.positions || [];
    el.innerHTML = P.length ? P.map(function (p) {
      var m = M.filter(function (x) { return x.marketId === p.market_id; })[0];
      return '<div class="prow"' + (m ? ' data-open="' + m.i + '"' : '') + '><span>' + esc(m ? m.name : p.market_id) + '</span><span class="m ' + cls(Number(p.unrealized_pnl)) + '" style="text-align:right;font-size:12.5px">' + usd(Number(p.unrealized_pnl)) + '</span>' +
        '<span class="m fnt" style="font-size:11px"><span class="' + (Number(p.size) > 0 ? 'up' : 'dn') + '">' + (Number(p.size) > 0 ? 'Long' : 'Short') + '</span> ' + esc(String(Math.abs(Number(p.size)))) + ' lots · equity ' + usd(Number(p.equity)) + '</span>' +
        '<span class="m fnt" style="font-size:11px;text-align:right">' + (p.liquidation_price ? 'liq ' + usd(Number(p.liquidation_price)) : '') + '</span></div>';
    }).join('') : '<div class="empty">No open positions.</div>';
    var tot = P.reduce(function (a, p) { return a + Number(p.unrealized_pnl || 0); }, 0);
    pnlEl.textContent = usd(tot); pnlEl.className = 'm ' + cls(tot);
  }

  /* ---------- trade view ---------- */
  function renderTick() {
    var m = M[S.sel], vm = venueMark(m);
    $('#tick').innerHTML =
      '<button type="button" class="back" id="backBtn"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 18l-6-6 6-6"/></svg>All markets</button>' +
      '<div class="tname"><span class="n">' + esc(m.name) + '</span><span class="fnt m" style="font-size:11px">' + esc(m.org) + ' · ' + esc(m.id) + '-PERP</span></div>' +
      '<div><span class="lbl">7D index</span><span class="v">' + usd(m.idx7) + '</span></div>' +
      '<div><span class="lbl">Day · ' + esc(dLabel(m.dayDate || B.latest_day)) + '</span><span class="v mut">' + usd(m.day) + '</span></div>' +
      '<div><span class="lbl">7D change</span><span class="v ' + cls(m.chg) + '">' + pct(m.chg) + '</span></div>' +
      (V ? '<div><span class="lbl">Mark</span><span class="v ' + (vm ? '' : 'na') + '">' + (vm ? usd(Number(vm.mark)) : 'not listed') + '</span></div>' +
        '<div><span class="lbl">Funding</span><span class="v ' + (vm ? '' : 'na') + '">' + (vm && vm.funding != null ? pct(Number(vm.funding) * 100, 4) : 'n/a') + '</span></div>'
        : '<div><span class="lbl">Tokens · day</span><span class="v">' + tok(m.tokens) + '</span></div>' +
        '<div><span class="lbl">Index</span><span class="v mut">' + (m.source === 'web' ? 'web 7D' : 'licensed 7D') + '</span></div>') +
      '<div><span class="lbl">Cost to move 1%</span><span class="v">' + usd(m.cost) + '</span></div>' +
      '<div><span class="lbl">OI cap</span><span class="v">' + usd(m.cap) + '</span></div>' +
      '<div><span class="lbl">Status</span><span class="v ' + (m.life === 'ACTIVE' ? 'up' : 'dn') + '">' + esc(m.life.replace('_', ' ').toLowerCase()) + '</span></div>' +
      '<div class="cd"><span class="lbl">Next index day · 00:00 UTC</span><span class="v up" id="idxCd"></span></div>';
  }
  function renderWatch() {
    $('#watch').innerHTML = M.slice().sort(function (a, b) { return (b.idx7 || 0) - (a.idx7 || 0); }).map(function (m) {
      return '<button type="button" class="wrow" data-pick="' + m.i + '" aria-current="' + (m.i === S.sel) + '"><span class="n">' + esc(m.name) + '</span><span class="m" style="font-size:12px;text-align:right">' + usd(m.idx7) + '</span>' +
        '<span class="fnt" style="font-size:11px">' + esc(m.org) + '</span><span class="m ' + cls(m.chg) + '" style="font-size:11px;text-align:right">' + pct(m.chg) + '</span></button>';
    }).join('');
  }
  $('#watch').addEventListener('click', function (e) { var b = e.target.closest('[data-pick]'); if (b) go('trade', +b.getAttribute('data-pick')); });
  $('#tick').addEventListener('click', function (e) { if (e.target.closest('#backBtn')) go('board'); });

  function renderSide() {
    var tabs = V ? [['data', 'Index data'], ['about', 'Details'], ['book', 'Order book']] : [['data', 'Index data'], ['about', 'Details']];
    if (!V && S.sideTab === 'book') S.sideTab = 'data';
    $('#sideTabs').innerHTML = tabs.map(function (t) { return '<button type="button" role="tab" data-stab="' + t[0] + '" aria-selected="' + (S.sideTab === t[0]) + '">' + t[1] + '</button>'; }).join('');
    var m = M[S.sel], s = SERIES[m.slug], html = '';
    if (S.sideTab === 'data') {
      if (!s) html = '<div class="empty">Loading...</div>';
      else {
        var pts = s.d.slice(-10).reverse();
        html = '<table class="tbl idxtbl"><thead><tr><th class="l">Day</th><th>Value</th><th>Tokens</th><th>Status</th></tr></thead><tbody>' + pts.map(function (p) {
          return '<tr><td class="l">' + esc(dLabel(p.utc_date)) + '</td><td>' + usd(n(p.value_usd)) + '</td><td class="mut">' + tok(n(p.total_tokens || (p.tokens && p.tokens.total))) + '</td><td class="fnt">' + esc((p.data_status || '') + (p.finality ? ' · ' + p.finality.toLowerCase() : '')) + '</td></tr>';
        }).join('') + '</tbody></table>' +
        '<div class="src">Each day: OpenRouter tokens from its documented daily dataset, times a frozen billed share and input/output mix, times documented listed prices. Every day is signed by the index reporter. Latest 7D print <span class="m">' + esc((m.printHash || '').slice(0, 16)) + '…</span>. <a href="/v1/indices/or-model-spend-7d/' + esc(m.slug) + '/series.json">Raw series</a></div>';
      }
    } else if (S.sideTab === 'about') {
      html = details(m);
    } else {
      html = V && V.renderBook ? V.renderBook(m) : venueOffline();
    }
    $('#book').innerHTML = html;
  }
  $('#sideTabs').addEventListener('click', function (e) { var b = e.target.closest('[data-stab]'); if (b) { S.sideTab = b.getAttribute('data-stab'); renderSide(); } });

  function details(m) {
    var row = function (a, b) { return '<div style="display:flex;justify-content:space-between;gap:12px;padding:9px 14px;border-bottom:1px solid var(--line);font-size:12.5px"><span class="fnt">' + a + '</span><span class="m" style="text-align:right">' + b + '</span></div>'; };
    var ix = (m.source === 'web' ? 'or-model-spend-7d/' : 'or-model-spend-lic-7d/') + m.slug;
    return row('Index', m.source === 'web' ? 'web-endpoint 7-day' : 'licensed 7-day') +
      row('Formula', m.source === 'web' ? 'ZFX-OR-MODEL-SPEND-USD-7D-v1' : 'ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1') +
      row('7-day index', usd(m.idx7) + ' / day') + row('Latest day', usd(m.day)) + row('Tokens, latest day', tok(m.tokens)) +
      row('Cost to move 1%', usd(m.cost)) + row('Open-interest cap', usd(m.cap)) + row('Status', esc(m.life.replace('_', ' ').toLowerCase())) +
      row('Perps market', m.perps ? '<a href="https://testnet.nearblocks.io/address/perps-v2.zeroflux.testnet" target="_blank" rel="noopener">' + esc(m.perps) + '</a>' : 'not listed') +
      row('On-chain oracle', m.source === 'web' ? 'not pushed' : '<a href="https://testnet.nearblocks.io/address/oracle.zeroflux.testnet" target="_blank" rel="noopener">oracle.zeroflux.testnet</a>') +
      row('Latest print', '<span title="' + esc(m.printHash || '') + '">' + esc((m.printHash || '').slice(0, 14)) + '…</span>') +
      '<div class="src">Downloads: <a href="/v1/indices/' + esc(ix) + '/series.json">7-day series</a> · <a href="/v1/datasets/rankings-daily/models/' + esc(m.slug) + '.json">daily tokens</a> · <a href="/v1/markets/models.json">all models</a>. Implied spend on one venue: not revenue, royalties or equity.</div>';
  }
  function venueOffline() {
    return '<div class="venue-off"><b style="color:var(--ink);font-weight:400">No trading venue connected.</b> The markets clear on NEAR testnet (perps-v2.zeroflux.testnet), but ZeroFlux has not hosted a public order-book node yet. With a node URL, press <i>Connect venue</i>. To get a funded testnet trading key:' +
      '<pre>' + esc(B && B.venue_howto ? B.venue_howto : 'see markets/zfx-perps/README.md') + '</pre></div>';
  }
  function renderTicket() {
    var m = M[S.sel];
    $('#ticket').hidden = !V;
    if (!V) return;
    if (V && V.renderTicket && m.tradable) { V.renderTicket($('#ticket'), m, S.side || 'long'); return; }
    var why = !V ? '' : !m.tradable ? '<div class="venue-off">This market is ' + (m.life === 'ACTIVE' ? 'index only: its open-interest cap is under $10K.' : m.life.replace('_', '-').toLowerCase() + ': ' + esc(m.lifeReason || 'see the index status') + '.') + '</div>' : '';
    $('#ticket').innerHTML = (why || (!V ? venueOffline() : '')) + '<button type="button" class="submit" disabled>Trading needs a connected venue</button>';
  }
  function renderAcct() {
    var tabs = [['pos', 'Positions'], ['ord', 'Open orders'], ['fills', 'Trade history']];
    $('#acctTabs').innerHTML = tabs.map(function (t) { return '<button type="button" role="tab" data-atab="' + t[0] + '" aria-selected="' + (S.acct === t[0]) + '">' + t[1] + '</button>'; }).join('');
    if (!V) {
      $('#acctTabs').innerHTML = '<button type="button" role="tab" aria-selected="true">Daily values</button>';
      $('#tPnl').textContent = ''; $('#tPnl').previousElementSibling.textContent = '';
      var m = M[S.sel], s = SERIES[m.slug];
      if (!s) { $('#acct').innerHTML = '<div class="empty">Loading...</div>'; return; }
      var w = {}; s.d7.forEach(function (p) { w[p.utc_date] = p; });
      $('#acct').innerHTML = '<table class="tbl"><thead><tr><th class="l">Day (UTC)</th><th>7D index</th><th>Day value</th><th>Tokens</th><th>Input $/M</th><th>Output $/M</th><th>Status</th><th>Print</th></tr></thead><tbody>' +
        s.d.slice().reverse().map(function (p) {
          var x = w[p.utc_date] || {}, pr = p.price || {};
          return '<tr><td class="l">' + esc(p.utc_date) + '</td><td>' + usd(x.data_status === 'OK' ? n(x.value_usd) : null) + '</td><td>' + usd(p.data_status === 'OK' ? n(p.value_usd) : null) + '</td><td class="mut">' + tok(n(p.total_tokens || (p.tokens && p.tokens.total))) + '</td>' +
            '<td class="mut">' + (pr.p_in_usd_per_m ? '$' + Number(pr.p_in_usd_per_m).toFixed(3) : 'n/a') + '</td><td class="mut">' + (pr.p_out_usd_per_m || pr.listed_out_usd_per_m ? '$' + Number(pr.p_out_usd_per_m || pr.listed_out_usd_per_m).toFixed(2) : 'n/a') + '</td>' +
            '<td class="fnt">' + esc((p.data_status || '').toLowerCase() + (p.finality ? ' · ' + p.finality.toLowerCase() : '')) + '</td><td class="fnt">' + esc((p.print_hash || '').slice(0, 10)) + '</td></tr>';
        }).join('') + '</tbody></table>';
      return;
    }
    if (S.acct === 'pos') renderPositions($('#acct'), $('#tPnl'));
    else $('#acct').innerHTML = V.renderAccount ? V.renderAccount(S.acct) : '';
  }
  $('#acctTabs').addEventListener('click', function (e) { var b = e.target.closest('[data-atab]'); if (b) { S.acct = b.getAttribute('data-atab'); renderAcct(); } });

  /* ---------- chart: daily values (dots), 7D index (line), mark (dashed, from the venue) ---------- */
  function renderChartCtl() {
    $('#ctype').innerHTML = [['both', '7D + daily'], ['7d', '7D only'], ['tokens', 'Tokens / day']].map(function (c) { return '<button type="button" class="chip" data-ctype="' + c[0] + '" aria-pressed="' + (S.series === c[0]) + '">' + c[1] + '</button>'; }).join('');
    $('#tfs').innerHTML = [[14, '14D'], [30, '30D'], [9999, 'All']].map(function (t) { return '<button type="button" class="chip" data-tf="' + t[0] + '" aria-pressed="' + (S.range === t[0]) + '">' + t[1] + '</button>'; }).join('');
  }
  $('#ctype').addEventListener('click', function (e) { var b = e.target.closest('[data-ctype]'); if (b) { S.series = b.getAttribute('data-ctype'); renderChartCtl(); draw(); } });
  $('#tfs').addEventListener('click', function (e) { var b = e.target.closest('[data-tf]'); if (b) { S.range = +b.getAttribute('data-tf'); renderChartCtl(); draw(); } });
  $('#resetView').addEventListener('click', function () { S.range = 30; S.series = 'both'; renderChartCtl(); draw(); });
  var cv = $('#chart'), ctx = cv.getContext('2d'), G = null;
  function draw() {
    if (S.view !== 'trade') return;
    var m = M[S.sel], s = SERIES[m.slug], box = $('#chartBox'), W = box.clientWidth, H = box.clientHeight;
    if (!W || !H) return;
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    if (cv.width !== Math.round(W * dpr) || cv.height !== Math.round(H * dpr)) { cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr); }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0); ctx.clearRect(0, 0, W, H);
    if (!s) return;
    var cs = getComputedStyle(document.documentElement), C = function (k) { return cs.getPropertyValue(k).trim(); };
    if (S.series === 'tokens') { drawUsage(m, W, H, C); return; }
    var days = {};
    s.d.forEach(function (p) { days[p.utc_date] = days[p.utc_date] || {}; days[p.utc_date].d = p.data_status === 'OK' ? n(p.value_usd) : null; days[p.utc_date].dp = p; });
    s.d7.forEach(function (p) { days[p.utc_date] = days[p.utc_date] || {}; days[p.utc_date].w = p.data_status === 'OK' ? n(p.value_usd) : null; days[p.utc_date].wp = p; });
    var keys = Object.keys(days).sort().slice(-S.range), marks = V && V.markHistory ? V.markHistory(m) : [];
    var pts = keys.map(function (k) { return { k: k, d: days[k].d, w: days[k].w, dp: days[k].dp, wp: days[k].wp }; });
    var vals = []; pts.forEach(function (p) { if (p.w != null) vals.push(p.w); if (S.series === 'both' && p.d != null) vals.push(p.d); });
    marks.forEach(function (x) { vals.push(Number(x.mark)); });
    if (!vals.length) { $('#readout').textContent = 'No published values yet.'; return; }
    var lo = Math.min.apply(null, vals), hi = Math.max.apply(null, vals), r = hi - lo || hi || 1; lo -= r * 0.08; hi += r * 0.08; if (lo < 0) lo = 0;
    var padR = 76, padB = 24, padT = 24, pw = W - padR, ph = H - padB - padT, cw = pw / Math.max(1, pts.length);
    var X = function (i) { return i * cw + cw / 2; }, Y = function (v) { return padT + (hi - v) / (hi - lo) * ph; };
    ctx.font = '400 10.5px "JetBrains Mono", ui-monospace, monospace'; ctx.textBaseline = 'middle'; ctx.textAlign = 'left';
    var step = Math.pow(10, Math.floor(Math.log10((hi - lo) / 4))), f = (hi - lo) / 4 / step; step *= f < 1.5 ? 1 : f < 3 ? 2 : f < 7 ? 5 : 10;
    for (var v = Math.ceil(lo / step) * step; v <= hi; v += step) { var yy = Math.round(Y(v)) + 0.5; ctx.strokeStyle = C('--grid'); ctx.lineWidth = 1; ctx.beginPath(); ctx.moveTo(0, yy); ctx.lineTo(pw, yy); ctx.stroke(); ctx.fillStyle = C('--faint'); ctx.fillText(usd(v), pw + 8, yy); }
    ctx.textAlign = 'center'; var last = -1e9;
    pts.forEach(function (p, i) { if (X(i) - last > 64 && X(i) > 28 && X(i) < pw - 20) { ctx.fillStyle = C('--faint'); ctx.fillText(dLabel(p.k), X(i), H - padB / 2); last = X(i); } });
    if (S.series === 'both') { ctx.fillStyle = C('--muted'); pts.forEach(function (p, i) { if (p.d != null) ctx.fillRect(Math.round(X(i)) - 2, Math.round(Y(p.d)) - 2, 4, 4); }); }
    ctx.strokeStyle = C('--long'); ctx.lineWidth = 2; ctx.beginPath(); var started = false;
    pts.forEach(function (p, i) { if (p.w == null) { started = false; return; } if (!started) { ctx.moveTo(X(i), Y(p.w)); started = true; } else ctx.lineTo(X(i), Y(p.w)); });
    ctx.stroke();
    pts.forEach(function (p, i) { if (p.wp && p.wp.clamped) { ctx.strokeStyle = C('--short'); ctx.lineWidth = 1; ctx.beginPath(); ctx.arc(X(i), Y(p.w), 5, 0, Math.PI * 2); ctx.stroke(); } });
    if (marks.length) { ctx.strokeStyle = C('--short'); ctx.setLineDash([4, 3]); ctx.lineWidth = 1.2; ctx.beginPath(); var mx = Number(marks[marks.length - 1].mark); ctx.moveTo(0, Y(mx)); ctx.lineTo(pw, Y(mx)); ctx.stroke(); ctx.setLineDash([]); }
    var h = S.hover >= 0 && S.hover < pts.length ? pts[S.hover] : pts[pts.length - 1];
    if (S.hover >= 0 && S.hover < pts.length) { var hx = Math.round(X(S.hover)) + 0.5; ctx.strokeStyle = C('--faint'); ctx.setLineDash([3, 3]); ctx.beginPath(); ctx.moveTo(hx, padT); ctx.lineTo(hx, padT + ph); ctx.stroke(); ctx.setLineDash([]); }
    var tk = h.dp ? n(h.dp.total_tokens || (h.dp.tokens && h.dp.tokens.total)) : null, px = h.dp && h.dp.price;
    $('#readout').innerHTML = '<span>' + esc(dLabel(h.k)) + ' UTC</span><span>7D <b>' + usd(h.w) + '</b></span><span>Day <b>' + usd(h.d) + '</b></span><span>Tokens <b>' + tok(tk) + '</b></span>' +
      (px ? '<span>In <b>$' + Number(px.p_in_usd_per_m).toFixed(3) + '/M</b></span><span>Out <b>$' + Number(px.p_out_usd_per_m || px.listed_out_usd_per_m).toFixed(2) + '/M</b></span>' : '') +
      (h.wp && h.wp.clamped ? '<span class="dn">clamped: close-only</span>' : '') + (h.dp && h.dp.data_status && h.dp.data_status !== 'OK' ? '<span class="dn">' + esc(h.dp.data_status) + '</span>' : '');
    cv.setAttribute('aria-label', m.name + ' 7-day index ' + usd(m.idx7) + ' per day');
    G = { cw: cw, pw: pw, n: pts.length };
  }
  function drawUsage(m, W, H, C) {
    var u = (USAGE[m.slug] || []).slice(-S.range);
    if (!u.length) { $('#readout').textContent = 'Not in the documented top-50 dataset on any day.'; return; }
    var vals = u.map(function (p) { return p[1]; }), hi = Math.max.apply(null, vals) * 1.08, lo = 0;
    var padR = 76, padB = 24, padT = 24, pw = W - padR, ph = H - padB - padT, cw = pw / u.length;
    var X = function (i) { return i * cw + cw / 2; }, Y = function (v) { return padT + (hi - v) / (hi - lo) * ph; };
    ctx.font = '400 10.5px "JetBrains Mono", ui-monospace, monospace'; ctx.textBaseline = 'middle'; ctx.textAlign = 'left';
    for (var k = 0; k <= 4; k++) { var v = hi * k / 4, yy = Math.round(Y(v)) + 0.5; ctx.strokeStyle = C('--grid'); ctx.beginPath(); ctx.moveTo(0, yy); ctx.lineTo(pw, yy); ctx.stroke(); ctx.fillStyle = C('--faint'); ctx.fillText(tok(v), pw + 8, yy); }
    ctx.textAlign = 'center'; var last = -1e9;
    u.forEach(function (p, i) { if (X(i) - last > 70 && X(i) > 28 && X(i) < pw - 20) { ctx.fillStyle = C('--faint'); ctx.fillText(dLabel(p[0]), X(i), H - padB / 2); last = X(i); } });
    ctx.fillStyle = C('--long'); ctx.globalAlpha = 0.35;
    u.forEach(function (p, i) { var bw = Math.max(1, cw * 0.7); ctx.fillRect(X(i) - bw / 2, Y(p[1]), bw, padT + ph - Y(p[1])); });
    ctx.globalAlpha = 1;
    var h = S.hover >= 0 && S.hover < u.length ? u[S.hover] : u[u.length - 1];
    $('#readout').innerHTML = '<span>' + esc(dLabel(h[0])) + ' UTC</span><span>Tokens <b>' + tok(h[1]) + '</b></span><span class="fnt">OpenRouter documented dataset · ' + u.length + ' reported days</span>';
    G = { cw: cw, pw: pw, n: u.length };
  }
  var box = $('#chartBox');
  box.addEventListener('pointermove', function (e) { if (!G) return; var x = e.clientX - box.getBoundingClientRect().left; S.hover = x < G.pw ? Math.floor(x / G.cw) : -1; draw(); });
  box.addEventListener('pointerleave', function () { S.hover = -1; draw(); });
  if (window.ResizeObserver) new ResizeObserver(function () { draw(); }).observe(box); else window.addEventListener('resize', draw);

  /* ---------- clocks, theme, top level ---------- */
  function clocks() {
    var now = Date.now(), ni = Math.ceil((now + 1) / DAY) * DAY, t = hms(ni - now);
    var a = $('#sIdxCd'); if (a) a.textContent = t;
    a = $('#idxCd'); if (a) a.textContent = t;
  }
  setInterval(clocks, 1000);
  $('#themeBtn').addEventListener('click', function () {
    var r = document.documentElement, t = r.getAttribute('data-theme');
    var dark = t ? t === 'dark' : !(window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches);
    var nx = dark ? 'light' : 'dark'; r.setAttribute('data-theme', nx); try { localStorage.setItem('zf-theme', nx); } catch (e) {} draw();
  });
  function renderTrade() { renderTick(); renderWatch(); renderSide(); renderTicket(); renderAcct(); renderChartCtl(); }
  function render() {
    var b = S.view === 'board', lb = S.view === 'labs', t = S.view === 'trade';
    $('#vBoard').hidden = !b; $('#vTrade').hidden = !t; $('#vLabs').hidden = !lb;
    $('#navBoard').setAttribute('aria-current', b ? 'page' : 'false'); $('#navTrade').setAttribute('aria-current', t ? 'page' : 'false'); $('#navLabs').setAttribute('aria-current', lb ? 'page' : 'false');
    if (b) renderBoard(); else if (lb) renderLabs(); else { renderTrade(); draw(); }
    clocks();
  }
  /* ---------- labs: tokens per day by lab since 2025 ---------- */
  var LS = { range: 9999, mode: 'share', hover: -1 }, LG = null;
  var LAB_COLORS = ['#5CF0AE', '#7AA2FF', '#FFB347', '#FF8A65', '#C792EA', '#4DD0E1', '#F06292', '#AED581', '#FFD54F', '#90A4AE', '#555555'];
  function labsData() {
    if (LABS) return Promise.resolve(LABS);
    return getJSON('/v1/datasets/rankings-daily/labs.json').then(function (d) { LABS = d; return d; });
  }
  function smooth(a) { return a.map(function (_, i) { var s = 0, k = 0; for (var j = Math.max(0, i - 6); j <= i; j++) { s += a[j]; k++; } return s / k; }); }
  function renderLabs() {
    $('#labRange').innerHTML = [[90, '90D'], [365, '1Y'], [9999, 'All']].map(function (t) { return '<button type="button" class="chip" data-lr="' + t[0] + '" aria-pressed="' + (LS.range === t[0]) + '">' + t[1] + '</button>'; }).join('');
    $('#labMode').innerHTML = [['share', 'Share'], ['tokens', 'Tokens']].map(function (t) { return '<button type="button" class="chip" data-lm="' + t[0] + '" aria-pressed="' + (LS.mode === t[0]) + '">' + t[1] + '</button>'; }).join('');
    labsData().then(function (d) {
      var names = Object.keys(d.labs), n = d.dates.length;
      var sum = function (k, a, b) { var s = 0; for (var i = Math.max(0, a); i < b; i++) s += d.labs[k][i]; return s; };
      var tot30 = names.reduce(function (a, k) { return a + sum(k, n - 30, n); }, 0), totP = names.reduce(function (a, k) { return a + sum(k, n - 60, n - 30); }, 0);
      $('#labTable').innerHTML = names.map(function (k, i) {
        var sh = sum(k, n - 30, n) / tot30 * 100, pv = sum(k, n - 60, n - 30) / totP * 100;
        return '<div class="lab-row"><span><span class="sw" style="background:' + LAB_COLORS[i % LAB_COLORS.length] + '"></span>' + esc(k) + '</span><span class="m">' + sh.toFixed(1) + '%</span><span class="m ' + cls(sh - pv) + '">' + (sh - pv >= 0 ? '+' : '−') + Math.abs(sh - pv).toFixed(1) + '</span></div>';
      }).join('');
      drawLabs();
    }).catch(function (e) { $('#labReadout').textContent = e.message; });
  }
  $('#labRange').addEventListener('click', function (e) { var b = e.target.closest('[data-lr]'); if (b) { LS.range = +b.getAttribute('data-lr'); renderLabs(); } });
  $('#labMode').addEventListener('click', function (e) { var b = e.target.closest('[data-lm]'); if (b) { LS.mode = b.getAttribute('data-lm'); renderLabs(); } });
  var lcv = $('#labChart'), lctx = lcv.getContext('2d');
  function drawLabs() {
    if (S.view !== 'labs' || !LABS) return;
    var d = LABS, box = $('#labBox'), W = box.clientWidth, H = box.clientHeight; if (!W || !H) return;
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    if (lcv.width !== Math.round(W * dpr) || lcv.height !== Math.round(H * dpr)) { lcv.width = Math.round(W * dpr); lcv.height = Math.round(H * dpr); }
    lctx.setTransform(dpr, 0, 0, dpr, 0, 0); lctx.clearRect(0, 0, W, H);
    var cs = getComputedStyle(document.documentElement), C = function (k) { return cs.getPropertyValue(k).trim(); };
    var names = Object.keys(d.labs), n = d.dates.length, start = Math.max(0, n - LS.range), dates = d.dates.slice(start);
    var sm = {}; names.forEach(function (k) { sm[k] = smooth(d.labs[k]).slice(start); });
    var tot = dates.map(function (_, i) { return names.reduce(function (a, k) { return a + sm[k][i]; }, 0); });
    var padR = 70, padB = 24, padT = 20, pw = W - padR, ph = H - padB - padT, cw = pw / Math.max(1, dates.length - 1);
    var X = function (i) { return i * cw; }, hi = LS.mode === 'share' ? 1 : Math.max.apply(null, tot) * 1.05, Y = function (v) { return padT + (1 - v / hi) * ph; };
    lctx.font = '400 10.5px "JetBrains Mono", ui-monospace, monospace'; lctx.textBaseline = 'middle';
    for (var k = 0; k <= 4; k++) { var v = hi * k / 4, yy = Math.round(Y(v)) + 0.5; lctx.strokeStyle = C('--grid'); lctx.beginPath(); lctx.moveTo(0, yy); lctx.lineTo(pw, yy); lctx.stroke(); lctx.fillStyle = C('--faint'); lctx.textAlign = 'left'; lctx.fillText(LS.mode === 'share' ? Math.round(v * 100) + '%' : tok(v), pw + 8, yy); }
    var base = dates.map(function () { return 0; });
    names.slice().reverse().forEach(function (k) {
      var i0 = names.indexOf(k), col = LAB_COLORS[i0 % LAB_COLORS.length];
      var top = base.map(function (b, i) { return b + (LS.mode === 'share' ? sm[k][i] / (tot[i] || 1) : sm[k][i]); });
      lctx.beginPath(); top.forEach(function (v, i) { if (i) lctx.lineTo(X(i), Y(v)); else lctx.moveTo(X(i), Y(v)); });
      for (var i = base.length - 1; i >= 0; i--) lctx.lineTo(X(i), Y(base[i]));
      lctx.closePath(); lctx.fillStyle = col; lctx.globalAlpha = 0.78; lctx.fill(); lctx.globalAlpha = 1;
      base = top;
    });
    lctx.textAlign = 'center'; var last = -1e9;
    dates.forEach(function (dt, i) { if (X(i) - last > 80 && X(i) > 30 && X(i) < pw - 30) { lctx.fillStyle = C('--faint'); lctx.fillText(dLabel(dt) + (dates.length > 200 ? ' ' + dt.slice(2, 4) : ''), X(i), H - padB / 2); last = X(i); } });
    var hi2 = LS.hover >= 0 && LS.hover < dates.length ? LS.hover : dates.length - 1;
    if (LS.hover >= 0) { lctx.strokeStyle = C('--ink'); lctx.setLineDash([3, 3]); lctx.beginPath(); lctx.moveTo(X(hi2), padT); lctx.lineTo(X(hi2), padT + ph); lctx.stroke(); lctx.setLineDash([]); }
    var lead = names.filter(function (k) { return k !== 'everything else'; }).sort(function (a, b) { return sm[b][hi2] - sm[a][hi2]; }).slice(0, 4);
    $('#labReadout').innerHTML = '<span>' + esc(dLabel(dates[hi2])) + ' ' + esc(dates[hi2].slice(0, 4)) + '</span><span>All <b>' + tok(tot[hi2]) + '/day</b></span>' + lead.map(function (k) { return '<span>' + esc(k) + ' <b>' + (sm[k][hi2] / tot[hi2] * 100).toFixed(1) + '%</b></span>'; }).join('');
    LG = { cw: cw, pw: pw, n: dates.length };
  }
  var lbox = $('#labBox');
  lbox.addEventListener('pointermove', function (e) { if (!LG) return; var x = e.clientX - lbox.getBoundingClientRect().left; LS.hover = x < LG.pw ? Math.round(x / LG.cw) : -1; drawLabs(); });
  lbox.addEventListener('pointerleave', function () { LS.hover = -1; drawLabs(); });
  if (window.ResizeObserver) new ResizeObserver(function () { drawLabs(); }).observe(lbox);

  window.ZfxApp = { markets: function () { return M; }, rerender: render };
  load().then(function () {
    fromHash(); go(S.view);
    var saved = Q.get('venue'); if (!saved) { try { saved = localStorage.getItem('zfx-venue'); } catch (e) {} }
    if (saved) connectVenue(saved);
  }).catch(function (e) {
    $('#lbBody').innerHTML = '<tr><td colspan="11" class="l dn" style="padding:18px 14px">Could not load the index: ' + esc(e.message) + '</td></tr>';
  });
})();
