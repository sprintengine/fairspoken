// Regenerates every README image in docs/images.
//
//   npm i --no-save playwright-core          # once, at the repo root
//   node docs/images/src/render.mjs          # all images
//   node docs/images/src/render.mjs network  # or: network | app | host
//
// Uses the installed Google Chrome (Playwright channel "chrome"), renders at
// deviceScaleFactor 2, then optimises with pngquant + oxipng when on PATH.
// PRODUCT_NAME=Foo previews a rename: the network SVG is regenerated with it
// and visible "MultiVoice"/"multivoice" text in the app/dashboard screenshots
// is swapped in the rendered DOM (the shipped sources are not touched).

import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { startMockHost, SCENARIO_NOW, SCENARIO_TZ } from "./host-mock.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const out = resolve(here, "..");
const only = process.argv[2];
const PRODUCT_NAME = process.env.PRODUCT_NAME;

// Moment of the network animation captured in the static PNG fallback (ms).
const NETWORK_FRAME_MS = 4200;

async function loadPlaywright() {
  try { return await import("playwright-core"); } catch {}
  if (process.env.PLAYWRIGHT_DIR) return createRequire(resolve(process.env.PLAYWRIGHT_DIR, "noop.js"))("playwright-core");
  throw new Error("playwright-core not found: run `npm i --no-save playwright-core` at the repo root, or set PLAYWRIGHT_DIR to a folder whose node_modules has it.");
}
const { chromium } = await loadPlaywright();
const browser = await chromium.launch({ channel: "chrome" });

// Swap the product name in rendered text (all frames) for rename previews.
async function previewRename(page) {
  if (!PRODUCT_NAME) return;
  for (const frame of page.frames()) {
    await frame.evaluate((name) => {
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      for (let node; (node = walker.nextNode());) {
        node.nodeValue = node.nodeValue.replace(/MultiVoice/g, name).replace(/multivoice/g, name.toLowerCase());
      }
    }, PRODUCT_NAME).catch(() => {});
  }
}

const written = [];
async function shoot(locator, name) {
  const file = resolve(out, name);
  await locator.screenshot({ path: file, animations: "allow" });
  written.push(file);
}

async function network() {
  execFileSync(process.execPath, [resolve(here, "network.mjs")], { stdio: "inherit", env: process.env });
  for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width: 1200, height: 640 }, deviceScaleFactor: 2 });
    await page.goto(pathToFileURL(resolve(out, `network-${theme}.svg`)).href);
    await page.evaluate(async (t) => {
      await document.fonts.ready;
      for (const a of document.getAnimations()) { a.pause(); a.currentTime = t; }
    }, NETWORK_FRAME_MS);
    await shoot(page.locator("svg"), `network-${theme}.png`);
    await page.close();
  }
}

async function app() {
  const { createServer } = await import("vite");
  const vite = await createServer({
    root: repo, configFile: resolve(repo, "vite.config.ts"), logLevel: "error",
    server: { port: 5199, strictPort: false, hmr: false },
  });
  await vite.listen();
  const base = vite.resolvedUrls.local[0];
  try {
    for (const theme of ["light", "dark"]) {
      const ctx = await browser.newContext({ viewport: { width: 1500, height: 960 }, deviceScaleFactor: 2, colorScheme: theme });
      await ctx.addInitScript({ path: resolve(here, "tauri-stub.js") });
      const page = await ctx.newPage();
      await page.goto(`${base}docs/images/src/app-showcase.html?theme=${theme}`);
      await page.waitForFunction(() => window.__ready === true, null, { timeout: 30000 });
      await page.waitForTimeout(300); // let the select focus ring and fonts settle
      await previewRename(page);
      await shoot(page.locator("#shot"), `app-dictation-${theme}.png`);
      await ctx.close();
    }
  } finally {
    await vite.close();
  }
}

async function host() {
  const server = await startMockHost(0);
  const url = `http://127.0.0.1:${server.address().port}/`;
  try {
    for (const theme of ["light", "dark"]) {
      const ctx = await browser.newContext({ viewport: { width: 1200, height: 1240 }, deviceScaleFactor: 2, timezoneId: SCENARIO_TZ });
      const page = await ctx.newPage();
      await page.clock.setFixedTime(SCENARIO_NOW);
      await page.goto(`${pathToFileURL(resolve(here, "host-showcase.html")).href}?theme=${theme}&host=${encodeURIComponent(url)}`);
      await page.waitForFunction((u) => [...document.querySelectorAll("iframe")].some((f) => f.src === u), url);
      const frame = () => page.frames().find((f) => f.url().startsWith(url));
      for (let i = 0; i < 50 && !frame(); i++) await page.waitForTimeout(100);
      await frame().waitForSelector('#conn[data-state="live"]');
      await page.waitForTimeout(400);
      await previewRename(page);
      await shoot(page.locator("#shot"), `host-dashboard-${theme}.png`);
      await ctx.close();
    }
  } finally {
    server.close();
  }
}

if (!only || only === "network") await network();
if (!only || only === "app") await app();
if (!only || only === "host") await host();
await browser.close();

const has = (bin) => { try { execFileSync("which", [bin], { stdio: "ignore" }); return true; } catch { return false; } };
for (const file of written) {
  if (has("pngquant")) execFileSync("pngquant", ["--quality=82-96", "--speed=1", "--strip", "--force", "--ext", ".png", file]);
  if (has("oxipng")) execFileSync("oxipng", ["-o", "4", "--strip", "safe", "-q", file]);
  console.log(`${(statSync(file).size / 1024).toFixed(0).padStart(5)} KB  ${file.replace(repo + "/", "")}`);
}
