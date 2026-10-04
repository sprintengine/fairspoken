/* Liquid Glass for HTML mockups.
 *
 * Each element marked [data-glass] gets its own SVG filter, used through
 * `backdrop-filter: url(#…)`. The filter:
 *   1. softly blurs what is behind the element (frost; the "regular" variant blurs more),
 *   2. refracts it with a displacement map generated from the element's own rounded shape:
 *      flat in the middle, bending hard across a convex rim, like a thick lens edge,
 *   3. splits the refraction slightly per colour channel (dispersion at the rim),
 *   4. lifts saturation and brightness so the glass reads as a material, not a blur.
 * CSS (glass.css) then adds the specular rim light, inner edge light, tint and shadow.
 *
 * Works in Chromium only. Safari and WKWebView ignore url() in backdrop-filter, so the
 * native app must use SwiftUI's .glassEffect(); these files only set the target look.
 *
 * data-glass="regular|clear|prominent"   variant
 * data-bevel="18"   width of the refracting rim in px      data-refract="1"  strength multiplier
 */
(() => {
  const NS = 'http://www.w3.org/2000/svg';
  const defs = document.createElementNS(NS, 'svg');
  defs.setAttribute('width', '0'); defs.setAttribute('height', '0');
  defs.style.cssText = 'position:absolute;width:0;height:0;pointer-events:none';
  document.body.prepend(defs);
  let n = 0;
  const cache = new Map();

  function radiusOf(el, w, h) {
    const r = parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0;
    return Math.min(r, w / 2, h / 2);
  }

  // Displacement map: R/G encode where to sample from (0.5 = no shift).
  function map(w, h, r, bevel) {
    const key = [w, h, r, bevel].join('x');
    if (cache.has(key)) return cache.get(key);
    const c = document.createElement('canvas');
    c.width = w; c.height = h;
    const ctx = c.getContext('2d');
    const img = ctx.createImageData(w, h);
    const hw = w / 2, hh = h / 2;
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const px = x + 0.5 - hw, py = y + 0.5 - hh;
        const qx = Math.abs(px) - (hw - r), qy = Math.abs(py) - (hh - r);
        const ox = Math.max(qx, 0), oy = Math.max(qy, 0);
        const d = Math.hypot(ox, oy) + Math.min(Math.max(qx, qy), 0) - r; // < 0 inside
        let nx = 0, ny = 0;
        if (qx > 0 && qy > 0) { const l = Math.hypot(qx, qy) || 1; nx = qx / l; ny = qy / l; }
        else if (qx > qy) nx = 1; else ny = 1;
        nx *= Math.sign(px) || 1; ny *= Math.sign(py) || 1;
        const t = Math.min(Math.max(-d / bevel, 0), 1);       // 0 at the edge → 1 past the rim
        const m = Math.pow(1 - t, 2.1);                         // convex rim profile
        const centre = 0.06 * (1 - m);                          // gentle magnification inside
        const dx = -nx * m - (px / hw) * centre;                // sample inward → lensing
        const dy = -ny * m - (py / hh) * centre;
        const i = (y * w + x) * 4;
        img.data[i] = 128 + 127 * dx;
        img.data[i + 1] = 128 + 127 * dy;
        img.data[i + 2] = 128;
        img.data[i + 3] = 255;
      }
    }
    ctx.putImageData(img, 0, 0);
    const url = c.toDataURL();
    cache.set(key, url);
    return url;
  }

  function build(el) {
    const rect = el.getBoundingClientRect();
    const w = Math.round(el.offsetWidth || rect.width), h = Math.round(el.offsetHeight || rect.height);
    if (!w || !h) return;
    const variant = el.dataset.glass || 'regular';
    const r = radiusOf(el, w, h);
    const bevel = +(el.dataset.bevel || Math.min(22, Math.max(10, Math.min(w, h) * 0.32)));
    const k = +(el.dataset.refract || 1);
    const S = Math.round(bevel * 1.9 * k);
    const blur = variant === 'clear' ? 0.6 : variant === 'prominent' ? 2 : (+el.dataset.frost || 3.2);
    const sat = variant === 'clear' ? 1.35 : 1.7;
    const id = el.dataset.glassId || `lg${++n}`;
    el.dataset.glassId = id;
    let f = defs.querySelector('#' + id);
    if (!f) { f = document.createElementNS(NS, 'filter'); f.id = id; defs.append(f); }
    for (const [a, v] of Object.entries({ x: 0, y: 0, width: w, height: h, filterUnits: 'userSpaceOnUse', primitiveUnits: 'userSpaceOnUse', 'color-interpolation-filters': 'sRGB' })) f.setAttribute(a, v);
    f.innerHTML = `
      <feImage href="${map(w, h, r, bevel)}" x="0" y="0" width="${w}" height="${h}" preserveAspectRatio="none" result="map"/>
      <feGaussianBlur in="SourceGraphic" stdDeviation="${blur}" result="frost"/>
      <feDisplacementMap in="frost" in2="map" scale="${S}" xChannelSelector="R" yChannelSelector="G" result="dR"/>
      <feDisplacementMap in="frost" in2="map" scale="${Math.round(S * 0.94)}" xChannelSelector="R" yChannelSelector="G" result="dG"/>
      <feDisplacementMap in="frost" in2="map" scale="${Math.round(S * 0.88)}" xChannelSelector="R" yChannelSelector="G" result="dB"/>
      <feColorMatrix in="dR" type="matrix" values="1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0" result="r"/>
      <feColorMatrix in="dG" type="matrix" values="0 0 0 0 0 0 1 0 0 0 0 0 0 0 0 0 0 0 1 0" result="g"/>
      <feColorMatrix in="dB" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 0 0 0 1 0" result="b"/>
      <feBlend in="r" in2="g" mode="screen" result="rg"/>
      <feBlend in="rg" in2="b" mode="screen" result="rgb"/>
      <feColorMatrix in="rgb" type="saturate" values="${sat}"/>`;
    el.style.backdropFilter = `url(#${id})`;
    el.style.webkitBackdropFilter = `url(#${id})`;
  }

  function all() { document.querySelectorAll('[data-glass]').forEach(build); }
  window.LiquidGlass = { build, all };

  // Specular light follows the pointer (stands in for device motion on iPhone).
  addEventListener('pointermove', (e) => {
    const a = Math.atan2(e.clientY - innerHeight / 2, e.clientX - innerWidth / 2) * 180 / Math.PI + 180;
    document.documentElement.style.setProperty('--spec', `${a.toFixed(1)}deg`);
  }, { passive: true });

  const ro = new ResizeObserver((entries) => entries.forEach((e) => build(e.target)));
  const ready = () => { all(); document.querySelectorAll('[data-glass]').forEach((el) => ro.observe(el)); };
  (document.fonts ? document.fonts.ready : Promise.resolve()).then(ready);
})();
