/* header state, theme toggle, copy buttons, chart */
(function () {
  var h = document.getElementById('top-header');
  function on() { h.classList.toggle('scrolled', window.scrollY > 40); }
  window.addEventListener('scroll', on, { passive: true }); on();

  var root = document.documentElement, btn = document.getElementById('themeToggle');
  function isDark() { var t = root.getAttribute('data-theme'); if (t) return t === 'dark'; return !(window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches); }
  function sync() { btn.setAttribute('aria-pressed', isDark() ? 'true' : 'false'); window.dispatchEvent(new Event('zf-theme')); }
  btn.addEventListener('click', function () {
    var next = isDark() ? 'light' : 'dark';
    root.setAttribute('data-theme', next);
    try { localStorage.setItem('zf-theme', next); } catch (e) {}
    sync();
  });
  if (window.matchMedia) window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', sync);
  btn.setAttribute('aria-pressed', isDark() ? 'true' : 'false');

  document.querySelectorAll('.copy').forEach(function (b) {
    b.addEventListener('click', function () {
      var t = document.getElementById(b.getAttribute('data-copy')).innerText;
      navigator.clipboard.writeText(t).then(function () { b.textContent = 'Copied'; setTimeout(function () { b.textContent = 'Copy'; }, 1400); }).catch(function () {});
    });
  });

  var demand = [32,26,22,20,22,30,44,58,70,78,84,88,86,82,80,84,88,90,80,68,56,48,40,36];
  var el = document.getElementById('bars');
  if (el) demand.forEach(function (d) {
    var b = document.createElement('div'); b.className = 'bar';
    b.innerHTML = '<i class="zf" style="flex:' + (100 - d) + ' 1 0"></i><i class="inf" style="flex:' + d + ' 1 0"></i>';
    el.appendChild(b);
  });
})();
(function () { var h = document.getElementById('top-header'), b = document.getElementById('menuToggle'); if (!b) return;
function set(o) { h.classList.toggle('open', o); b.setAttribute('aria-expanded', o ? 'true' : 'false'); b.setAttribute('aria-label', o ? 'Close menu' : 'Open menu'); }
b.addEventListener('click', function () { set(!h.classList.contains('open')); });
document.querySelectorAll('#mnav a').forEach(function (a) { a.addEventListener('click', function () { set(false); }); });
document.addEventListener('keydown', function (e) { if (e.key === 'Escape') set(false); }); })();
/* private beta form: posts to /api/beta, shows the result inline */
(function () {
  var f = document.getElementById('betaForm'); if (!f) return;
  var msg = document.getElementById('betaMsg'), btn = f.querySelector('button[type=submit]');
  function say(t, cls) { msg.textContent = t; msg.className = 'bmsg' + (cls ? ' ' + cls : ''); }
  f.addEventListener('submit', function (e) {
    e.preventDefault();
    var email = f.email.value.trim();
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) { say('Enter a valid email address.', 'err'); f.email.focus(); return; }
    btn.disabled = true; say('Sending...');
    fetch('/api/beta', { method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ email: email, note: f.note.value, website: f.website.value }) })
      .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, j: j }; }); })
      .then(function (r) {
        if (r.ok) { f.reset(); say("You're on the list. We'll email you when your spot opens.", 'ok'); }
        else say(r.j.error || 'Something went wrong. Try again.', 'err');
      })
      .catch(function () { say('Could not reach the server. Try again.', 'err'); })
      .finally(function () { btn.disabled = false; });
  });
})();
