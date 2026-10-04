/* Fairspoken hero: voice → signal → fair copy.
 *
 * One raw WebGL2 fragment shader (no library). Left of the red margin, a bundle of
 * strands carries the rough voice: they swell with each syllable and fan out. At the
 * margin they settle into a single straight line, the fair line, and the clean
 * sentence is written along it in HTML.
 *
 * Scrolling clears the rough work away: the margin slides left until it becomes the
 * page's own margin rule, and the strands collapse into one line.
 *
 * Budget: ~2 KB of GLSL, one draw call, three uniforms change per frame. Renders only
 * while the hero is on screen and the tab is visible. Reduced motion: one still frame.
 */
(() => {
  const stage = document.querySelector('.hero-stage');
  if (!stage) return;
  const canvas = stage.querySelector('canvas');
  const content = stage.querySelector('.hero-content');
  const roughEl = stage.querySelector('.rough');
  const fairEl = stage.querySelector('.faircopy');
  const fairTxt = fairEl.querySelector('.txt');
  const latEl = stage.querySelector('.latency');
  const fallback = stage.querySelector('.hero-fallback');
  const pageRule = document.querySelector('.margin-rule');
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const params = new URLSearchParams(location.search);
  const fixedT = params.has('t') ? parseFloat(params.get('t')) : null; // screenshot hook

  /* ---------- Dictation scripts (rough speech → fair copy) ---------- */
  const SCRIPTS = [
    { rough: 'please see this um sixty-two no wait sixty-three year old man with uh chest tightness on exertion',
      fillers: [3, 5, 6, 12], fair: 'Please see this 63-year-old man with chest tightness on exertion.', dest: 'Healthlink' },
    { rough: 'okay so refactor the auth middleware to use the uh no the session store instead of JWTs',
      fillers: [0, 1, 8, 9], fair: 'Refactor the auth middleware to use the session store, not JWTs.', dest: 'Claude Code' },
    { rough: 'review in like two weeks and um repeat bloods FBC U and E and HbA1c',
      fillers: [2, 6], fair: 'Review in two weeks. Repeat bloods: FBC, U&E and HbA1c.', dest: 'Socrates' },
    { rough: 'can you add a retry with um backoff to the webhook handler and you know log the failures',
      fillers: [0, 1, 5, 12, 13], fair: 'Add a retry with backoff to the webhook handler and log failures.', dest: 'Cursor' },
  ];

  /* ---------- Layout ---------- */
  let W = 0, H = 0, dpr = 1, lineY = 0, marginX0 = 0, marginPage = 0, progress = 0, mobile = false;
  const ease = (t) => 1 - Math.pow(1 - Math.min(Math.max(t, 0), 1), 3);
  function layout() {
    const r = stage.getBoundingClientRect();
    W = r.width; H = r.height; mobile = W < 860;
    dpr = Math.min(window.devicePixelRatio || 1, 1.75);
    canvas.width = Math.round(W * dpr); canvas.height = Math.round(H * dpr);
    lineY = Math.round(H * (mobile ? 0.56 : 0.585));
    marginX0 = mobile ? W * 0.34 : W * 0.36;
    marginPage = pageRule ? pageRule.getBoundingClientRect().left || parseFloat(getComputedStyle(pageRule).left) : 80;
    place();
  }
  function marginNow() { return marginX0 + (marginPage - marginX0) * ease(progress / 0.85); }
  function place() {
    const m = marginNow();
    if (!mobile) content.style.setProperty('--hx', m + 'px');
    fairEl.style.left = mobile ? (m + 14) + 'px' : '';
    // Seat the fair copy on the line: measure the baseline with a zero-height probe.
    const fs = parseFloat(getComputedStyle(fairEl).fontSize);
    const baseline = fs * 0.93;
    fairEl.style.top = (mobile ? lineY + 22 : lineY - baseline + 4) + 'px';
    const amp = H * (mobile ? 0.1 : 0.15);
    roughEl.style.top = (mobile ? lineY - amp - 96 : lineY + amp + 12) + 'px';
    latEl.style.left = (m + 14) + 'px';
    latEl.style.top = (mobile ? lineY - 40 : lineY + 16) + 'px';
    const fade = 1 - ease(progress / 0.45);
    roughEl.style.opacity = fade;
    latEl.style.visibility = progress > 0.35 ? 'hidden' : '';
    const foot = stage.querySelector('.hero-foot');
    if (foot && document.body.classList.contains('ready')) foot.style.opacity = 1 - ease((progress - 0.25) / 0.4);
    const cue = stage.querySelector('.scroll-cue'); if (cue) cue.style.opacity = fade;
    if (pageRule) pageRule.classList.toggle('on', progress > 0.84);
  }

  /* ---------- Voice level: syllable-rate bursts while speaking ---------- */
  let speaking = false, level = 0, target = 0, sylPhase = 0;
  function updateLevel(dt, t) {
    if (speaking) {
      sylPhase += dt * (4.1 + Math.sin(t * 0.7) * 0.6) * Math.PI * 2;
      const syl = Math.pow(Math.max(0, Math.sin(sylPhase)), 1.6);
      target = 0.38 + 0.62 * syl * (0.75 + 0.25 * Math.sin(t * 1.9));
    } else target = 0;
    const k = target > level ? 18 : 3.2; // fast attack, slow settle
    level += (target - level) * Math.min(1, dt * k);
  }

  /* ---------- Choreography ---------- */
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  async function loop() {
    let i = 0;
    while (true) {
      const s = SCRIPTS[i++ % SCRIPTS.length];
      await perform(s);
    }
  }
  async function perform(s) {
    roughEl.innerHTML = '';
    const state = document.createElement('span');
    state.className = 'w in'; state.style.cssText = 'flex-basis:100%;color:var(--signal-text);font-size:12px;letter-spacing:.08em;text-transform:uppercase';
    state.textContent = '● Listening';
    roughEl.append(state);
    const words = s.rough.split(' ').map((w, idx) => {
      const el = document.createElement('span'); el.className = 'w'; el.textContent = w;
      el.dataset.filler = s.fillers.includes(idx) ? '1' : '';
      roughEl.append(el); return el;
    });
    speaking = true;
    for (const w of words) { w.classList.add('in'); await wait(150 + w.textContent.length * 26 + Math.random() * 70); }
    speaking = false;
    state.textContent = 'Settling';
    await wait(260);
    words.forEach((w) => { if (w.dataset.filler) w.classList.add('filler'); });
    await wait(520);
    const ms = 40 + Math.round(Math.random() * 38);
    latEl.textContent = `${ms} ms · ${s.dest}`;
    latEl.classList.add('show');
    words.forEach((w, k) => setTimeout(() => w.classList.add('out'), k * 28));
    state.textContent = 'Inserted';
    fairEl.classList.remove('fade');
    fairTxt.textContent = s.fair;
    fairEl.classList.add('show');
    await wait(3400);
    fairEl.classList.add('fade'); latEl.classList.remove('show');
    await wait(650);
    fairEl.classList.remove('show');
    await wait(300);
  }
  function showStatic(s) {
    // Reduced motion and screenshot mode: a settled, legible composition.
    roughEl.innerHTML = '<span class="w in" style="flex-basis:100%;color:var(--signal-text);font-size:12px;letter-spacing:.08em;text-transform:uppercase">● Settling</span>' +
      s.rough.split(' ').map((w, i) => `<span class="w in${s.fillers.includes(i) ? ' filler' : ''}">${w}</span>`).join('');
    fairTxt.textContent = s.fair; fairEl.classList.add('show');
    latEl.textContent = `58 ms · ${s.dest}`; latEl.classList.add('show');
  }

  /* ---------- WebGL2 ---------- */
  const gl = canvas.getContext('webgl2', { antialias: false, alpha: false, depth: false, stencil: false, powerPreference: 'low-power', preserveDrawingBuffer: false });
  const VS = `#version 300 es
  in vec2 p; void main(){ gl_Position = vec4(p, 0., 1.); }`;
  const FS = `#version 300 es
  precision highp float;
  uniform vec2 uRes; uniform float uTime, uLevel, uMargin, uLine, uCollapse, uDpr, uDark;
  uniform vec2 uPointer; uniform vec3 uBg, uInk, uInk2, uSig;
  out vec4 o;
  const int N = 16;
  float h1(float n){ return fract(sin(n * 91.345) * 47453.5453); }
  float vnoise(float x){ float i = floor(x), f = fract(x); float u = f*f*(3.-2.*f); return mix(h1(i), h1(i+1.), u) * 2. - 1.; }
  float strand(float x, float s, float A){
    float t = uTime;
    float y = sin(x * (.0055 + .004 * h1(s+1.)) + t * (.8 + .7 * h1(s+3.)) + s * 6.1) * .55
            + sin(x * (.016 + .011 * h1(s+2.)) - t * (1.6 + h1(s+4.)) + s * 2.7) * .3
            + vnoise(x * .011 + t * 1.2 + s * 9.7) * .38;
    return A * (y + (h1(s+5.) - .5) * 1.1);
  }
  void main(){
    vec2 p = vec2(gl_FragCoord.x, uRes.y - gl_FragCoord.y);
    float m = uMargin, L = uLine;
    vec3 col = uBg;
    float d0 = (m - p.x) / max(m, 1.);
    float env = smoothstep(0., .42, d0) * (.5 + .5 * clamp(d0, 0., 1.));
    float pull = exp(-dot(p - uPointer, p - uPointer) / (2. * pow(160. * uDpr, 2.)));
    float A = uRes.y * .15 * env * (.2 + .8 * uLevel + .35 * pull) * (1. - uCollapse);
    float ink = 0., glow = 0.;
    if (p.x < m + 2. * uDpr && abs(p.y - L) < A * 1.9 + 40. * uDpr) {
      for (int i = 0; i < N; i++){
        float s = float(i) * 1.731;
        float y = L + strand(p.x, s, A);
        float y2 = L + strand(p.x + uDpr, s, A);
        float slope = (y2 - y) / uDpr;
        float d = abs(p.y - y) / sqrt(1. + slope * slope);
        float w = (.45 + .55 * h1(s * 7.)) * uDpr;
        float a = .3 + .7 * h1(s * 3.1);
        ink += smoothstep(w + uDpr, w - .5 * uDpr, d) * a;
        glow += exp(-d * d / (2. * pow(9. * uDpr, 2.))) * a;
      }
    }
    // The fair line: one clean stroke from the margin to the edge.
    float fl = abs(p.y - L);
    float fair = smoothstep(1.3 * uDpr, .1 * uDpr, fl) * step(m, p.x) * (1. - smoothstep(uRes.x * .92, uRes.x, p.x) * .6);
    float fairGlow = exp(-fl * fl / (2. * pow(7. * uDpr, 2.))) * step(m, p.x);
    // Copybook ruling on the fair side, one rule every 64 px, aligned to the line.
    float rl = abs(mod(p.y - L + 32. * uDpr, 64. * uDpr) - 32. * uDpr);
    float ruling = smoothstep(.9 * uDpr, .0, rl) * step(m, p.x) * (1. - uCollapse * .5);
    // The margin rule and the caret spark where voice becomes text.
    float mr = smoothstep(1.1 * uDpr, .2 * uDpr, abs(p.x - m));
    float jr = length(p - vec2(m, L));
    float spark = exp(-jr * jr / (2. * pow((16. + 30. * uLevel) * uDpr, 2.)));
    float heat = clamp(1. - abs(p.x - m) / (uRes.x * .18), 0., 1.);
    vec3 strandCol = mix(uInk2, uInk, heat * heat);
    if (uDark > .5) {
      col += strandCol * min(ink, 1.6) * .75 + strandCol * glow * .055;
      col += uInk2 * ruling * .07;
      col += uInk * fair * .95 + uInk2 * fairGlow * .12;
      col = mix(col, uSig, mr * .85);
      col += uSig * spark * (.35 + .5 * uLevel);
    } else {
      col = mix(col, strandCol, clamp(min(ink, 1.4) * .62 + glow * .025, 0., .95));
      col = mix(col, uInk2, ruling * .09);
      col = mix(col, uInk, fair * .92);
      col = mix(col, uSig, mr * .9);
      col = mix(col, uSig, spark * (.1 + .22 * uLevel));
    }
    // Soft vignette and dither to keep gradients clean.
    vec2 q = gl_FragCoord.xy / uRes;
    col *= 1. - uDark * .25 * pow(length(q - vec2(.6, .55)), 2.2);
    col += (fract(sin(dot(gl_FragCoord.xy, vec2(12.9898, 78.233))) * 43758.5453) - .5) / 255.;
    o = vec4(col, 1.);
  }`;

  let prog, U = {};
  function initGL() {
    const sh = (type, src) => { const s = gl.createShader(type); gl.shaderSource(s, src); gl.compileShader(s); if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s)); return s; };
    prog = gl.createProgram();
    gl.attachShader(prog, sh(gl.VERTEX_SHADER, VS)); gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, FS));
    gl.bindAttribLocation(prog, 0, 'p'); gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
    gl.useProgram(prog);
    const buf = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0); gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    for (const n of ['uRes', 'uTime', 'uLevel', 'uMargin', 'uLine', 'uCollapse', 'uDpr', 'uDark', 'uPointer', 'uBg', 'uInk', 'uInk2', 'uSig']) U[n] = gl.getUniformLocation(prog, n);
  }
  const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);
  function themeColours() {
    const dark = document.documentElement.dataset.theme === 'dark';
    return dark ? { dark: 1, bg: hex('#0A0D12'), ink: hex('#F4F6FF'), ink2: hex('#6F86F0'), sig: hex('#FF7148') }
                : { dark: 0, bg: hex('#F3F1EA'), ink: hex('#0E1318'), ink2: hex('#2C4BD0'), sig: hex('#E5512B') };
  }
  let theme = null;
  const pointer = [-9999, -9999];
  stage.addEventListener('pointermove', (e) => { const r = stage.getBoundingClientRect(); pointer[0] = (e.clientX - r.left) * dpr; pointer[1] = (e.clientY - r.top) * dpr; });
  stage.addEventListener('pointerleave', () => { pointer[0] = pointer[1] = -9999; });

  function draw(t) {
    if (!gl) return;
    if (!theme) theme = themeColours();
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.uniform2f(U.uRes, canvas.width, canvas.height);
    gl.uniform1f(U.uTime, t);
    gl.uniform1f(U.uLevel, level);
    gl.uniform1f(U.uMargin, marginNow() * dpr);
    gl.uniform1f(U.uLine, lineY * dpr);
    gl.uniform1f(U.uCollapse, ease((progress - 0.05) / 0.75));
    gl.uniform1f(U.uDpr, dpr);
    gl.uniform1f(U.uDark, theme.dark);
    gl.uniform2f(U.uPointer, pointer[0], pointer[1]);
    gl.uniform3fv(U.uBg, theme.bg); gl.uniform3fv(U.uInk, theme.ink); gl.uniform3fv(U.uInk2, theme.ink2); gl.uniform3fv(U.uSig, theme.sig);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  /* Static SVG fallback when WebGL2 is unavailable. */
  function drawFallback() {
    const m = marginNow(), A = H * 0.11;
    let paths = '';
    for (let i = 0; i < 9; i++) {
      let d = '';
      for (let x = 0; x <= m; x += 6) {
        const env = Math.min(1, (m - x) / (m * 0.42)) * (0.5 + 0.5 * (m - x) / m);
        const y = lineY + A * env * (Math.sin(x * (0.006 + i * 0.0007) + i * 2.1) * 0.6 + Math.sin(x * 0.019 - i) * 0.3 + (i / 8 - 0.5) * 0.9);
        d += (x ? 'L' : 'M') + x.toFixed(1) + ' ' + y.toFixed(1);
      }
      paths += `<path d="${d}" fill="none" stroke="var(--accent)" stroke-opacity="${0.25 + (i % 3) * 0.2}" stroke-width="1.2"/>`;
    }
    fallback.setAttribute('viewBox', `0 0 ${W} ${H}`);
    fallback.innerHTML = paths + `<path d="M${m} ${lineY}H${W}" stroke="var(--fg)" stroke-width="1.4"/><path d="M${m} 0V${H}" stroke="var(--signal)" stroke-width="1.4"/>`;
  }

  /* ---------- Frame scheduling ---------- */
  let visible = true, running = false, pending = false, last = performance.now(), t0 = performance.now();
  function frame(now) {
    pending = false;
    const dt = Math.min(0.05, (now - last) / 1000); last = now;
    const t = (now - t0) / 1000;
    updateLevel(dt, t);
    draw(t);
    schedule();
  }
  function schedule() {
    if (!running || pending || !visible || document.hidden) return;
    pending = true;
    const go = () => requestAnimationFrame(frame);
    // Full rate when the window has focus, about 30 fps otherwise.
    if (document.hasFocus()) go(); else setTimeout(go, 33);
  }
  function onScroll() {
    const hero = stage.parentElement;
    const span = hero.offsetHeight - innerHeight;
    progress = Math.min(1, Math.max(0, -hero.getBoundingClientRect().top / Math.max(span, 1)));
    place();
    if (!running || reduce) { gl ? draw(fixedT ?? 0) : drawFallback(); }
  }

  function start() {
    layout();
    let ok = false;
    if (gl) { try { initGL(); ok = true; } catch (e) { console.warn('Hero shader unavailable, using the static fallback.', e); } }
    if (!ok) { canvas.remove(); drawFallback(); }
    else fallback.remove();
    document.body.classList.add('ready');
    if (reduce || fixedT !== null) {
      level = fixedT !== null ? 0.85 : 0.55;
      showStatic(SCRIPTS[params.has('script') ? +params.get('script') : 0]);
      if (ok) draw(fixedT ?? 2.0);
      return;
    }
    running = ok;
    schedule();
    loop();
  }

  addEventListener('resize', () => { layout(); if (!running) (gl ? draw(fixedT ?? 2) : drawFallback()); }, { passive: true });
  addEventListener('scroll', onScroll, { passive: true });
  document.addEventListener('visibilitychange', schedule);
  addEventListener('focus', schedule);
  new IntersectionObserver(([e]) => { visible = e.isIntersecting; schedule(); }).observe(stage);
  new MutationObserver(() => { theme = null; if (!running) gl ? draw(fixedT ?? 2) : drawFallback(); }).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

  // Lazy start: wait for fonts (so the copy is measured correctly) and the first idle moment.
  (document.fonts ? document.fonts.ready : Promise.resolve()).then(() => {
    (window.requestIdleCallback || ((f) => setTimeout(f, 1)))(start, { timeout: 400 });
  });
  window.FSHero = { get progress() { return progress; } };
})();
