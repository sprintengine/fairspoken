/* Shared shell for the Fairspoken macOS 26 mockups: desktop wallpaper, menu bar,
   window, floating glass sidebar, icons and the generative "signal" artwork.
   ?theme=light|dark picks the appearance (default light). */
(() => {
  const q = new URLSearchParams(location.search);
  const theme = q.get('theme') || 'light';
  document.documentElement.dataset.theme = theme;
  const dark = theme === 'dark';

  const I = {
    home: '<path d="M4 10.5 12 4l8 6.5V19a1.5 1.5 0 0 1-1.5 1.5H15v-6H9v6H5.5A1.5 1.5 0 0 1 4 19z"/>',
    notes: '<rect x="5" y="3.5" width="14" height="17" rx="2.5"/><path d="M8.5 8.5h7M8.5 12h7M8.5 15.5h4"/>',
    activity: '<path d="M3 12h3.5l2.5-6 4 12 2.5-6H21"/>',
    models: '<path d="m12 3.5 8 4.2v8.6l-8 4.2-8-4.2V7.7z"/><path d="M4 7.7 12 12l8-4.3M12 12v8.5"/>',
    server: '<rect x="4" y="4" width="16" height="7" rx="2"/><rect x="4" y="13" width="16" height="7" rx="2"/><path d="M8 7.5h.01M8 16.5h.01M12 7.5h5M12 16.5h5"/>',
    settings: '<circle cx="12" cy="12" r="3"/><path d="M12 2.8v2.4M12 18.8v2.4M4.2 7.5l2 1.2M17.8 15.3l2 1.2M4.2 16.5l2-1.2M17.8 8.7l2-1.2"/>',
    search: '<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4 4"/>',
    eye: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>',
    eyeoff: '<path d="M3 3l18 18M10.6 5.6A10 10 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a17 17 0 0 1-3 3.6M6.4 6.9A17 17 0 0 0 2.5 12S6 18.5 12 18.5a9.6 9.6 0 0 0 4.2-1"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/>',
    moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>',
    mic: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21"/>',
    lock: '<rect x="4.5" y="10.5" width="15" height="10" rx="3"/><path d="M8 10.5V8a4 4 0 0 1 8 0v2.5"/>',
    refresh: '<path d="M20 12a8 8 0 1 1-2.3-5.6M20 4v4.5h-4.5"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    down: '<path d="M12 4v12m-5-5 5 5 5-5M5 20h14"/>',
    check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
    ipad: '<rect x="5" y="3" width="14" height="18" rx="2.5"/><path d="M11 18h2"/>',
    pc: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
    laptop: '<path d="M5 6.5A1.5 1.5 0 0 1 6.5 5h11A1.5 1.5 0 0 1 19 6.5V15H5z"/><path d="M2.5 18h19"/>',
    phone: '<rect x="7" y="2.5" width="10" height="19" rx="2.5"/><path d="M11 18.5h2"/>',
    cpu: '<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 2.5v3M15 2.5v3M9 18.5v3M15 18.5v3M2.5 9h3M2.5 15h3M18.5 9h3M18.5 15h3"/>',
    sparkle: '<path d="M12 3.5 13.8 10l6.7 2-6.7 2L12 20.5 10.2 14l-6.7-2 6.7-2z"/>',
    wave: '<path d="M3 12c1.5 0 1.5-6 3-6s1.5 12 3 12 1.5-8 3-8 1.5 4 3 4h6"/>',
    chev: '<path d="m9 6 6 6-6 6"/>',
  };
  const icon = (name, extra = '') => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" ${extra}>${I[name] || ''}</svg>`;
  const MARK = `<svg viewBox="0 0 64 64" fill="none" stroke-linecap="round" stroke-linejoin="round"><path d="M7 32 C7.28 30.49 8.1 25.05 8.65 22.91 C9.2 20.77 9.75 19.35 10.3 19.15 C10.85 18.95 11.4 20.16 11.95 21.73 C12.5 23.3 13.05 26.21 13.6 28.57 C14.15 30.93 14.7 33.94 15.25 35.89 C15.8 37.84 16.35 39.55 16.9 40.26 C17.45 40.97 18 40.79 18.55 40.15 C19.1 39.51 19.65 37.87 20.2 36.42 C20.75 34.98 21.3 32.91 21.85 31.47 C22.4 30.04 22.95 28.6 23.5 27.84 C24.05 27.08 24.6 26.81 25.15 26.91 C25.7 27.02 26.25 27.77 26.8 28.48 C27.35 29.18 27.9 30.34 28.45 31.15 C29 31.96 29.55 32.84 30.1 33.34 C30.65 33.84 31.2 34.09 31.75 34.13 C32.3 34.18 32.85 33.88 33.4 33.62 C33.95 33.37 34.5 32.89 35.05 32.61 C35.6 32.32 36.15 32.03 36.7 31.9 C37.25 31.76 37.8 31.78 38.35 31.8 C38.9 31.82 39.73 31.97 40 32 L45 32" stroke="var(--accent)" stroke-width="5"/><path d="M53 20V44" stroke="var(--signal)" stroke-width="5"/></svg>`;

  /* ---------- Seeded random ---------- */
  function rng(seed) { let s = (seed * 9301 + 49297) % 233280 || 1; return () => (s = (s * 16807) % 2147483647) / 2147483647; }

  /* ---------- Wallpaper: "Atlantic dusk", silk ribbons over a deep gradient ---------- */
  function wallpaper(c) {
    const W = c.width = 1600 * 2, H = c.height = 1000 * 2;
    const x = c.getContext('2d');
    x.scale(2, 2);
    const g = x.createLinearGradient(0, 0, 0, 1000);
    if (dark) { g.addColorStop(0, '#070B1E'); g.addColorStop(.55, '#141A44'); g.addColorStop(1, '#2A1430'); }
    else { g.addColorStop(0, '#9DB6EE'); g.addColorStop(.55, '#C9D3F2'); g.addColorStop(1, '#F4CDB4'); }
    x.fillStyle = g; x.fillRect(0, 0, 1600, 1000);
    const blob = (cx, cy, r, col) => { const rg = x.createRadialGradient(cx, cy, 0, cx, cy, r); rg.addColorStop(0, col); rg.addColorStop(1, 'transparent'); x.fillStyle = rg; x.fillRect(0, 0, 1600, 1000); };
    if (dark) { blob(1250, 820, 620, 'rgba(229,81,43,.45)'); blob(300, 200, 700, 'rgba(44,75,208,.55)'); blob(900, 500, 500, 'rgba(120,90,220,.25)'); }
    else { blob(1300, 850, 600, 'rgba(255,140,90,.55)'); blob(250, 150, 700, 'rgba(44,75,208,.45)'); blob(800, 420, 520, 'rgba(255,255,255,.5)'); }
    // silk ribbons
    const ribbon = (y0, amp, f, ph, thick, c1, c2, alpha) => {
      x.save(); x.globalAlpha = alpha; x.filter = 'blur(1px)';
      const lg = x.createLinearGradient(0, y0 - amp, 1600, y0 + amp); lg.addColorStop(0, c1); lg.addColorStop(1, c2);
      x.fillStyle = lg; x.beginPath();
      for (let i = 0; i <= 160; i++) { const X = i * 10, Y = y0 + amp * Math.sin(X * f + ph) + amp * .35 * Math.sin(X * f * 2.3 + ph * 2); i ? x.lineTo(X, Y) : x.moveTo(X, Y); }
      for (let i = 160; i >= 0; i--) { const X = i * 10, t = thick * (.4 + .6 * Math.abs(Math.sin(X * f * .7 + ph))); const Y = y0 + t + amp * Math.sin(X * f + ph + .5) + amp * .35 * Math.sin(X * f * 2.3 + ph * 2 + .4); x.lineTo(X, Y); }
      x.closePath(); x.fill(); x.restore();
    };
    if (dark) {
      ribbon(420, 120, .0035, .4, 160, 'rgba(70,100,255,.9)', 'rgba(150,120,255,.6)', .55);
      ribbon(560, 140, .003, 2.1, 120, 'rgba(255,113,72,.85)', 'rgba(255,170,90,.5)', .5);
      ribbon(700, 90, .004, 4, 90, 'rgba(120,200,190,.6)', 'rgba(70,100,255,.5)', .4);
    } else {
      ribbon(420, 120, .0035, .4, 160, 'rgba(44,75,208,.85)', 'rgba(140,160,255,.7)', .55);
      ribbon(560, 140, .003, 2.1, 120, 'rgba(229,81,43,.85)', 'rgba(255,190,120,.7)', .55);
      ribbon(700, 90, .004, 4, 90, 'rgba(255,255,255,.9)', 'rgba(150,200,230,.7)', .6);
    }
    // fine strands for detail under glass
    x.globalAlpha = dark ? .35 : .3; x.strokeStyle = '#fff'; x.lineWidth = .8;
    for (let k = 0; k < 26; k++) { x.beginPath(); for (let i = 0; i <= 160; i++) { const X = i * 10, Y = 500 + (k - 13) * 9 + 160 * Math.sin(X * .0032 + k * .05) * Math.cos(X * .001 + k * .02); i ? x.lineTo(X, Y) : x.moveTo(X, Y); } x.stroke(); }
    x.globalAlpha = 1;
  }

  /* ---------- Signal artwork: rough strands settling into one line at a margin ---------- */
  function signalArt(c, o = {}) {
    const { w, h, seed = 1, mx = .62, my = .55, palette = 'brand', ruled = true, bg, strands = 40, amp = .34, lineTo = 1 } = o;
    const s = 2;
    c.width = w * s; c.height = h * s; c.style.width = w + 'px'; c.style.height = h + 'px';
    const x = c.getContext('2d'); x.scale(s, s);
    const R = rng(seed);
    const P = {
      brand: dark ? ['#93A8FF', '#FFFFFF', '#FF7148', '#F4BE4F'] : ['#2C4BD0', '#4767F0', '#E5512B', '#F2A93B'],
      sea: dark ? ['#7FD4C4', '#93A8FF', '#FFFFFF', '#F4BE4F'] : ['#0E7C6B', '#2C4BD0', '#4BA89A', '#F2A93B'],
      ember: dark ? ['#FF7148', '#FFB18F', '#FFFFFF', '#93A8FF'] : ['#E5512B', '#B23B16', '#F2A93B', '#2C4BD0'],
      gorse: dark ? ['#F4BE4F', '#FFFFFF', '#FF7148', '#93A8FF'] : ['#B87800', '#E5512B', '#2C4BD0', '#F2A93B'],
      ink: dark ? ['#FFFFFF', '#B0B8C3', '#93A8FF', '#FF7148'] : ['#0E1318', '#3D4752', '#2C4BD0', '#E5512B'],
    }[palette];
    x.fillStyle = bg || (dark ? '#0A0D14' : '#F3F1EA'); x.fillRect(0, 0, w, h);
    const blob = (cx, cy, r, col) => { const g = x.createRadialGradient(cx, cy, 0, cx, cy, r); g.addColorStop(0, col); g.addColorStop(1, 'transparent'); x.fillStyle = g; x.fillRect(0, 0, w, h); };
    const a = dark ? .5 : .32;
    const hex2 = (hx, al) => hx + Math.round(al * 255).toString(16).padStart(2, '0');
    blob(w * (.15 + R() * .2), h * (.3 + R() * .4), Math.max(w, h) * .55, hex2(P[0], a));
    blob(w * mx, h * my, Math.max(w, h) * .35, hex2(P[2], a * .9));
    blob(w * (.35 + R() * .3), h * (.8 + R() * .3), Math.max(w, h) * .45, hex2(P[3], a * .7));
    if (ruled) { x.strokeStyle = dark ? 'rgba(147,168,255,.08)' : 'rgba(44,75,208,.1)'; x.lineWidth = 1; for (let y = (h * my) % 28; y < h; y += 28) { x.beginPath(); x.moveTo(0, y + .5); x.lineTo(w, y + .5); x.stroke(); } }
    const X0 = w * mx, Y0 = h * my, A = h * amp;
    x.globalCompositeOperation = dark ? 'lighter' : 'source-over';
    for (let i = 0; i < strands; i++) {
      const f1 = .006 + R() * .012, f2 = .02 + R() * .03, ph = R() * 6.28, sp = (R() - .5) * 1.2, col = P[Math.floor(R() * 3)];
      x.strokeStyle = col; x.globalAlpha = (dark ? .25 : .22) + R() * .45; x.lineWidth = .8 + R() * 1.4;
      x.beginPath();
      for (let X = 0; X <= X0; X += 3) {
        const d = (X0 - X) / X0, env = Math.min(1, d / .45) * (.5 + .5 * d);
        const Y = Y0 + A * env * (Math.sin(X * f1 + ph) * .6 + Math.sin(X * f2 - ph) * .25 + sp * .8);
        X ? x.lineTo(X, Y) : x.moveTo(X, Y);
      }
      x.stroke();
    }
    x.globalAlpha = 1; x.globalCompositeOperation = 'source-over';
    // fair line + margin + spark
    x.strokeStyle = dark ? '#F4F6FF' : '#0E1318'; x.lineWidth = 1.6; x.beginPath(); x.moveTo(X0, Y0); x.lineTo(w * lineTo, Y0); x.stroke();
    x.strokeStyle = dark ? '#FF7148' : '#E5512B'; x.lineWidth = 1.4; x.beginPath(); x.moveTo(X0, 0); x.lineTo(X0, h); x.stroke();
    blob(X0, Y0, 70, dark ? 'rgba(255,113,72,.55)' : 'rgba(229,81,43,.35)');
  }

  /* ---------- Chrome ---------- */
  function desktop({ app = 'Fairspoken', menus = ['File', 'Edit', 'View', 'Dictation', 'Window', 'Help'] } = {}) {
    const wp = document.createElement('canvas'); wp.className = 'wallpaper'; document.body.prepend(wp); wallpaper(wp);
    const mb = document.createElement('div'); mb.className = 'menubar';
    mb.innerHTML = `<svg viewBox="0 0 24 24" fill="#fff"><path d="M16.4 12.6c0-2.3 1.9-3.4 2-3.5-1.1-1.6-2.8-1.8-3.4-1.8-1.4-.1-2.8.9-3.5.9s-1.8-.9-3-.9C7 7.3 5.6 8.2 4.8 9.6c-1.6 2.8-.4 6.9 1.1 9.1.8 1.1 1.7 2.3 2.9 2.3 1.2-.1 1.6-.8 3-.8s1.8.8 3 .7c1.3 0 2.1-1.1 2.8-2.2.9-1.3 1.3-2.5 1.3-2.6-.1 0-2.5-.9-2.5-3.5zM14.1 5.8c.6-.8 1.1-1.8 1-2.8-.9 0-2 .6-2.7 1.4-.6.7-1.1 1.7-1 2.7 1 .1 2-.5 2.7-1.3z"/></svg><b>${app}</b>${menus.map((m) => `<span>${m}</span>`).join('')}<span class="right"><span style="display:inline-flex;width:22px;height:14px">${MARK}</span><span>Sun 4 Oct 15:42</span></span>`;
    document.body.append(mb);
  }
  function mount({ active = 'home', counts = {} } = {}) {
    desktop();
    const win = document.querySelector('.window');
    const sb = document.createElement('aside'); sb.className = 'sidebar glass'; sb.dataset.glass = 'regular'; sb.dataset.bevel = '16';
    const item = (k, label, ic) => `<a class="${k === active ? 'on' : ''}" href="${k}.html?theme=${theme}">${icon(ic)}${label}${counts[k] ? `<span class="count">${counts[k]}</span>` : ''}</a>`;
    sb.innerHTML = `<div class="traffic"><i></i><i></i><i></i></div>
      <div class="sb-brand">${MARK}Fairspoken</div>
      <nav class="nav">${item('dashboard', 'Home', 'home')}${item('notes', 'Notes', 'notes')}${item('activity', 'Activity', 'activity')}</nav>
      <div class="sb-label">Engine</div>
      <nav class="nav">${item('models', 'Models', 'models')}${item('server', 'Server', 'server')}${item('settings', 'Settings', 'settings')}</nav>
      <div class="sb-foot"><div class="me"><span class="av">AW</span><div><b>Dr Aoife Walsh</b><span>Harbour Road Practice</span></div></div></div>`;
    win.append(sb);
  }

  window.Shell = { mount, desktop, icon, MARK, signalArt, rng, theme, dark };
})();
