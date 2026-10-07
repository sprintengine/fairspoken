// Run against a local Vite server; native IPC is replaced with in-memory fixtures.
// Checks the Dictionary screen's Packs section: listing, enable switch, term preview.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE_PATH || 'playwright');
const assert = require('node:assert/strict');
let browser;
(async () => {
  browser = await chromium.launch({ executablePath: process.env.CHROMIUM_EXECUTABLE_PATH, headless: true });
  const page = await browser.newPage({ viewport: { width: 1000, height: 900 } });
  await page.addInitScript(() => {
    const callbacks = new Map(); let next = 1;
    window.smokeSaved = [];
    window.smokeSettings = { vocabularyHints: ['Hypercube'], transcriptCorrections: [], snippets: [], enabledPacks: ['software-engineering'] };
    const packs = [
      { id: 'ie-general-practice', name: 'Irish general practice', description: 'Medicines authorised in Ireland.', version: '2026.10.06.1', licence: 'CC-BY-4.0', attribution: 'Contains information from the HPRA.', sources: [{ name: 'HPRA', url: '', licence: 'CC-BY-4.0', used_for: '' }], termCount: 4737, alwaysOn: [] },
      { id: 'software-engineering', name: 'Software engineering', description: 'Languages, frameworks and tools.', version: '2026.10.1', licence: 'MIT', attribution: 'Curated by the Fairspoken contributors.', sources: [{ name: 'Curated', url: '', licence: 'MIT', used_for: '' }], termCount: 440, alwaysOn: [] },
    ];
    const terms = [{ term: 'kubectl', category: 'identifier', spokenForms: ['cube control'] }, { term: 'PostgreSQL', category: 'product', spokenForms: ['postgres'] }];
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    window.__TAURI_INTERNALS__ = {
      transformCallback: cb => { const id = next++; callbacks.set(id, cb); return id; }, unregisterCallback: () => {},
      metadata: { currentWindow: { label: 'home' }, currentWebview: { label: 'home' } },
      invoke: async (cmd, args = {}) => {
        if (cmd === 'plugin:event|listen') return next++;
        if (cmd === 'get_settings') return { ...window.smokeSettings };
        if (cmd === 'list_vocabulary_packs') return packs;
        if (cmd === 'search_vocabulary_pack') return terms.filter(t => !args.query || t.term.toLowerCase().includes(args.query.toLowerCase()));
        if (cmd === 'save_dictionary') { window.smokeSaved.push(args.update); return; }
        if (cmd === 'get_transcript_history' || cmd === 'get_notes' || cmd === 'get_learned_suggestions') return [];
        return {};
      },
    };
  });
  await page.goto((process.env.FAIRSPOKEN_UI_URL || 'http://localhost:1420') + '/home.html');
  await page.evaluate(() => document.querySelector('[data-route="dictionary"]')?.click());
  const rows = page.locator('#packRows .row');
  await rows.first().waitFor();
  assert.equal(await rows.count(), 2);
  assert.match(await rows.nth(0).innerText(), /Irish general practice[\s\S]*4,737 terms · CC-BY-4.0/);
  const gp = page.getByRole('switch', { name: 'Irish general practice' });
  const se = page.getByRole('switch', { name: 'Software engineering' });
  assert.equal(await gp.isChecked(), false);
  assert.equal(await se.isChecked(), true);
  await gp.check();
  await page.waitForFunction(() => window.smokeSaved.length === 1);
  const saved = await page.evaluate(() => window.smokeSaved[0]);
  assert.deepEqual(saved.enabledPacks, ['software-engineering', 'ie-general-practice']);
  assert.deepEqual(saved.vocabularyHints, ['Hypercube']);
  const browse = rows.nth(1).getByRole('button', { name: 'Browse terms' });
  await browse.click();
  assert.equal(await browse.getAttribute('aria-expanded'), 'true');
  const detail = page.locator('#pack-detail-software-engineering');
  await detail.locator('.chip', { hasText: 'kubectl' }).waitFor();
  await detail.getByRole('searchbox').fill('postgre');
  await page.waitForFunction(() => document.querySelectorAll('#pack-detail-software-engineering .chip').length === 1);
  if (process.env.SCREENSHOT) await page.locator('#screen-dictionary').screenshot({ path: process.env.SCREENSHOT });
  console.log('dictionary packs: ok');
  await browser.close();
})().catch(async error => { console.error(error); await browser?.close(); process.exit(1); });
