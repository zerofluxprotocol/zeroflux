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
