// Generates the animated network hero (docs/images/network-{light,dark}.svg).
//
//   node docs/images/src/network.mjs
//
// One template, two themes. Inter (SIL OFL 1.1, from @fontsource/inter) is
// embedded as base64 so the SVG renders identically on every machine and in
// GitHub's <img> sandbox (no scripts, no external requests — inline <style>
// with CSS keyframes only). Motion honours prefers-reduced-motion.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ── Product name: the ONE place it appears in this file. ────────────────
const PRODUCT_NAME = process.env.PRODUCT_NAME || "Fairspoken";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const out = resolve(here, "..");

const font = (weight) =>
  readFileSync(resolve(repo, `node_modules/@fontsource/inter/files/inter-latin-${weight}-normal.woff2`)).toString("base64");
const FONT_FACES = [400, 500, 600]
  .map((w) => `@font-face{font-family:"Inter";font-style:normal;font-weight:${w};src:url(data:font/woff2;base64,${font(w)}) format("woff2")}`)
  .join("");

// Palette — taken from src/design-system/tokens.css so the diagram matches
// the app: ink/mist neutrals, the product green for "text back", and the
// violet→magenta activity glow (pill waveform, cursor preview rim) for audio.
const THEMES = {
  light: {
    bgTop: "#f8f9fb", bgBottom: "#edf0f3", frame: "rgba(32,36,40,0.08)",
    dot: "rgba(32,36,40,0.13)",
    glowA: "rgba(137,87,229,0.10)", glowB: "rgba(47,106,74,0.10)",
    card: "#ffffff", cardStroke: "rgba(32,36,40,0.10)", shadow: "rgba(16,20,24,0.10)",
    tile: "#f0f2f5", tileStroke: "rgba(32,36,40,0.06)", icon: "#3c444c",
    row: "#f5f6f8",
    text: "#202428", muted: "#646c78", subtle: "#80889a",
    wire: "rgba(32,36,40,0.13)",
    band: "rgba(47,106,74,0.035)", bandStroke: "rgba(47,106,74,0.30)",
    audioA: "#8957e5", audioB: "#d6409f",
    textA: "#2f8a5a", textB: "#2f6a4a",
    good: "#157f37",
  },
  dark: {
    bgTop: "#0e0f13", bgBottom: "#08080c", frame: "rgba(255,255,255,0.07)",
    dot: "rgba(255,255,255,0.075)",
    glowA: "rgba(163,113,247,0.13)", glowB: "rgba(63,148,104,0.13)",
    card: "#111418", cardStroke: "rgba(255,255,255,0.09)", shadow: "rgba(0,0,0,0.55)",
    tile: "#1a1d22", tileStroke: "rgba(255,255,255,0.06)", icon: "#c8c8d0",
    row: "#161a1f",
    text: "#ececec", muted: "#9c9ca4", subtle: "#80808a",
    wire: "rgba(255,255,255,0.11)",
    band: "rgba(63,148,104,0.05)", bandStroke: "rgba(77,175,125,0.34)",
    audioA: "#a371f7", audioB: "#f778ba",
    textA: "#4daf7d", textB: "#30d058",
    good: "#30d058",
  },
};

// 24×24 line glyphs, drawn to match the app's 1.6–1.7px icon stroke.
const ICONS = {
  laptop: '<rect x="4.5" y="5" width="15" height="10.5" rx="1.6"/><path d="M2.5 18.5h19"/>',
  imac: '<rect x="2.5" y="3.5" width="19" height="13.5" rx="2"/><path d="M2.5 13.6h19M10.2 17l-.6 3.4h4.8l-.6-3.4"/>',
  ipad: '<rect x="4.5" y="2.5" width="15" height="19" rx="2.4"/><path d="M11.2 5h1.6"/>',
  iphone: '<rect x="7" y="2.5" width="10" height="19" rx="2.6"/><path d="M10.7 5.1h2.6" stroke-width="1.9"/>',
  android: '<rect x="7" y="2.5" width="10" height="19" rx="1.8"/><circle cx="12" cy="5.3" r=".55" fill="currentColor"/><path d="M10.6 19h2.8"/>',
  desktop: '<rect x="2.5" y="4.5" width="13.5" height="10" rx="1.5"/><path d="M7.2 18.5h4.2M9.25 14.5v4"/><rect x="18" y="4.5" width="3.5" height="14" rx="1"/><path d="M19.75 7.3v.01"/>',
  macmini: '<rect x="2.5" y="8.5" width="19" height="7.5" rx="2.4"/><path d="M5.5 18.6h13"/><circle cx="18.4" cy="12.25" r=".6" fill="currentColor"/>',
  lock: '<rect x="5" y="10.5" width="14" height="10" rx="2.2"/><path d="M8.2 10.5V7.8a3.8 3.8 0 0 1 7.6 0v2.7"/><path d="M12 14.4v2.2"/>',
};

