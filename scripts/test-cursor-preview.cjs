// Run against `npm run dev`. Install Playwright separately or point
// PLAYWRIGHT_MODULE_PATH at an existing installation; no native app state is used.
const assert = require('node:assert/strict');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE_PATH || 'playwright');
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.CHROMIUM_EXECUTABLE_PATH, headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 400, height: 204 } });
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.addInitScript(() => {
      let id = 0;
      const callbacks = new Map(), listeners = new Map();
      window.previewInteractions = [];
      window.previewEmit = payload => (listeners.get('cursor-preview-state') || []).forEach(handler => callbacks.get(handler)?.({ payload }));
      window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
      window.__TAURI_INTERNALS__ = {
        metadata: { currentWindow: { label: 'cursor-preview' }, currentWebview: { label: 'cursor-preview' } },
        transformCallback(callback) { callbacks.set(++id, callback); return id; }, unregisterCallback() {},
        async invoke(command, args = {}) {
          if (command === 'plugin:event|listen') { listeners.set(args.event, [...(listeners.get(args.event) || []), args.handler]); return ++id; }
          if (command === 'get_cursor_preview_state') return { sessionId: 0, revision: 0, phase: 'idle', text: '', polished: false, remote: false };
          if (command === 'set_cursor_preview_interacting') window.previewInteractions.push(args);
        },
      };
    });
    await page.goto((process.env.MULTIVOICE_UI_URL || 'http://localhost:1420') + '/cursor-preview.html');
    const emit = payload => page.evaluate(payload => window.previewEmit(payload), payload);
    const canonical = () => page.locator('#previewText').evaluate(element => {
      const copy = element.cloneNode(true); copy.querySelectorAll('del').forEach(node => node.remove()); return copy.textContent;
    });
    const base = { sessionId: 1, revision: 1, phase: 'recording', text: 'um hello sam please send teh draft', polished: false, remote: false };
    await emit(base);
    await page.locator('#cursorPreview').waitFor();
    assert.equal(await page.locator('#previewStatus, #previewHint, .cursor-preview-heading').count(), 0);
    await emit({ ...base, text: 'Hello Sam, please send the draft.', polished: true });
    assert.equal(await canonical(), 'Hello Sam, please send the draft.');
    assert(await page.locator('mark').count() >= 2, 'separate edits are highlighted independently');
    assert(await page.locator('del').count() > 0, 'removed words remain briefly struck through');
    assert.equal(await page.locator('#previewAnnouncement').textContent(), 'Hello Sam, please send the draft.');
    await emit(base);
    assert.equal(await canonical(), 'Hello Sam, please send the draft.', 'late raw revision cannot undo polish');
    await emit({ ...base, revision: 2, text: base.text + ' tomorrow' });
    assert.equal(await canonical(), 'Hello Sam, please send the draft. tomorrow', 'new words retain the polished prefix');
    assert(await page.locator('del').count() > 0, 'append preserves edit lifetime');
    await emit({ ...base, revision: 2, text: 'Hello Sam, please send the draft tomorrow.', polished: true });
    assert.equal(await canonical(), 'Hello Sam, please send the draft tomorrow.');
    await page.waitForTimeout(3600);
    assert.equal(await page.locator('mark, del').count(), 0, 'marks and deleted text expire');
    assert.equal(await page.locator('#previewText').textContent(), 'Hello Sam, please send the draft tomorrow.');
    await emit({ ...base, revision: 2, phase: 'finishing', text: base.text + ' tomorrow' });
    assert.equal(await canonical(), 'Hello Sam, please send the draft tomorrow.', 'final raw input retains the polished prefix while the final pass runs');
    await emit({ ...base, revision: 2, phase: 'complete', text: 'Hello Sam, please send the draft tomorrow morning.', polished: true });
    assert.equal(await canonical(), 'Hello Sam, please send the draft tomorrow morning.');
    assert(await page.locator('mark').count() > 0, 'the final polish pass also highlights its changes');
    const long = Array.from({ length: 75 }, (_, i) => `Sentence ${i} stays available to read.`).join('\n');
    await emit({ ...base, sessionId: 2, text: long });
    const scroller = page.locator('#previewText');
    const bottom = await scroller.evaluate(e => e.scrollTop);
    assert(bottom > 0, 'long transcript scrolls');
    await scroller.hover(); await page.mouse.wheel(0, -250); await page.waitForTimeout(120);
    const position = await scroller.evaluate(e => e.scrollTop);
    assert(position < bottom, 'real wheel interaction scrolls the preview');
    await emit({ ...base, sessionId: 2, revision: 2, text: long + '\nOne more sentence.' });
    assert(Math.abs(await scroller.evaluate(e => e.scrollTop) - position) < 2, 'new text does not pull the reader to the bottom');
    assert(await page.evaluate(() => window.previewInteractions.some(e => e.active)), 'hover pauses native dismissal');
    await page.mouse.move(0, 0);
    assert(await page.evaluate(() => window.previewInteractions.some(e => !e.active)), 'leaving releases dismissal hold');
    await emit({ ...base, sessionId: 3, text: 'um delete this' });
    await emit({ ...base, sessionId: 3, text: 'Delete this.', polished: true });
    await emit({ ...base, sessionId: 4, text: '<img src=x onerror=alert(1)> stays literal' });
    assert.equal(await scroller.locator('img').count(), 0);
    await emit({ ...base, sessionId: 3, revision: 99, text: 'stale', polished: true });
    await page.waitForTimeout(3600);
    assert.equal(await canonical(), '<img src=x onerror=alert(1)> stays literal', 'old updates and timers cannot touch a new session');
    await emit({ ...base, sessionId: 4, phase: 'idle' });
    assert(await page.locator('#cursorPreview').isHidden());
    await emit({ ...base, sessionId: 4, revision: 999, text: 'stale completion', phase: 'complete', polished: true });
    assert(await page.locator('#cursorPreview').isHidden(), 'late completion cannot reopen idle preview');
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await emit({ ...base, sessionId: 5 });
    await emit({ ...base, sessionId: 5, text: 'Hello Sam, please send the draft.', polished: true });
    assert.equal(await page.locator('mark').first().evaluate(e => getComputedStyle(e).animationName), 'none');
    assert.deepEqual(errors, []);
    console.log('Cursor preview: edit lifecycle, append/revision/session guards, literal text, wheel scrolling, reading position, hover hold and reduced motion passed.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
