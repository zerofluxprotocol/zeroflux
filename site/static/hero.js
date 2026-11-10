/* Hero: star funnel. Stars stream along invisible converging curves through one pinch point. Colors come from the theme tokens. */
(function () {
  var cvs = document.getElementById('heroCanvas'); if (!cvs) return; var ctx = cvs.getContext('2d');
  var reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var W = 0, H = 0, DPR = 1, cx = 0, cy = 0, curves = [], parts = [], sprite = null, blend = 'lighter', acc = '92,240,174', last = performance.now(), running = true;
  var P = 2.3;

  function makeStar(fill, glow) {
    var s = 128, sh = document.createElement('canvas'); sh.width = sh.height = s;
    var g = sh.getContext('2d'), m = s / 2, L = s * 0.42, wb = s * 0.07, wt = s * 0.016;
    g.fillStyle = fill;
    for (var a = 0; a < 4; a++) {
      var ang = a * Math.PI / 2, ca = Math.cos(ang), sa = Math.sin(ang), px = -sa, py = ca, tx = m + ca * L, ty = m + sa * L;
      g.beginPath(); g.moveTo(m + px * wb, m + py * wb); g.lineTo(tx + px * wt, ty + py * wt);
      g.arc(tx, ty, wt, ang + Math.PI / 2, ang - Math.PI / 2, true); g.lineTo(m - px * wb, m - py * wb); g.closePath(); g.fill();
    }
    var r = s * 0.24, k = s * 0.075;
    g.beginPath(); g.moveTo(m, m - r); g.quadraticCurveTo(m + k, m - k, m + r, m); g.quadraticCurveTo(m + k, m + k, m, m + r);
    g.quadraticCurveTo(m - k, m + k, m - r, m); g.quadraticCurveTo(m - k, m - k, m, m - r); g.closePath(); g.fill();
    var c = document.createElement('canvas'); c.width = c.height = s;
    var o = c.getContext('2d'); o.shadowColor = glow; o.shadowBlur = 10; o.drawImage(sh, 0, 0); o.shadowBlur = 0; o.globalAlpha = .4; o.drawImage(sh, 0, 0);
    return c;
  }
  function theme() {
    var cs = getComputedStyle(document.documentElement), v = function (n) { return cs.getPropertyValue(n).trim(); };
    blend = v('--star-blend') || 'lighter'; acc = v('--acc') || acc;
    sprite = makeStar(v('--star') || '#5CF5A8', v('--star-glow') || 'rgba(80,240,160,0.8)');
  }
  function yAt(c, x) { var u = Math.abs(x - cx) / (W / 2); return cy + c.sign * c.amp * Math.pow(u, P); }
  function pick() { var f = Math.pow(Math.random(), 1.15); return curves[Math.min(curves.length - 1, Math.floor(f * (curves.length - 1)))]; }
  function spawn(anywhere) {
    return { dir: Math.random() < 0.5 ? 1 : -1, a: pick(), b: pick(), t: anywhere ? Math.random() : 0, sp: 0.028 + Math.random() * 0.032, size: 7 + Math.random() * 11, ph: Math.random() * 6.28, alpha: 0.55 + Math.random() * 0.45 };
  }
  function build() {
    var r = cvs.getBoundingClientRect();
    W = r.width; H = r.height; DPR = Math.min(window.devicePixelRatio || 1, 2);
    cvs.width = Math.round(W * DPR); cvs.height = Math.round(H * DPR); ctx.setTransform(DPR, 0, 0, DPR, 0, 0);
    cx = W / 2; var pe = document.querySelector('.hero-inner'), cr = cvs.getBoundingClientRect(); if (pe) { var pr = pe.getBoundingClientRect(); cy = (pr.top + pr.height / 2 - cr.top) * (H / cr.height); } else { cy = H * 0.6; }
    curves = [];
    for (var i = 0; i < 46; i++) { var f = (i + 1) / 46, amp = H * 1.7 * Math.pow(f, 1.35); curves.push({ amp: amp, sign: -1 }); curves.push({ amp: amp * 0.9, sign: 1 }); }
    curves.push({ amp: 0, sign: 1 });
    parts = []; var count = Math.round(Math.min(760, W * (W < 640 ? 0.28 : 0.5)));
    for (var k = 0; k < count; k++) parts.push(spawn(true));
  }
  function frame(now) {
    var dt = Math.min(0.05, (now - last) / 1000); last = now;
    if (running || reduce) {
      ctx.clearRect(0, 0, W, H);
      var t = now / 1000, glowA = (blend === 'lighter' ? 0.22 : 0.08) + 0.06 * Math.sin(t * 2);
      var core = ctx.createRadialGradient(cx, cy, 0, cx, cy, 40);
      core.addColorStop(0, 'rgba(' + acc + ',' + glowA + ')'); core.addColorStop(1, 'rgba(' + acc + ',0)');
      ctx.fillStyle = core; ctx.fillRect(cx - 40, cy - 40, 80, 80);
      ctx.globalCompositeOperation = blend;
      for (var i = 0; i < parts.length; i++) {
        var p = parts[i], x = p.dir > 0 ? p.t * W : W - p.t * W, u = Math.abs(x - cx) / (W / 2);
        if (!reduce) p.t += dt * p.sp * (0.55 + 1.6 * Math.pow(1 - u, 2));
        if (p.t > 1) { parts[i] = spawn(false); continue; }
        var before = p.dir > 0 ? x < cx : x > cx, y = yAt(before ? p.a : p.b, x);
        if (y < -20 || y > H + 20) continue;
        var tw = 0.65 + 0.35 * Math.sin(t * 2 + p.ph), s = p.size * (0.35 + 0.65 * u);
        ctx.globalAlpha = Math.min(1, p.alpha * tw * (0.5 + 0.8 * (1 - u)));
        ctx.drawImage(sprite, x - s / 2, y - s / 2, s, s);
      }
      ctx.globalAlpha = 1; ctx.globalCompositeOperation = 'source-over';
    }
    if (!reduce) requestAnimationFrame(frame);
  }
  theme(); build();
  window.addEventListener('zf-theme', function () { theme(); if (reduce) frame(performance.now()); });
  var to; window.addEventListener('resize', function () { clearTimeout(to); to = setTimeout(function () { build(); if (reduce) frame(performance.now()); }, 150); });
  if ('IntersectionObserver' in window) new IntersectionObserver(function (e) { running = e[0].isIntersecting; }).observe(cvs);
  if (reduce) frame(performance.now()); else requestAnimationFrame(frame);
})();