const DEVICES = [
  { name: "Dr Patel", kind: "MacBook Pro", icon: "laptop" },
  { name: "Consult Room 2", kind: "iMac", icon: "imac" },
  { name: "Reception", kind: "iPad", icon: "ipad" },
  { name: "Nurse Jones", kind: "Android phone", icon: "android" },
  { name: "Dr Okafor", kind: "iPhone", icon: "iphone" },
  { name: "Front desk", kind: "Windows or Linux PC", icon: "desktop" },
];

const WORKERS = [
  { name: "Parakeet TDT 0.6B v3", meta: "NVIDIA · speech · 25 languages", busy: 0 },
  { name: "Parakeet TDT 0.6B v2", meta: "NVIDIA · speech · English", busy: 1 },
  { name: "Whisper V3 Turbo", meta: "OpenAI · speech · multilingual", busy: 2 },
  { name: "Qwen3.5 2B", meta: "Qwen · text polish", busy: 3 },
];

const W = 1200, H = 640;
const CARD = { x: 48, w: 268, h: 60, top: 104, gap: 16 };
const BAND = { x: 500, w: 210, y: 40, h: 548 };
const HOST = { x: 872, y: 150, w: 280, h: 348 };
const PORT = { x: HOST.x, y: HOST.y + HOST.h / 2 };
const PERIOD = 6.6; // seconds per dictation cycle, per device
const OFFSETS = [0, 3.6, 1.4, 5.0, 2.5, 4.3]; // staggered, so traffic always looks concurrent

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");
const icon = (name, x, y, size, color, width = 1.6) =>
  `<g transform="translate(${x} ${y}) scale(${size / 24})" fill="none" stroke="${color}" stroke-width="${width}" stroke-linecap="round" stroke-linejoin="round" style="color:${color}">${ICONS[name]}</g>`;

