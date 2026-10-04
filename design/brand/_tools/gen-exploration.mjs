import { writeFileSync } from 'node:fs';
import { C, settlePath, roughPath, quotePath } from './marks.mjs';
const OUT = new URL('../logo/exploration/', import.meta.url).pathname;
const svg = (body, bg = 'none') => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">${bg !== 'none' ? `<rect width="64" height="64" rx="14" fill="${bg}"/>` : ''}${body}</svg>`;
const S = 'fill="none" stroke-linecap="round" stroke-linejoin="round"';
const marks = {
  'a-settle': `<path d="${settlePath()}" ${S} stroke="${C.blue}" stroke-width="5"/><path d="M55 19V45" ${S} stroke="${C.red}" stroke-width="5"/>`,
  'b-rough-fair': `<path d="M11 8V56" ${S} stroke="${C.red}" stroke-width="3"/><path d="${roughPath()}" ${S} stroke="${C.blue}" stroke-width="3.2" opacity=".55"/><path d="M18 42H54" ${S} stroke="${C.blue}" stroke-width="5"/>`,
  'c-margin-f': `<path d="M27 56V24C27 15 32 10 40 10C45 10 48 12.5 50 15" ${S} stroke="${C.red}" stroke-width="5.5"/><path d="M14 32H53" ${S} stroke="${C.blue}" stroke-width="5.5"/>`,
  'd-quote-line': `<path d="${quotePath}" ${S} stroke="${C.blue}" stroke-width="5"/><circle cx="54" cy="42" r="0.01" stroke="${C.red}" stroke-width="7" stroke-linecap="round"/>`,
};
for (const [k, body] of Object.entries(marks)) {
  writeFileSync(OUT + `mark-${k}.svg`, svg(body));
}
// Contact sheet: each mark at 160px on paper, 32px, 16px, and reversed on night.
const rev = (s) => s.replaceAll(C.blue, C.mist).replaceAll(C.red, C.red400);
const cell = (k, body) => `
<figure><div class="big">${svg(body)}</div>
<div class="row"><div class="tile">${svg(body, C.paper)}</div><div class="s32">${svg(body)}</div><div class="s16">${svg(body)}</div><div class="rev">${svg(rev(body), C.night)}</div></div>
<figcaption>${k.toUpperCase().replace('-', '. ').replaceAll('-', ' ')}</figcaption></figure>`;
writeFileSync(OUT + 'exploration.html', `<!doctype html><meta charset="utf-8"><title>Fairspoken mark exploration</title><style>
@font-face{font-family:Host;src:url(../../site/fonts/host-grotesk-latin-wght-normal.woff2);font-weight:300 800}
@font-face{font-family:Mono;src:url(../../site/fonts/atkinson-hyperlegible-mono-latin-wght-normal.woff2);font-weight:200 800}
body{margin:0;background:${C.paper};color:${C.ink};font-family:Host;padding:56px 64px}
h1{font-weight:600;font-size:34px;letter-spacing:-.02em;margin:0 0 6px}p{margin:0 0 36px;color:#3D4752;font-size:17px;max-width:70ch}
.grid{display:grid;grid-template-columns:repeat(4,1fr);gap:28px}
figure{margin:0;background:#FBFAF6;border:1px solid ${C.rule};border-radius:20px;padding:24px}
.big svg{width:100%;height:auto;aspect-ratio:1;display:block;background:repeating-linear-gradient(to bottom,transparent 0 31px,${C.rule}66 31px 32px)}
.row{display:flex;gap:14px;align-items:center;margin-top:18px}.tile svg{width:64px;height:64px}.s32 svg{width:32px;height:32px}.s16 svg{width:16px;height:16px}.rev svg{width:64px;height:64px}
figcaption{font:600 13px Mono;letter-spacing:.06em;margin-top:16px;color:#3D4752}
</style><h1>Mark exploration</h1><p>Four routes from “rough work” to “fair copy”. Each is shown large on a ruled page, then as a tile, at 32 px, at 16 px and reversed.</p><div class="grid">${Object.entries(marks).map(([k, b]) => cell(k, b)).join('')}</div>`);
console.log('ok');
