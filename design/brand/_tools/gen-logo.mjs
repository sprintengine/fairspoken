// Generates the final Fairspoken logo files from the chosen mark (route A2, "Settle").
// node gen-logo.mjs   (needs PYTHONPATH pointing at fontTools for the wordmark outlines)
import { writeFileSync, mkdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { C, settlePath } from './marks.mjs';
const ROOT = new URL('../', import.meta.url).pathname;
const OUT = ROOT + 'logo/';
mkdirSync(OUT + 'app-icon', { recursive: true });

export const WAVE = settlePath({ x0: 7, x1: 40, x2: 45, amp: 15, cycles: 2.25 });
export const CARET = 'M53 20V44';
const SW = 4.5;
const S = `fill="none" stroke-linecap="round" stroke-linejoin="round" stroke-width="${SW}"`;
const markBody = (wave, caret) => `<path d="${WAVE}" ${S} stroke="${wave}"/><path d="${CARET}" ${S} stroke="${caret}"/>`;

// Wordmark outlines (Host Grotesk 600, -0.025em), from the self-hosted OFL font.
const wm = JSON.parse(execFileSync('python3', [ROOT + '_tools/outline_text.py', ROOT + 'site/fonts/host-grotesk-latin-wght-normal.woff2', 'Fairspoken', 'wght=600', 'tracking_em=-0.022'], { env: { ...process.env } }).toString());
// Optical kerning tweak for "Fa": pull the a under the F's arm.
const capH = wm.capHeight || 700;

const svg = (vb, body, title) => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${vb}" role="img" aria-label="${title}"><title>${title}</title>${body}</svg>\n`;

// --- Marks ---------------------------------------------------------------
const variants = {
  '': [C.blue, C.red], '-mono': [C.ink, C.ink], '-reversed': ['#FFFFFF', C.red400], '-mono-reversed': ['#FFFFFF', '#FFFFFF'],
};
for (const [suf, [w, c]] of Object.entries(variants)) {
  writeFileSync(OUT + `fairspoken-mark${suf}.svg`, svg('0 0 64 64', markBody(w, c), 'Fairspoken'));
}

// --- Wordmark and lockups --------------------------------------------------
// Lockup geometry on the mark's 64-unit grid: the wordmark cap height equals the caret's length (24 units).
const k = 24 / capH;            // font units → mark units
const wmW = wm.width * k;
const gap = 12;
const markRight = 53 + SW / 2;  // visible right edge of the caret
const markLeft = 7 - SW / 2;
const tx = markRight + gap;      // wordmark x
const baseline = 44;             // caret bottom = cap baseline
const W = Math.ceil(tx + wmW + 2);
const lockup = (w, c, t) => `<g transform="translate(${-markLeft} 0)">${markBody(w, c)}<path transform="translate(${tx.toFixed(2)} ${baseline}) scale(${k.toFixed(5)})" d="${wm.d}" fill="${t}"/></g>`;
const lockVB = `0 14 ${Math.ceil(W - markLeft)} 36`;
const lockVariants = { '': [C.blue, C.red, C.ink], '-mono': [C.ink, C.ink, C.ink], '-reversed': ['#FFFFFF', C.red400, '#FFFFFF'], '-mono-reversed': ['#FFFFFF', '#FFFFFF', '#FFFFFF'] };
for (const [suf, [w, c, t]] of Object.entries(lockVariants)) {
  writeFileSync(OUT + `fairspoken-lockup${suf}.svg`, svg(lockVB, lockup(w, c, t), 'Fairspoken'));
}
writeFileSync(OUT + 'fairspoken-wordmark.svg', svg(`0 ${-capH - 40} ${Math.ceil(wm.width)} ${capH + 260}`, `<path d="${wm.d}" fill="${C.ink}"/>`, 'Fairspoken'));
writeFileSync(OUT + 'fairspoken-wordmark-reversed.svg', svg(`0 ${-capH - 40} ${Math.ceil(wm.width)} ${capH + 260}`, `<path d="${wm.d}" fill="#FFFFFF"/>`, 'Fairspoken'));

// --- Favicon (adapts to the browser's colour scheme) ----------------------
writeFileSync(OUT + 'favicon.svg', `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><style>.w{stroke:${C.blue}}.c{stroke:${C.red}}.b{fill:${C.paper}}@media (prefers-color-scheme:dark){.w{stroke:#fff}.c{stroke:${C.red400}}.b{fill:${C.night}}}</style><rect class="b" width="64" height="64" rx="15"/><g transform="translate(32 32) scale(1.12) translate(-30 -32)"><path class="w" d="${WAVE}" fill="none" stroke-width="5.2" stroke-linecap="round" stroke-linejoin="round"/><path class="c" d="${CARET}" fill="none" stroke-width="5.2" stroke-linecap="round"/></g></svg>\n`);

// --- App icon layers (macOS 26 / Icon Composer: background, middle, foreground) ---
// 1024 canvas. Icon Composer applies the squircle mask, glass, specular and shadows itself;
// these flat layers are what you drop into it. The composed PNGs are previews only.
const s = 1024 / 64;
writeFileSync(OUT + 'app-icon/layer-0-background.svg', svg('0 0 1024 1024', `<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#F7F5EF"/><stop offset="1" stop-color="#E6E2D7"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#g)"/>`, 'Background: limestone paper'));
writeFileSync(OUT + 'app-icon/layer-0-background-dark.svg', svg('0 0 1024 1024', `<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#1A2030"/><stop offset="1" stop-color="#06080C"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#g)"/>`, 'Background: night'));
const rules = Array.from({ length: 7 }, (_, i) => `<path d="M0 ${248 + i * 88}H1024" stroke="#2C4BD0" stroke-opacity=".16" stroke-width="6"/>`).join('');
writeFileSync(OUT + 'app-icon/layer-1-ruling.svg', svg('0 0 1024 1024', rules, 'Middle: copybook ruling'));
writeFileSync(OUT + 'app-icon/layer-2-signal.svg', svg('0 0 1024 1024', `<g transform="translate(512 512) scale(${(s * 0.96).toFixed(3)}) translate(-30 -32)"><path d="${WAVE}" fill="none" stroke="${C.blue}" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/></g>`, 'Foreground: the signal'));
writeFileSync(OUT + 'app-icon/layer-3-caret.svg', svg('0 0 1024 1024', `<g transform="translate(512 512) scale(${(s * 0.96).toFixed(3)}) translate(-30 -32)"><path d="${CARET}" fill="none" stroke="${C.red}" stroke-width="5" stroke-linecap="round"/></g>`, 'Foreground: the caret'));
writeFileSync(OUT + '_meta.json', JSON.stringify({ WAVE, CARET, lockVB, wordmarkWidth: wm.width, capH }, null, 2));
console.log('logo files written', { W, wmW: wmW.toFixed(1) });