function svg(themeName) {
  const t = THEMES[themeName];
  const cy = (i) => CARD.top + i * (CARD.h + CARD.gap) + CARD.h / 2;
  const wire = (i) => {
    const x0 = CARD.x + CARD.w, y0 = cy(i);
    return `M${x0} ${y0} C ${x0 + 230} ${y0}, ${PORT.x - 210} ${PORT.y}, ${PORT.x} ${PORT.y}`;
  };

  const devices = DEVICES.map((d, i) => {
    const y = CARD.top + i * (CARD.h + CARD.gap);
    const delay = -OFFSETS[i];
    return `
    <g class="device">
      <rect x="${CARD.x}" y="${y}" width="${CARD.w}" height="${CARD.h}" rx="12" fill="${t.card}" stroke="${t.cardStroke}" filter="url(#shadow)"/>
      <rect x="${CARD.x + 10}" y="${y + 10}" width="40" height="40" rx="9" fill="${t.tile}" stroke="${t.tileStroke}"/>
      ${icon(d.icon, CARD.x + 18, y + 18, 24, t.icon)}
      <text x="${CARD.x + 62}" y="${y + 26.5}" class="name">${esc(d.name)}</text>
      <text x="${CARD.x + 62}" y="${y + 44}" class="meta">${esc(d.kind)}</text>
      <g class="mic" style="animation-delay:${delay}s">
        ${[0, 1, 2, 3].map((b) => `<rect x="${CARD.x + CARD.w - 42 + b * 5}" y="${y + 24}" width="2.4" height="12" rx="1.2" fill="url(#audioGrad)" class="lvl lvl${b}" style="animation-delay:${delay - b * 0.13}s, ${delay}s"/>`).join("")}
      </g>
      <g class="done" style="animation-delay:${delay}s">
        <circle cx="${CARD.x + CARD.w - 30}" cy="${y + 30}" r="8" fill="${t.textA}" fill-opacity="0.14"/>
        <path d="M${CARD.x + CARD.w - 33.6} ${y + 30.2}l2.4 2.4 4.8-4.8" fill="none" stroke="${t.textB}" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>
      </g>
      <circle cx="${CARD.x + CARD.w}" cy="${cy(i)}" r="3.5" fill="${t.card}" stroke="${t.cardStroke}" stroke-width="1.2"/>
    </g>`;
  }).join("");

  const wires = DEVICES.map((_, i) => {
    const d = wire(i);
    const delay = -OFFSETS[i];
    return `
    <path d="${d}" class="wire" pathLength="100"/>
    <path d="${d}" class="c audio tail" pathLength="100" style="animation-delay:${delay}s"/>
    <path d="${d}" class="c audio head" pathLength="100" style="animation-delay:${delay}s"/>
    <path d="${d}" class="c text tail" pathLength="100" style="animation-delay:${delay}s"/>
    <path d="${d}" class="c text head" pathLength="100" style="animation-delay:${delay}s"/>`;
  }).join("");

  const rowsTop = HOST.y + 114;
  const workers = WORKERS.map((wk, i) => {
    const y = rowsTop + i * 46;
    const bx = HOST.x + HOST.w - 44;
    const delay = -(i * 1.7);
    return `
      <rect x="${HOST.x + 12}" y="${y}" width="${HOST.w - 24}" height="40" rx="9" fill="${t.row}"/>
      <text x="${HOST.x + 24}" y="${y + 17.5}" class="wname">${esc(wk.name)}</text>
      <text x="${HOST.x + 24}" y="${y + 32}" class="wmeta">${esc(wk.meta)}</text>
      <g class="busy" style="animation-delay:${delay}s">
        ${[0, 1, 2].map((b) => `<rect x="${bx + b * 6}" y="${y + 14}" width="3" height="12" rx="1.5" fill="url(#audioGrad)" class="bar" style="animation-delay:${delay - b * 0.18}s"/>`).join("")}
      </g>
      <circle cx="${bx + 7.5}" cy="${y + 20}" r="3" fill="${t.subtle}" fill-opacity="0.5" class="idle" style="animation-delay:${delay}s"/>`;
  }).join("");

  // Legend (centred; widths measured against embedded Inter 12.5px).
  const legend = [
    { kind: "audio", label: "Audio streams in" },
    { kind: "text", label: "Text comes back" },
    { kind: "lock", label: "Encrypted with Tailscale" },
  ];

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" role="img" aria-labelledby="title desc">
  <title id="title">Every device dictates through one self-hosted transcription host</title>
  <desc id="desc">Phones, tablets, laptops and desktops stream audio over an encrypted Tailscale tailnet to one always-on Mac mini or Linux box running speech and polish models; transcripts flow back to each device.</desc>
  <defs>
    <style>
      ${FONT_FACES}
      text { font-family: "Inter", -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; font-feature-settings: "cv11", "ss01"; }
      .name  { font-size: 14px; font-weight: 600; fill: ${t.text}; letter-spacing: -0.01em; }
      .meta  { font-size: 12.5px; font-weight: 400; fill: ${t.muted}; }
      .eyebrow { font-size: 11px; font-weight: 600; fill: ${t.subtle}; letter-spacing: 0.09em; }
      .title { font-size: 16.5px; font-weight: 600; fill: ${t.text}; letter-spacing: -0.015em; }
      .wname { font-size: 13px; font-weight: 500; fill: ${t.text}; letter-spacing: -0.005em; }
      .wmeta { font-size: 11.5px; font-weight: 400; fill: ${t.muted}; }
      .mono  { font-family: ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace; font-size: 11.5px; fill: ${t.muted}; }
      .band-title { font-size: 13.5px; font-weight: 600; fill: ${t.text}; letter-spacing: -0.01em; }
      .band-meta { font-size: 12px; fill: ${t.muted}; }
      .legend { font-size: 12.5px; fill: ${t.muted}; }

      .wire { fill: none; stroke: ${t.wire}; stroke-width: 1.4; }
      .c { fill: none; stroke-linecap: round; animation-duration: ${PERIOD}s; animation-timing-function: cubic-bezier(.45,.05,.55,.95); animation-iteration-count: infinite; }
      .c.audio { stroke: url(#audioGrad); }
      .c.text  { stroke: url(#textGrad); }
      .c.tail  { stroke-width: 3.2; stroke-dasharray: 16 300; opacity: .4; }
      .c.head  { stroke-width: 3.2; stroke-dasharray: 5 300; filter: url(#glow); }
      .c.audio.tail { animation-name: audio-tail; }
      .c.audio.head { animation-name: audio-head; }
      .c.text.tail  { animation-name: text-tail; }
      .c.text.head  { animation-name: text-head; }
      /* Audio travels device → host over the first third of the cycle; the
         transcript returns host → device in the second half. */
      @keyframes audio-tail { 0% { stroke-dashoffset: 16; } 34%, 100% { stroke-dashoffset: -101; } }
      @keyframes audio-head { 0% { stroke-dashoffset: 5; } 34%, 100% { stroke-dashoffset: -112; } }
      @keyframes text-tail  { 0%, 46% { stroke-dashoffset: -101; } 74%, 100% { stroke-dashoffset: 16; } }
      @keyframes text-head  { 0%, 46% { stroke-dashoffset: -101; } 74%, 100% { stroke-dashoffset: 6; } }

      .mic, .done, .busy, .idle { animation: ${PERIOD}s linear infinite; }
      .mic  { animation-name: show-mic; }
      .done { animation-name: show-done; }
      @keyframes show-mic  { 0%, 30% { opacity: 1; } 36%, 100% { opacity: 0; } }
      @keyframes show-done { 0%, 72% { opacity: 0; } 77%, 95% { opacity: 1; } 100% { opacity: 0; } }
      .lvl { transform-box: fill-box; transform-origin: center; animation: level .52s ease-in-out infinite alternate, show-mic ${PERIOD}s linear infinite; }
      @keyframes level { from { transform: scaleY(.3); } to { transform: scaleY(1); } }

      .busy { animation-name: busy-on; }
      .idle { animation-name: busy-off; }
      @keyframes busy-on  { 0%, 62% { opacity: 1; } 63.5%, 94% { opacity: 0; } 95.5%, 100% { opacity: 1; } }
      @keyframes busy-off { 0%, 62% { opacity: 0; } 63.5%, 94% { opacity: 1; } 95.5%, 100% { opacity: 0; } }
      .bar { transform-box: fill-box; transform-origin: center; animation: level .46s ease-in-out infinite alternate; }

      .ring { transform-box: fill-box; transform-origin: center; animation: ring 2.2s ease-out infinite; }
      @keyframes ring { 0% { transform: scale(.6); opacity: .55; } 100% { transform: scale(2.6); opacity: 0; } }
      .live { animation: live 2.2s ease-in-out infinite; }
      @keyframes live { 50% { opacity: .45; } }

      @media (prefers-reduced-motion: reduce) {
        *, *::before, *::after { animation: none !important; }
        .c { opacity: 0 !important; }
        .mic, .done, .idle { opacity: 0; }
      }
    </style>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${t.bgTop}"/><stop offset="1" stop-color="${t.bgBottom}"/></linearGradient>
    <radialGradient id="glowA" cx="0.16" cy="0.55" r="0.42"><stop offset="0" stop-color="${t.glowA}"/><stop offset="1" stop-color="${t.glowA}" stop-opacity="0"/></radialGradient>
    <radialGradient id="glowB" cx="0.84" cy="0.5" r="0.36"><stop offset="0" stop-color="${t.glowB}"/><stop offset="1" stop-color="${t.glowB}" stop-opacity="0"/></radialGradient>
    <radialGradient id="fade" cx="0.5" cy="0.5" r="0.62"><stop offset="0.35" stop-color="#fff"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></radialGradient>
    <mask id="dotMask"><rect width="${W}" height="${H}" fill="url(#fade)"/></mask>
    <pattern id="dots" width="20" height="20" patternUnits="userSpaceOnUse"><circle cx="10" cy="10" r="1" fill="${t.dot}"/></pattern>
    <linearGradient id="audioGrad" gradientUnits="userSpaceOnUse" x1="${CARD.x + CARD.w}" y1="0" x2="${PORT.x}" y2="0"><stop offset="0" stop-color="${t.audioA}"/><stop offset="1" stop-color="${t.audioB}"/></linearGradient>
    <linearGradient id="textGrad" gradientUnits="userSpaceOnUse" x1="${CARD.x + CARD.w}" y1="0" x2="${PORT.x}" y2="0"><stop offset="0" stop-color="${t.textB}"/><stop offset="1" stop-color="${t.textA}"/></linearGradient>
    <filter id="shadow" x="-20%" y="-30%" width="140%" height="180%">
      <feDropShadow dx="0" dy="1" stdDeviation="1" flood-color="${t.shadow}" flood-opacity="0.6"/>
      <feDropShadow dx="0" dy="8" stdDeviation="10" flood-color="${t.shadow}" flood-opacity="0.55"/>
    </filter>
    <filter id="glow" x="-50%" y="-50%" width="200%" height="200%">
      <feGaussianBlur stdDeviation="2.4" result="b"/>
      <feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
  </defs>

  <!-- Backdrop -->
  <rect x="0.5" y="0.5" width="${W - 1}" height="${H - 1}" rx="20" fill="url(#bg)" stroke="${t.frame}"/>
  <rect width="${W}" height="${H}" rx="20" fill="url(#glowA)"/>
  <rect width="${W}" height="${H}" rx="20" fill="url(#glowB)"/>
  <rect width="${W}" height="${H}" rx="20" fill="url(#dots)" mask="url(#dotMask)"/>

  <!-- Tailnet band -->
  <rect x="${BAND.x}" y="${BAND.y}" width="${BAND.w}" height="${BAND.h}" rx="22" fill="${t.band}" stroke="${t.bandStroke}" stroke-dasharray="4 5"/>
  ${icon("lock", BAND.x + BAND.w / 2 - 9, BAND.y + 18, 18, t.good, 1.8)}
  <text x="${BAND.x + BAND.w / 2}" y="${BAND.y + 56}" text-anchor="middle" class="band-title">Tailscale tailnet</text>
  <text x="${BAND.x + BAND.w / 2}" y="${BAND.y + 73}" text-anchor="middle" class="band-meta">WireGuard-encrypted</text>
  <text x="${BAND.x + BAND.w / 2}" y="${BAND.y + BAND.h - 22}" text-anchor="middle" class="band-meta">Home, practice or on the road</text>

  <!-- Column labels -->
  <text x="${CARD.x + 2}" y="84" class="eyebrow">EVERY DEVICE</text>
  <text x="${HOST.x + 2}" y="${HOST.y - 20}" class="eyebrow">ONE ALWAYS-ON HOST</text>

  <!-- Wires + traffic -->
  <g>${wires}
  </g>

  <!-- Devices -->
  ${devices}

  <!-- Host -->
  <g class="host">
    <rect x="${HOST.x}" y="${HOST.y}" width="${HOST.w}" height="${HOST.h}" rx="14" fill="${t.card}" stroke="${t.cardStroke}" filter="url(#shadow)"/>
    <rect x="${HOST.x + 14}" y="${HOST.y + 16}" width="44" height="44" rx="10" fill="${t.tile}" stroke="${t.tileStroke}"/>
    ${icon("macmini", HOST.x + 22, HOST.y + 24, 28, t.icon)}
    <text x="${HOST.x + 70}" y="${HOST.y + 35}" class="title"><tspan id="product-name">${PRODUCT_NAME}</tspan> host</text>
    <text x="${HOST.x + 70}" y="${HOST.y + 53}" class="meta">Mac mini or Linux box</text>
    <circle cx="${HOST.x + HOST.w - 22}" cy="${HOST.y + 30}" r="3.5" fill="${t.good}" class="live"/>
    <line x1="${HOST.x}" y1="${HOST.y + 76}" x2="${HOST.x + HOST.w}" y2="${HOST.y + 76}" stroke="${t.cardStroke}"/>
    <text x="${HOST.x + 16}" y="${HOST.y + 100}" class="eyebrow">WORKERS</text>
    <text x="${HOST.x + HOST.w - 16}" y="${HOST.y + 100}" text-anchor="end" class="eyebrow">${WORKERS.length} MODELS</text>
    ${workers}
    <line x1="${HOST.x}" y1="${HOST.y + HOST.h - 42}" x2="${HOST.x + HOST.w}" y2="${HOST.y + HOST.h - 42}" stroke="${t.cardStroke}"/>
    <text x="${HOST.x + 16}" y="${HOST.y + HOST.h - 17}" class="mono">mac-mini.tail1234.ts.net</text>
  </g>
  <circle cx="${PORT.x}" cy="${PORT.y}" r="5" fill="none" stroke="${t.audioB}" stroke-width="1.5" class="ring"/>
  <circle cx="${PORT.x}" cy="${PORT.y}" r="4.5" fill="${t.card}" stroke="${t.audioB}" stroke-width="1.6"/>

  <!-- Legend -->
  <g transform="translate(${W / 2 - 323} ${H - 26})">
    <line x1="0" y1="-4" x2="22" y2="-4" stroke="url(#legendAudio)" stroke-width="3" stroke-linecap="round"/>
    <text x="32" y="0" class="legend">${legend[0].label}</text>
    <line x1="160" y1="-4" x2="182" y2="-4" stroke="${t.textA}" stroke-width="3" stroke-linecap="round"/>
    <text x="192" y="0" class="legend">${legend[1].label}</text>
    ${icon("lock", 318, -12.5, 15, t.good, 1.9)}
    <text x="340" y="0" class="legend">${legend[2].label}</text>
  </g>
  <defs><linearGradient id="legendAudio" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="22" y2="0"><stop offset="0" stop-color="${t.audioA}"/><stop offset="1" stop-color="${t.audioB}"/></linearGradient></defs>
</svg>
`;
}

for (const theme of Object.keys(THEMES)) {
  const file = resolve(out, `network-${theme}.svg`);
  writeFileSync(file, svg(theme));
  console.log("wrote", file);
}
