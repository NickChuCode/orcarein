/* OrcaRein landing — vanilla-JS port of the Claude Design component.
   Faithful reproduction of the original DCLogic component: pixel-orca canvas,
   snowfall, terminal demo, vim keybindings, language toggle, copy buttons,
   help overlay and the statusline. No framework, no build step. */
(function () {
  'use strict';

  // props (from the design's data-props defaults)
  var PROPS = { crtScanlines: true, edgeOrca: true, demoSpeed: 1 };

  var state = {
    lang: 'zh', mode: 'NORMAL', pct: 'Top', sect: 'hero', help: false,
    lines: [], typing: '', tmode: 'NORMAL', usage: 'ctx 0%', copied: ''
  };

  var MAP = [
    "...............KK...................",
    "...............KKK..................",
    "..............KKKK..................",
    "..............KKKKK.................",
    ".............KKKKKKK................",
    ".KKK.....KKKKKKKKKKKKKKKKKK.........",
    ".KKKK..KKKKKKTKKKGGGKKKKKKWWWWK.....",
    "..KKKKKKKKKKKKKKKKKKKKKKKKWWWWWKK...",
    "...KKKKKKKKKKKKKKKKKKKKKKKKWWWKKKKK.",
    "...KKKKKKKKKKKKKKKKKKKKKKKKKKKKKKKKK",
    ".KKK.KKKKKKKKKKKKKKKKKKKKKKWWWWWWWK.",
    ".KK...KKKKKKWWWWWWWKKKKKKKWWWWWWWK..",
    "........KKWWWWWWWWWWWWWWWWWWWWKK....",
    "...........WWWWWWKKKWWWWWWWW........",
    ".................KKK................",
    "..................KK................"
  ];
  var COL = { K: '#33437A', W: '#EAF0FA', G: '#8FA3C8', T: '#2BD4C4', R: '#4E5F9E' };

  var $ = function (id) { return document.getElementById(id); };

  // DOM refs (resolved on init)
  var page, hOrca, hPar, sOrca, swWrap, snA, snB, snC,
      demoEl, typingEl, tcurEl, tModeEl, tUsageEl,
      sModeEl, sFileEl, sSectEl, sPctEl,
      navLang, statusLang, langNextEl, helpBtn, helpOverlay;

  var raf, demoTimer, swTimer, swimFlip = false, typed = null, lg = 0, scrollTick = false;

  function spd() { return PROPS.demoSpeed || 1; }

  // --- pixel orca ---
  function draw(cv, px, t, flip) {
    var rows = MAP.length, cols = MAP[0].length;
    var W = cols * px, H = (rows + 3) * px;
    if (cv.width !== W) { cv.width = W; cv.height = H; cv.style.width = W + 'px'; cv.style.height = H + 'px'; }
    var ctx = cv.getContext('2d');
    ctx.clearRect(0, 0, W, H);
    var teal = [];
    for (var y = 0; y < rows; y++) for (var x = 0; x < cols; x++) {
      var ch = MAP[y][x]; if (ch === '.') continue;
      var dy = x < 7 ? Math.round(Math.sin(t * 2.2 + (7 - x) * 0.55) * (7 - x) * 0.16) : 0;
      var dx = flip ? cols - 1 - x : x;
      var Y = Math.round((y + 1.5 + dy) * px);
      if (ch === 'T') { teal.push([dx * px, Y]); continue; }
      var col = COL[ch];
      if (ch === 'K') {
        var nb = function (yy, xx) { return yy < 0 || yy >= rows || xx < 0 || xx >= cols || MAP[yy][xx] === '.'; };
        if (nb(y - 1, x)) col = '#7A94E8';
        else if (nb(y + 1, x) || nb(y, x - 1) || nb(y, x + 1)) col = '#4E5F9E';
      }
      ctx.fillStyle = col;
      ctx.fillRect(dx * px, Y, px, px);
    }
    ctx.shadowColor = '#2BD4C4'; ctx.shadowBlur = px * 1.2; ctx.fillStyle = COL.T;
    for (var i = 0; i < teal.length; i++) ctx.fillRect(teal[i][0], teal[i][1], px, px);
    ctx.shadowBlur = 0;
  }

  // --- snow ---
  function genSnow() {
    var mk = function (el, n, s, dur, op, blur, pal) {
      if (!el) return;
      var a = [];
      for (var i = 0; i < n; i++) {
        var x = Math.round(Math.random() * 1900), y = Math.round(Math.random() * 1600), c = pal[i % pal.length];
        a.push(x + 'px ' + y + 'px 0 ' + s + 'px ' + c, x + 'px ' + (y + 1600) + 'px 0 ' + s + 'px ' + c);
      }
      el.style.boxShadow = a.join(',');
      el.style.animation = 'rise ' + dur + 's linear infinite';
      el.style.opacity = op;
      if (blur) el.style.filter = 'blur(' + blur + 'px)';
    };
    mk(snA, 36, 0, 130, .45, 0, ['rgba(198,208,224,.55)', 'rgba(43,212,196,.35)', 'rgba(77,107,254,.35)']);
    mk(snB, 20, 1, 75, .4, 0, ['rgba(198,208,224,.4)', 'rgba(43,212,196,.3)']);
    mk(snC, 9, 2, 42, .3, 2, ['rgba(234,240,250,.35)']);
  }

  // --- edge-swimming orca ---
  function swim() {
    var on = PROPS.edgeOrca;
    if (on && swWrap) {
      var dir = Math.random() < 0.5 ? 1 : -1;
      swimFlip = dir < 0;
      swWrap.style.bottom = Math.round(38 + Math.random() * 70) + 'px';
      var from = dir > 0 ? '-140px' : (window.innerWidth + 140) + 'px';
      var to = dir > 0 ? (window.innerWidth + 140) + 'px' : '-140px';
      var an = swWrap.animate([{ left: from }, { left: to }], { duration: 24000, easing: 'linear' });
      an.onfinish = function () { swTimer = setTimeout(swim, (20 + Math.random() * 26) * 1000); };
    } else {
      swTimer = setTimeout(swim, 8000);
    }
  }

  // --- terminal demo script ---
  function steps() {
    return [
      { a: 'reset', d: 300 },
      { a: 'l', c: 'dim', zh: 'OrcaRein — /help 查看命令，Ctrl+D 退出。', en: 'OrcaRein — /help for commands, Ctrl+D to quit.', d: 500 },
      { a: 'l', c: 'dim', zh: 'provider deepseek · 8 tools + mcp · session 已恢复', en: 'provider deepseek · 8 tools + mcp · session resumed', d: 900 },
      { a: 'tm', m: 'INSERT', d: 420 },
      { a: 'ty', zh: '跑一下测试，失败的贴出来', en: 'run the tests, paste any failures', d: 62 },
      { a: 'tm', m: 'NORMAL', d: 620 },
      { a: 'sub', d: 400 },
      { a: 'l', c: 'dim', zh: '[思考] cargo test 要走 bash —— risky 工具，先请求许可。', en: '[思考] cargo test needs bash — a risky tool. Asking first.', d: 1100 },
      { a: 'l', c: 'fg', zh: '[tool: bash({"cmd":"cargo test --workspace"})]', en: '[tool: bash({"cmd":"cargo test --workspace"})]', d: 900 },
      { a: 'perm', d: 1600 },
      { a: 'permY', d: 800 },
      { a: 'l', c: 'ok', zh: '[result] test result: ok. 360 passed; 0 failed', en: '[result] test result: ok. 360 passed; 0 failed', d: 1000 },
      { a: 'st', zh: '全部通过：360 个测试，0 失败，没有要修的。要我顺手跑一下 clippy 吗？', en: 'All green: 360 passed, 0 failed. Nothing to fix. Want me to run clippy while we are here?', d: 42 },
      { a: 'l', c: 'dim', zh: '✓ auto-saved · ctx 3% · 2.1k tokens', en: '✓ auto-saved · ctx 3% · 2.1k tokens', d: 4200 },
      { a: 'loop' }
    ];
  }

  function renderDemo() {
    var zh = state.lang === 'zh';
    // rebuild demo lines
    while (demoEl.firstChild) demoEl.removeChild(demoEl.firstChild);
    for (var i = 0; i < state.lines.length; i++) {
      var ln = state.lines[i];
      var div = document.createElement('div');
      div.className = 'tline';
      for (var j = 0; j < ln.s.length; j++) {
        var sg = ln.s[j];
        var sp = document.createElement('span');
        sp.className = sg.c;
        sp.textContent = zh ? sg.zh : sg.en;
        div.appendChild(sp);
      }
      demoEl.appendChild(div);
    }
    typingEl.textContent = state.typing;
    tcurEl.className = 'tcur' + (state.tmode === 'INSERT' ? ' tcurI' : '');
    tModeEl.textContent = '-- ' + state.tmode + ' --';
    tModeEl.className = 'mchip' + (state.tmode === 'INSERT' ? ' mIns' : '');
    tUsageEl.textContent = state.usage;
  }

  function push(ln) { state.lines.push(ln); renderDemo(); }
  function mutLast(si, zh, en) {
    var last = state.lines[state.lines.length - 1];
    var seg = last.s[si];
    last.s[si] = { c: seg.c, zh: zh, en: en };
    renderDemo();
  }

  function run(i) {
    var S = steps();
    if (i >= S.length) return;
    var s = S[i];
    var nx = function (d) { demoTimer = setTimeout(function () { run(i + 1); }, d / spd()); };
    if (s.a === 'reset') { state.lines = []; state.typing = ''; state.tmode = 'NORMAL'; state.usage = 'ctx 0%'; renderDemo(); nx(s.d); }
    else if (s.a === 'l') { push({ s: [{ c: s.c, zh: s.zh, en: s.en }] }); nx(s.d); }
    else if (s.a === 'tm') { state.tmode = s.m; renderDemo(); nx(s.d); }
    else if (s.a === 'ty') { typed = s; doType(s, 1, function () { nx(0); }); }
    else if (s.a === 'sub') {
      var t = typed || { zh: '', en: '' };
      push({ s: [{ c: 'brand', zh: '> ', en: '> ' }, { c: 'wt', zh: t.zh, en: t.en }] });
      state.typing = ''; renderDemo(); nx(s.d);
    }
    else if (s.a === 'perm') { push({ s: [{ c: 'warn', zh: '允许 bash?  ', en: 'Allow bash?  ' }, { c: 'dim', zh: '[y/n/A/N] ', en: '[y/n/A/N] ' }, { c: 'ok', zh: '', en: '' }] }); nx(s.d); }
    else if (s.a === 'permY') { mutLast(2, 'y', 'y'); nx(s.d); }
    else if (s.a === 'st') {
      push({ s: [{ c: 'brand', zh: '[回复] ', en: '[回复] ' }, { c: 'fg', zh: '', en: '' }] });
      doStream(s, 1, function () { state.usage = 'ctx 3% · 2.1k tok'; renderDemo(); nx(600); });
    }
    else if (s.a === 'loop') { demoTimer = setTimeout(function () { run(0); }, 500); }
  }

  function doType(s, i, done) {
    var txt = state.lang === 'zh' ? s.zh : s.en;
    if (i > txt.length) { done(); return; }
    state.typing = txt.slice(0, i); renderDemo();
    demoTimer = setTimeout(function () { doType(s, i + 1, done); }, s.d / spd());
  }
  function doStream(s, i, done) {
    var txt = state.lang === 'zh' ? s.zh : s.en;
    if (i > txt.length) { mutLast(1, s.zh, s.en); done(); return; }
    var part = txt.slice(0, i);
    mutLast(1, part, part);
    demoTimer = setTimeout(function () { doStream(s, i + 1, done); }, s.d / spd());
  }
  function restartDemo() {
    clearTimeout(demoTimer);
    state.lines = []; state.typing = ''; renderDemo();
    demoTimer = setTimeout(function () { run(0); }, 300);
  }

  // --- statusline / chrome ---
  var SN = { hero: ['首页', 'home'], cap: ['特色', 'why'], tools: ['工具', 'tools'], install: ['安装', 'install'], roadmap: ['航线', 'roadmap'], honest: ['直说', 'plain talk'] };

  function renderChrome() {
    var zh = state.lang === 'zh';
    page.className = 'page L-' + state.lang;
    navLang.textContent = zh ? 'EN' : '中文';
    langNextEl.textContent = zh ? 'en' : 'zh';
    sModeEl.textContent = '-- ' + state.mode + ' --';
    sModeEl.className = 'schip' + (state.mode === 'INSERT' ? ' mIns' : '');
    sFileEl.textContent = zh ? '~/orcarein/首页.zh.md [+]' : '~/orcarein/index.en.md [+]';
    sSectEl.textContent = '§ ' + (SN[state.sect] ? SN[state.sect][zh ? 0 : 1] : state.sect);
    sPctEl.textContent = state.pct;
    helpOverlay.style.display = state.help ? 'flex' : 'none';
  }

  function toggleLang() {
    var nl = state.lang === 'zh' ? 'en' : 'zh';
    try { localStorage.setItem('orcarein_lang', nl); } catch (e) {}
    state.lang = nl; renderChrome(); restartDemo();
  }

  function onCopy(e) {
    var b = e.currentTarget;
    try { navigator.clipboard.writeText(b.dataset.c); } catch (err) {}
    b.textContent = state.lang === 'zh' ? '已复制' : 'copied';
    setTimeout(function () { b.textContent = 'copy'; }, 1400);
  }

  // --- keyboard (vim-ish) ---
  function onKey(e) {
    if (e.metaKey || e.altKey) return;
    var k = e.key;
    if (e.ctrlKey) {
      if (k === 'd' || k === 'u') { e.preventDefault(); window.scrollBy({ top: (k === 'd' ? 1 : -1) * window.innerHeight / 2, behavior: 'smooth' }); }
      return;
    }
    if (k === '?') { state.help = !state.help; renderChrome(); return; }
    if (k === 'Escape') { state.help = false; state.mode = 'NORMAL'; renderChrome(); return; }
    if (k === 'i') { state.mode = 'INSERT'; renderChrome(); return; }
    if (k === 'j') { window.scrollBy({ top: 120 }); return; }
    if (k === 'k') { window.scrollBy({ top: -120 }); return; }
    if (k === 'G') { window.scrollTo({ top: document.documentElement.scrollHeight, behavior: 'smooth' }); return; }
    if (k === 'g') {
      var now = Date.now();
      if (lg && now - lg < 450) { window.scrollTo({ top: 0, behavior: 'smooth' }); lg = 0; }
      else lg = now;
    }
  }

  function onScroll() {
    if (scrollTick) return; scrollTick = true;
    requestAnimationFrame(function () {
      scrollTick = false;
      var d = document.documentElement;
      var max = d.scrollHeight - window.innerHeight;
      var y = window.scrollY;
      var pct = y < 40 ? 'Top' : (y >= max - 40 ? 'Bot' : Math.round(y / max * 100) + '%');
      var sect = 'hero';
      document.querySelectorAll('[data-sec]').forEach(function (el) { if (el.getBoundingClientRect().top <= 140) sect = el.getAttribute('data-sec'); });
      if (pct !== state.pct || sect !== state.sect) { state.pct = pct; state.sect = sect; renderChrome(); }
    });
  }

  function init() {
    page = $('page'); hOrca = $('orcaHero'); hPar = $('orcaPar'); sOrca = $('orcaSwim');
    swWrap = $('swimWrap'); snA = $('snowA'); snB = $('snowB'); snC = $('snowC');
    demoEl = $('demoLines'); typingEl = $('typingTxt'); tcurEl = $('tcur');
    tModeEl = $('tMode'); tUsageEl = $('tUsage');
    sModeEl = $('sMode'); sFileEl = $('sFile'); sSectEl = $('sSect'); sPctEl = $('sPct');
    navLang = $('navLang'); statusLang = $('statusLang'); langNextEl = $('langNext');
    helpBtn = $('helpBtn'); helpOverlay = $('helpOverlay');

    try { var l = localStorage.getItem('orcarein_lang'); if (l === 'en' || l === 'zh') state.lang = l; } catch (e) {}

    window.addEventListener('keydown', onKey);
    window.addEventListener('scroll', onScroll, { passive: true });
    genSnow();
    if (!PROPS.crtScanlines) { var sc = document.querySelector('.scan'); if (sc) sc.style.display = 'none'; }

    var loop = function () {
      var t = performance.now() / 1000;
      if (hOrca) draw(hOrca, 8, t, false);
      if (sOrca) draw(sOrca, 3, t * 1.5, swimFlip);
      if (hPar) hPar.style.transform = 'translateY(' + (window.scrollY * -0.16) + 'px)';
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);

    demoTimer = setTimeout(function () { run(0); }, 600);
    swTimer = setTimeout(swim, 6000);

    navLang.addEventListener('click', toggleLang);
    statusLang.addEventListener('click', toggleLang);
    helpBtn.addEventListener('click', function () { state.help = !state.help; renderChrome(); });
    helpOverlay.addEventListener('click', function () { state.help = false; renderChrome(); });
    var cbs = document.querySelectorAll('.js-copy');
    for (var i = 0; i < cbs.length; i++) cbs[i].addEventListener('click', onCopy);

    renderChrome();
    renderDemo();
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
