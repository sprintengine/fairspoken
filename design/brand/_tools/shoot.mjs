// Screenshot helper for the brand prototypes (Playwright + local Chrome, 2x).
// node shoot.mjs <file-or-url> <out.png> [--w 1440] [--h 900] [--theme light|dark] [--scroll <px|selector>] [--offset px]
//                [--wait ms] [--full] [--reduced] [--steps n]   (--steps scrolls gradually to trigger scroll-driven effects)
import { chromium } from 'playwright';
const a = process.argv.slice(2);
const opt = (k, d) => { const i = a.indexOf('--' + k); return i < 0 ? d : (a[i + 1] && !a[i + 1].startsWith('--') ? a[i + 1] : true); };
const [src, out] = a;
const url = /^(https?|file):/.test(src) ? src : 'file://' + src;
const b = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true, args: ['--headless=new', '--enable-gpu', '--use-angle=metal', '--ignore-gpu-blocklist', '--enable-unsafe-swiftshader'] });
const ctx = await b.newContext({ viewport: { width: +opt('w', 1440), height: +opt('h', 900) }, deviceScaleFactor: +opt('scale', 2), colorScheme: opt('theme', 'light'), reducedMotion: opt('reduced', false) ? 'reduce' : 'no-preference' });
const p = await ctx.newPage();
p.on('console', (m) => { if (['error', 'warning'].includes(m.type())) console.log('console.' + m.type(), m.text()); });
p.on('pageerror', (e) => console.log('PAGEERROR', e.message));
const theme = opt('theme', 'light');
await p.addInitScript((t) => localStorage.setItem('fs-theme', t), theme);
await p.goto(url, { waitUntil: 'load' });
await p.waitForTimeout(600);
const sc = opt('scroll', null);
if (sc !== null) {
  const off = +opt('offset', 0);
  const y = isNaN(+sc) ? await p.evaluate(([s, o]) => document.querySelector(s).getBoundingClientRect().top + scrollY + o, [sc, off]) : +sc + off;
  const steps = +opt('steps', 12);
  for (let i = 1; i <= steps; i++) { await p.evaluate((yy) => window.scrollTo(0, yy), (y * i) / steps); await p.waitForTimeout(40); }
}
await p.waitForTimeout(+opt('wait', 1500));
await p.screenshot({ path: out, fullPage: !!opt('full', false) });
await b.close();
