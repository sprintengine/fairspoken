/* Fairspoken marketing site prototype: behaviour outside the hero. No dependencies. */
(() => {
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const root = document.documentElement;
  const isDark = () => root.dataset.theme === 'dark';
  const cssVar = (el, n) => getComputedStyle(el).getPropertyValue(n).trim();

  /* ---------- Theme ---------- */
  const btn = $('.theme-btn');
  const lock = $('[data-lock]');
  function syncTheme() {
    const dark = isDark();
    btn.innerHTML = `<svg aria-hidden="true"><use href="#i-${dark ? 'sun' : 'moon'}"/></svg>`;
    btn.setAttribute('aria-label', dark ? 'Switch to light appearance' : 'Switch to dark appearance');
    if (lock) lock.src = `../logo/fairspoken-lockup${dark ? '-reversed' : ''}.svg`;
    $('meta[name="theme-color"]')?.setAttribute('content', dark ? '#0A0D12' : '#F3F1EA');
  }
  btn.addEventListener('click', () => {
    root.dataset.theme = isDark() ? 'light' : 'dark';
    localStorage.setItem('fs-theme', root.dataset.theme);
    syncTheme(); redrawAll(); navTone();
  });
  syncTheme();

  /* ---------- Nav adapts to the band beneath it ---------- */
  const nav = $('.nav');
  function navTone() {
    const y = 40;
    const sec = $$('main > section').find((el) => { const r = el.getBoundingClientRect(); return r.top <= y && r.bottom > y; });
    const blue = sec && sec.classList.contains('download');
    const inv = sec && sec.classList.contains('invert');
    nav.classList.toggle('on-blue', !!blue);
    nav.classList.toggle('on-dark', !blue && !!inv && !isDark());
    nav.classList.toggle('on-light', !blue && !!inv && isDark());
  }
  addEventListener('scroll', navTone, { passive: true });
  navTone();

  /* ---------- Split headings into words for the kinetic reveal ---------- */
  $$('.split').forEach((el) => {
    let i = 0;
    const walk = (node) => {
      [...node.childNodes].forEach((n) => {
        if (n.nodeType === 3) {
          const frag = document.createDocumentFragment();
          n.textContent.split(/(\s+)/).forEach((part) => {
            if (!part) return;
            if (/^\s+$/.test(part)) { frag.append(part); return; }
            const wd = document.createElement('span'); wd.className = 'wd';
            const inner = document.createElement('span'); inner.textContent = part; inner.style.setProperty('--i', i++);
            wd.append(inner); frag.append(wd);
          });
          n.replaceWith(frag);
        } else if (n.nodeType === 1) walk(n);
      });
    };
    walk(el);
  });
  const io = new IntersectionObserver((entries) => entries.forEach((e) => { if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); } }), { rootMargin: '0px 0px -12% 0px' });
  $$('.rv, .split, .timeline').forEach((el, k) => { if (el.classList.contains('rv')) el.style.transitionDelay = `${(k % 4) * 70}ms`; io.observe(el); });
  if (reduce || new URLSearchParams(location.search).has('all')) $$('.rv, .split, .timeline').forEach((el) => el.classList.add('in'));

  /* ---------- How it works: scroll-driven three-step demo ---------- */
  const EX = {
    gp: {
      raw: ['dear', 'doctor', 'byrne', 'um', 'thanks', 'for', 'seeing', 'this', 'sixty-two', 'no', 'wait', 'sixty-three', 'year', 'old', 'man', 'with', 'uh', 'three', 'weeks', 'of', 'chest', 'tightness', 'on', 'exertion'],
      fill: [3, 9, 10, 16], corr: [8, 11],
      fair: 'Dear Dr Byrne, thank you for seeing this <em>63-year-old</em> man with three weeks of chest tightness on exertion.',
      dest: 'Healthlink', app: 'Healthlink · New referral', short: 'Dear Dr Byrne, thank you for seeing this 63-year-old man…', ms: 58,
    },
    dev: {
      raw: ['okay', 'so', 'in', 'the', 'auth', 'middleware', 'um', 'swap', 'the', 'JWT', 'check', 'for', 'the', 'no', 'wait', 'a', 'session', 'store', 'lookup', 'and', 'like', 'add', 'a', 'test', 'for', 'expired', 'sessions'],
      fill: [0, 1, 6, 12, 13, 14, 20], corr: [15, 16, 17, 18],
      fair: 'In the auth middleware, swap the JWT check for a <em>session store lookup</em> and add a test for expired sessions.',
      dest: 'Claude Code', app: 'Terminal · claude', short: 'In the auth middleware, swap the JWT check for a session…', ms: 61,
    },
  };
  let ex = 'gp', step = -1;
  const how = $('.how');
  const demo = $('.demo');
  function renderDemo() {
    const e = EX[ex];
    $('[data-raw]', demo).textContent = e.raw.join(' ');
    $('[data-raw-marked]', demo).innerHTML = e.raw.map((w, i) => e.fill.includes(i) ? `<span class="f">${w}</span>` : e.corr.includes(i) ? `<span class="c">${w}</span>` : w).join(' ');
    $('[data-tokens]', demo).innerHTML = e.raw.slice(0, 18).map((w, i) => `<span class="${e.fill.includes(i) ? 'f' : ''}">${w}<small style="opacity:.55;margin-left:6px">${(0.18 + i * 0.27).toFixed(2)}s</small></span>`).join('');
    $('[data-fair]', demo).innerHTML = e.fair;
    $('[data-before]', demo).innerHTML = e.raw.map((w, i) => e.fill.includes(i) ? `<s>${w}</s>` : w).join(' ') + '<span class="arrow">↓ settled by SpeakoFlow Mini</span>';
    $('[data-dest-app]', demo).textContent = e.app;
    $('[data-dest-text]', demo).textContent = e.short;
    $('[data-fillers]', demo).textContent = e.fill.length;
    $('[data-dest]', demo).textContent = e.dest;
    $('[data-ms]', demo).textContent = `${e.ms} ms`;
  }
  $$('.seg button', demo).forEach((b) => b.addEventListener('click', () => {
    ex = b.dataset.ex; $$('.seg button', demo).forEach((x) => x.setAttribute('aria-pressed', String(x === b))); renderDemo();
  }));
  renderDemo();
  const LABELS = ['Step 1 of 3: listening', 'Step 2 of 3: transcribed on the Neural Engine', 'Step 3 of 3: polished and inserted'];
  function setStep(s) {
    if (s === step) return; step = s;
    $$('.how-steps li').forEach((li) => li.classList.toggle('on', +li.dataset.step === s));
    $$('.demo-pane', demo).forEach((p) => p.classList.toggle('on', +p.dataset.pane === s));
    $('[data-stage-label]', demo).textContent = LABELS[s];
  }
  setStep(0);
  function howProgress() {
    if (innerWidth < 860 || reduce) return;
    const r = how.getBoundingClientRect();
    const p = Math.min(1, Math.max(0, -r.top / (how.offsetHeight - innerHeight)));
    setStep(Math.min(2, Math.floor(p * 3)));
    $('.demo-progress i').style.width = `${p * 100}%`;
  }

  /* Live waveform in step 1: a continuous line, like the mark, never bars. */
  const wave = $('canvas.wave', demo);
  const wctx = wave.getContext('2d');
  let wt = 0;
  function drawWave() {
    const dpr = Math.min(devicePixelRatio, 2), w = wave.clientWidth, h = wave.clientHeight;
    if (wave.width !== Math.round(w * dpr)) { wave.width = Math.round(w * dpr); wave.height = Math.round(h * dpr); }
    wctx.setTransform(dpr, 0, 0, dpr, 0, 0); wctx.clearRect(0, 0, w, h);
    const ink = cssVar(demo, '--accent'), sig = cssVar(demo, '--signal');
    for (let k = 0; k < 3; k++) {
      wctx.beginPath();
      for (let x = 0; x <= w; x += 2) {
        const u = x / w;
        const env = Math.pow(Math.sin(Math.PI * Math.min(1, u * 1.05)), 1.2) * (0.55 + 0.45 * Math.abs(Math.sin(wt * 3.9 + u * 7)));
        const y = h / 2 + env * h * 0.36 * (Math.sin(u * 38 + wt * 6 + k * 1.3) * 0.6 + Math.sin(u * 91 - wt * 9 + k) * 0.25 + Math.sin(u * 13 + wt * 2) * 0.15) * (1 - k * 0.28);
        x ? wctx.lineTo(x, y) : wctx.moveTo(x, y);
      }
      wctx.strokeStyle = ink; wctx.globalAlpha = k ? 0.28 : 0.95; wctx.lineWidth = k ? 1 : 2; wctx.lineJoin = 'round'; wctx.stroke();
    }
    wctx.globalAlpha = 1; wctx.fillStyle = sig; wctx.fillRect(w - 2, h * 0.2, 2, h * 0.6);
  }

  /* ---------- Speed: count-up ---------- */
  const countIO = new IntersectionObserver((es) => es.forEach((e) => {
    if (!e.isIntersecting) return; countIO.unobserve(e.target);
    $$('[data-count]', e.target).forEach((el) => {
      const to = +el.dataset.count; if (reduce) return;
      const t0 = performance.now();
      const tick = (n) => { const k = Math.min(1, (n - t0) / 1400); el.textContent = Math.round(to * (1 - Math.pow(1 - k, 3))); if (k < 1) requestAnimationFrame(tick); };
      requestAnimationFrame(tick);
    });
  }), { threshold: 0.4 });
  countIO.observe($('.bignum'));

  /* ---------- Privacy: building floor plan ---------- */
  const bld = $('.building canvas');
  const bctx = bld.getContext('2d');
  const ROOMS = [
    { n: 'Reception', x: .08, y: .14, w: .30, h: .30, dev: [[.2, .3, 'iPad']] },
    { n: 'Consult 1', x: .38, y: .14, w: .27, h: .30, dev: [[.5, .29, 'Windows PC']] },
    { n: 'Consult 2', x: .65, y: .14, w: .27, h: .30, dev: [[.79, .29, 'MacBook']] },
    { n: 'Nurse', x: .08, y: .44, w: .30, h: .30, dev: [[.21, .6, 'iPhone']] },
    { n: 'Back office', x: .38, y: .44, w: .54, h: .30, host: [.66, .6] },
  ];
  function drawBuilding(t) {
    const dpr = Math.min(devicePixelRatio, 2), w = bld.clientWidth, h = bld.clientHeight;
    if (bld.width !== Math.round(w * dpr)) { bld.width = Math.round(w * dpr); bld.height = Math.round(h * dpr); }
    const c = bctx; c.setTransform(dpr, 0, 0, dpr, 0, 0); c.clearRect(0, 0, w, h);
    const el = bld.parentElement, fg = cssVar(el, '--fg'), fg3 = cssVar(el, '--fg-3'), ac = cssVar(el, '--accent'), sg = cssVar(el, '--signal'), hair = cssVar(el, '--hair'), rule = cssVar(el, '--rule');
    // ruled paper
    c.strokeStyle = rule; c.lineWidth = 1;
    for (let y = 16; y < h; y += 24) { c.beginPath(); c.moveTo(0, y + .5); c.lineTo(w, y + .5); c.stroke(); }
    // outside: the cloud, crossed out
    c.font = '500 12px "Atkinson Hyperlegible Mono"'; c.fillStyle = fg3; c.textAlign = 'center';
    const cx = w * .5, cy = h * .91;
    c.setLineDash([4, 5]); c.strokeStyle = fg3; c.beginPath(); c.moveTo(w * .66, h * .74); c.lineTo(cx + 6, cy - 16); c.stroke(); c.setLineDash([]);
    c.fillText('cloud speech service · 0 bytes', cx, cy + 4);
    c.strokeStyle = sg; c.lineWidth = 2; c.beginPath(); c.moveTo(cx - 8, cy - 26); c.lineTo(cx + 8, cy - 10); c.moveTo(cx + 8, cy - 26); c.lineTo(cx - 8, cy - 10); c.stroke();
    // rooms
    c.lineWidth = 1.2; c.strokeStyle = fg3; c.textAlign = 'left'; c.font = '500 11.5px "Atkinson Hyperlegible Mono"';
    ROOMS.forEach((r) => { c.strokeRect(r.x * w, r.y * h, r.w * w, r.h * h); c.fillStyle = fg3; c.fillText(r.n.toUpperCase(), r.x * w + 10, r.y * h + 20); });
    // outer wall
    c.lineWidth = 4; c.strokeStyle = fg; c.strokeRect(w * .08, h * .14, w * .84, h * .6);
    c.fillStyle = fg; c.font = '600 12px "Atkinson Hyperlegible Mono"'; c.fillText('THE BUILDING', w * .08, h * .1);
    // host
    const [hx, hy] = ROOMS[4].host; const HX = hx * w, HY = hy * h;
    c.fillStyle = cssVar(el, '--bg-raised'); c.strokeStyle = ac; c.lineWidth = 1.6;
    roundRect(c, HX - 52, HY - 18, 104, 36, 10); c.fill(); c.stroke();
    c.fillStyle = fg; c.textAlign = 'center'; c.font = '600 12px "Host Grotesk"'; c.fillText('Mac mini · host', HX, HY + 4);
    // devices + flows
    let k = 0;
    ROOMS.forEach((r) => (r.dev || []).forEach(([dx, dy, name]) => {
      const X = dx * w, Y = dy * h;
      c.strokeStyle = hair; c.lineWidth = 1.2; c.beginPath(); c.moveTo(X, Y); c.quadraticCurveTo((X + HX) / 2, Math.max(Y, HY) + 30, HX, HY); c.stroke();
      const ph = ((t * 0.45 + k * 0.27) % 1);
      const px = (1 - ph) * (1 - ph) * X + 2 * (1 - ph) * ph * (X + HX) / 2 + ph * ph * HX;
      const py = (1 - ph) * (1 - ph) * Y + 2 * (1 - ph) * ph * (Math.max(Y, HY) + 30) + ph * ph * HY;
      c.fillStyle = sg; c.beginPath(); c.arc(px, py, 3.2, 0, 7); c.fill();
      c.fillStyle = ac; c.beginPath(); c.arc(X, Y, 5, 0, 7); c.fill();
      c.fillStyle = fg3; c.font = '500 11px "Atkinson Hyperlegible Mono"'; c.fillText(name, X, Y + 20);
      k++;
    }));
    c.textAlign = 'left';
  }
  function roundRect(c, x, y, w, h, r) { c.beginPath(); c.moveTo(x + r, y); c.arcTo(x + w, y, x + w, y + h, r); c.arcTo(x + w, y + h, x, y + h, r); c.arcTo(x, y + h, x, y, r); c.arcTo(x, y, x + w, y, r); c.closePath(); }

  /* ---------- Practice host: live flow ---------- */
  const flow = $('.flow canvas');
  const fctx = flow.getContext('2d');
  const DEVICES = ['Reception · iPad', 'Consult 1 · Windows', 'Consult 2 · MacBook', 'Consult 3 · Windows', 'Nurse · iPhone', 'Dr Walsh · iPhone'];
  const WORKERS = ['worker 1', 'worker 2'];
  const MODELS = ['Parakeet TDT 0.6B v3', 'SpeakoFlow Mini'];
  const jobs = [];
  let nextJob = 0, logT = 0;
  const logEl = $('.flow-log');
  function nodes(w, h) {
    if (w < 700) { // vertical layout for phones: devices on top, models at the bottom
      const short = DEVICES.map((n) => n.split(' · ')[0]);
      return {
        v: true,
        dev: short.map((n, i) => ({ n, x: w * (i % 2 ? .62 : .1), y: h * (.13 + Math.floor(i / 2) * .075) })),
        host: { n: 'practice-host', x: w * .5, y: h * .48 },
        wk: WORKERS.map((n, i) => ({ n, x: w * (i ? .74 : .26), y: h * .64 })),
        md: MODELS.map((n, i) => ({ n, x: w * (i ? .74 : .26), y: h * .79 })),
      };
    }
    const pad = 40;
    const col = (f) => pad + (w - pad * 2) * f;
    const dy = (i, n, top = .16, bot = .8) => h * (top + (bot - top) * (n === 1 ? .5 : i / (n - 1)));
    return {
      dev: DEVICES.map((n, i) => ({ n, x: col(.0), y: dy(i, DEVICES.length, .14, .74) })),
      host: { n: 'practice-host', x: col(.42), y: h * .44 },
      wk: WORKERS.map((n, i) => ({ n, x: col(.66), y: dy(i, 2, .3, .58) })),
      md: MODELS.map((n, i) => ({ n, x: col(.9), y: dy(i, 2, .3, .58) })),
    };
  }
  function drawFlow(t, dt) {
    const dpr = Math.min(devicePixelRatio, 2), w = flow.clientWidth, h = flow.clientHeight;
    if (flow.width !== Math.round(w * dpr)) { flow.width = Math.round(w * dpr); flow.height = Math.round(h * dpr); }
    const c = fctx; c.setTransform(dpr, 0, 0, dpr, 0, 0); c.clearRect(0, 0, w, h);
    const el = flow.parentElement, fg = cssVar(el, '--fg'), fg2 = cssVar(el, '--fg-2'), fg3 = cssVar(el, '--fg-3'), ac = cssVar(el, '--accent'), sg = cssVar(el, '--signal'), hair = cssVar(el, '--hair'), raised = cssVar(el, '--bg-raised');
    const N = nodes(w, h), small = w < 700;
    const V = !!N.v;
    const ctrl = (a, b) => V ? [[a.x, (a.y + b.y) / 2], [b.x, (a.y + b.y) / 2]] : [[(a.x + b.x) / 2, a.y], [(a.x + b.x) / 2, b.y]];
    const curve = (a, b) => { const [p1, p2] = ctrl(a, b); c.beginPath(); c.moveTo(a.x, a.y); c.bezierCurveTo(p1[0], p1[1], p2[0], p2[1], b.x, b.y); c.stroke(); };
    c.strokeStyle = hair; c.lineWidth = 1.2;
    N.dev.forEach((d) => curve(d, N.host)); N.wk.forEach((k) => curve(N.host, k)); N.wk.forEach((k, i) => curve(k, N.md[i === 0 ? 0 : 0])); curve(N.wk[1], N.md[1]); curve(N.wk[0], N.md[1]);
    // spawn
    if (dt && t > nextJob) {
      const d = Math.floor(Math.random() * DEVICES.length), wk = Math.random() < .55 ? 0 : 1;
      jobs.push({ d, wk, t0: t, ms: 44 + Math.round(Math.random() * 34), secs: (1 + Math.random() * 6).toFixed(1) });
      nextJob = t + 0.45 + Math.random() * 0.9;
    }
    const bez = (a, b, u) => { const [p1, p2] = ctrl(a, b); const p0 = [a.x, a.y], p3 = [b.x, b.y]; const v = 1 - u; return [v*v*v*p0[0] + 3*v*v*u*p1[0] + 3*v*u*u*p2[0] + u*u*u*p3[0], v*v*v*p0[1] + 3*v*v*u*p1[1] + 3*v*u*u*p2[1] + u*u*u*p3[1]]; };
    for (let i = jobs.length - 1; i >= 0; i--) {
      const j = jobs[i], age = t - j.t0;
      const D = N.dev[j.d], K = N.wk[j.wk], M = N.md[0], P = N.md[1];
      let pt, colr = sg, r = 4;
      if (age < 1.1) pt = bez(D, N.host, ease(age / 1.1));
      else if (age < 1.7) pt = bez(N.host, K, ease((age - 1.1) / .6));
      else if (age < 2.1) pt = bez(K, M, ease((age - 1.7) / .4));
      else if (age < 2.5) pt = bez(K, P, ease((age - 2.1) / .4)), colr = ac;
      else if (age < 3.7) pt = bez(N.host, D, ease((age - 2.5) / 1.2)), colr = ac, r = 3.4;
      else { jobs.splice(i, 1); continue; }
      if (age >= 2.5 && !j.logged) { j.logged = 1; log(j); }
      c.fillStyle = colr; c.globalAlpha = .18; c.beginPath(); c.arc(pt[0], pt[1], r * 3, 0, 7); c.fill();
      c.globalAlpha = 1; c.beginPath(); c.arc(pt[0], pt[1], r, 0, 7); c.fill();
    }
    // nodes
    const card = (x, y, wd, ht, title, sub, accent) => {
      c.save(); c.shadowColor = '#0003'; c.shadowBlur = 24; c.shadowOffsetY = 8;
      c.fillStyle = raised; roundRect(c, x - wd / 2, y - ht / 2, wd, ht, 14); c.fill(); c.restore();
      c.strokeStyle = accent ? ac : hair; c.lineWidth = accent ? 1.6 : 1; roundRect(c, x - wd / 2, y - ht / 2, wd, ht, 14); c.stroke();
      c.fillStyle = fg; c.font = `600 ${small ? 11 : 13}px "Host Grotesk"`; c.textAlign = 'left'; c.fillText(title, x - wd / 2 + 14, y - (sub ? 2 : -4));
      if (sub) { c.fillStyle = fg3; c.font = `500 ${small ? 10 : 11}px "Atkinson Hyperlegible Mono"`; c.fillText(sub, x - wd / 2 + 14, y + 14); }
    };
    N.dev.forEach((d) => {
      c.fillStyle = ac; c.beginPath(); c.arc(d.x + 6, d.y, 5, 0, 7); c.fill();
      c.fillStyle = fg2; c.font = `500 ${small ? 10.5 : 12.5}px "Atkinson Hyperlegible Mono"`; c.textAlign = 'left'; c.fillText(d.n, d.x + 18, d.y + 4);
    });
    const busy = jobs.filter((j) => t - j.t0 > 1.1 && t - j.t0 < 2.5);
    card(N.host.x, N.host.y, small ? 150 : 180, 64, 'Mac mini', `queue ${jobs.filter((j) => t - j.t0 < 1.1).length} · tailnet`, true);
    N.wk.forEach((k, i) => card(k.x, k.y, small ? w * .44 : 140, 52, k.n, busy.some((j) => j.wk === i) ? '● transcribing' : 'idle'));
    N.md.forEach((m, i) => card(V ? m.x : m.x - 60, m.y, V ? w * .44 : 180, 52, V ? (i ? 'SpeakoFlow Mini' : 'Parakeet v3') : m.n, i ? 'clean-up · loaded' : (V ? 'speech · ANE' : 'speech · Neural Engine')));
  }
  const ease = (u) => 1 - Math.pow(1 - u, 3);
  function log(j) {
    const now = new Date(Date.now()).toLocaleTimeString('en-IE', { hour12: false });
    const line = document.createElement('div');
    line.innerHTML = `${now}  <b>${DEVICES[j.d]}</b>  ${j.secs} s audio → ${WORKERS[j.wk]} → text in <b>${j.ms} ms</b>`;
    logEl.append(line); while (logEl.children.length > 4) logEl.firstChild.remove();
    const p = $('[data-kpi-p50]'); if (p) p.textContent = `p50 ${55 + Math.round(Math.random() * 10)} ms`;
  }

  /* ---------- Models ---------- */
  const MODELS_DATA = [
    { name: 'Parakeet TDT 0.6B v3', by: 'NVIDIA · Core ML', badge: 'Default', text: 'Fast, accurate dictation in English and 24 other European languages.', dl: [['Download', '470 MB'], ['Languages', '25'], ['Word error rate', '2.27%'], ['Speed', '128× real time']], seed: 1, hue: 'accent' },
    { name: 'Parakeet Ultra', by: 'Moondream · Core ML', badge: 'Most accurate', text: 'Post-trained v3. Lowest error rate in every language at the same speed.', dl: [['Download', '613 MB'], ['Languages', '25'], ['Word error rate', '2.13%'], ['Speed', '127× real time']], seed: 2, hue: 'accent' },
    { name: 'Parakeet TDT 0.6B v2', by: 'NVIDIA · Core ML', badge: 'English only', text: 'The English-only predecessor, kept for parity with the Windows and Linux app.', dl: [['Download', '470 MB'], ['Languages', 'English'], ['Runs on', 'Neural Engine'], ['Status', 'Stable']], seed: 3, hue: 'fg' },
    { name: 'SpeakoFlow Mini', by: 'Fairspoken · clean-up', badge: 'Clean-up', text: 'Trained on dictation. Removes fillers, settles self-corrections, keeps your meaning.', dl: [['Runs', 'On device'], ['Input', 'Raw transcript'], ['Fixes', '“No wait…”'], ['Keeps', 'Your words']], seed: 4, hue: 'signal' },
  ];
  function art(seed, colour) {
    let s = seed * 9301;
    const rnd = () => ((s = (s * 16807) % 2147483647) / 2147483647);
    const W = 320, H = 280, m = 150 + seed * 18;
    let paths = '';
    for (let i = 0; i < 14; i++) {
      const a = 18 + rnd() * 38, f1 = .02 + rnd() * .03, f2 = .05 + rnd() * .05, ph = rnd() * 6, sp = (rnd() - .5) * 60;
      let d = '';
      for (let x = 0; x <= W; x += 4) {
        const env = x < m ? Math.pow((m - x) / m, .7) : 0;
        const y = 150 + env * (a * Math.sin(x * f1 + ph) + a * .4 * Math.sin(x * f2 - ph) + sp);
        d += (x ? 'L' : 'M') + x + ' ' + y.toFixed(1);
      }
      paths += `<path d="${d}" fill="none" stroke="var(--${colour === 'signal' ? 'signal' : colour === 'fg' ? 'fg' : 'accent'})" stroke-opacity="${(.18 + rnd() * .5).toFixed(2)}" stroke-width="1.2"/>`;
    }
    return `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid slice" aria-hidden="true"><defs><pattern id="r${seed}" width="8" height="20" patternUnits="userSpaceOnUse"><path d="M0 19.5H8" stroke="var(--rule)" stroke-width="1"/></pattern></defs><rect width="${W}" height="${H}" fill="url(#r${seed})"/>${paths}<path d="M${m} 150H${W}" stroke="var(--fg)" stroke-width="1.6"/><path d="M${m} 0V${H}" stroke="var(--signal)" stroke-width="1.4"/></svg>`;
  }
  $('[data-models]').innerHTML = MODELS_DATA.map((m) => `
    <article class="model rv"><div class="art">${art(m.seed, m.hue)}<span class="badge">${m.badge}</span></div>
    <div class="body"><h3>${m.name}</h3><span class="by">${m.by}</span><p>${m.text}</p>
    <dl>${m.dl.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join('')}</dl></div></article>`).join('');
  $$('[data-models] .rv').forEach((el, k) => { el.style.transitionDelay = `${k * 80}ms`; io.observe(el); if (reduce) el.classList.add('in'); });

  /* ---------- Developers: terminal ---------- */
  const term = $('[data-term]');
  const TERM = [
    ['m', 'claude code · ~/clinic-api · sonnet'],
    ['', ''],
    ['r', '● fn held · listening 6.2 s'],
    ['wait', 1600],
    ['p', '> '],
    ['wait', 300],
    ['paste', 'Add a retry with exponential backoff to the Healthlink webhook handler. Cap it at five attempts and log each failure with the message ID.'],
    ['g', '↳ 61 ms · polished · 5 fillers removed · 1 correction settled'],
    ['', ''],
    ['m', '⏺ I\'ll add retry logic to src/webhooks/healthlink.ts.'],
    ['m', '  Reading src/webhooks/healthlink.ts …'],
  ];
  async function runTerm() {
    term.innerHTML = '';
    let promptLine = null;
    for (const [k, v] of TERM) {
      if (k === 'wait') { if (!reduce) await new Promise((r) => setTimeout(r, v)); continue; }
      if (k === 'paste') { promptLine.innerHTML = `<span class="p">&gt;</span> <span class="typed">${v}</span>`; continue; }
      const line = document.createElement('div');
      line.innerHTML = k === 'p' ? '<span class="p">&gt;</span> <span class="caret"></span>' : `<span class="${k}">${v || '&nbsp;'}</span>`;
      if (k === 'p') promptLine = line;
      term.append(line);
      if (!reduce) await new Promise((r) => setTimeout(r, 160));
    }
    const end = document.createElement('div'); end.innerHTML = '<span class="p">&gt;</span> <span class="caret"></span>'; term.append(end);
  }
  const termIO = new IntersectionObserver((es) => { if (es[0].isIntersecting) { termIO.disconnect(); runTerm(); } }, { threshold: .35 });
  termIO.observe(term);

  /* ---------- Shared animation loop for canvases (only when visible) ---------- */
  const vis = new Map();
  const visIO = new IntersectionObserver((es) => es.forEach((e) => vis.set(e.target, e.isIntersecting)), { rootMargin: '100px' });
  [wave, bld, flow].forEach((c) => visIO.observe(c));
  let last = performance.now(), T = 0;
  function tick(now) {
    const dt = Math.min(.05, (now - last) / 1000); last = now; T += dt;
    if (!document.hidden) {
      if (vis.get(wave) && step === 0) { wt += dt; drawWave(); }
      if (vis.get(bld)) drawBuilding(T);
      if (vis.get(flow)) drawFlow(T, dt);
    }
    requestAnimationFrame(tick);
  }
  function redrawAll() { drawWave(); drawBuilding(T); drawFlow(T, 0); }
  // Prime the flow with a few jobs in flight so it never looks empty.
  for (let i = 0; i < 5; i++) jobs.push({ d: i % DEVICES.length, wk: i % 2, t0: -i * .7, ms: 50 + i * 4, secs: (2 + i).toFixed(1), logged: 1 });
  (document.fonts ? document.fonts.ready : Promise.resolve()).then(() => {
    if (reduce) { T = 1.3; redrawAll(); [1, 2, 3].forEach((i) => log({ d: i, wk: i % 2, ms: 52 + i * 5, secs: (2.4 + i).toFixed(1) })); return; }
    requestAnimationFrame(tick);
  });

  addEventListener('scroll', howProgress, { passive: true });
  howProgress();
})();
