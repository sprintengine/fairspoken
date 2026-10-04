// Headless-Chrome screenshot helper for the design concepts (CDP over the
// built-in WebSocket in Node >= 22). No dependencies.
//
//   node shot.mjs <url> <out.png> [--dark] [--width 980] [--height 680]
//                 [--scale 2] [--stub] [--wait 1200] [--eval "js"]
//
// --stub injects a fake window.__TAURI_INTERNALS__ so the real home.html can
// render outside Tauri with plausible data.
import { spawn } from "node:child_process";
import { writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const args = process.argv.slice(2);
const url = args[0];
const out = args[1];
const flag = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  if (i < 0) return fallback;
  const v = args[i + 1];
  return v === undefined || v.startsWith("--") ? true : v;
};
const dark = Boolean(flag("dark", false));
const width = Number(flag("width", 980));
const height = Number(flag("height", 680));
const scale = Number(flag("scale", 2));
const wait = Number(flag("wait", 1500));
const stub = Boolean(flag("stub", false));
const evalJs = flag("eval", "");
const reduced = Boolean(flag("reduced", false));

const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const port = 9300 + Math.floor(Math.random() * 500);
const profile = mkdtempSync(join(tmpdir(), "mv-shot-"));
const chrome = spawn(CHROME, [
  "--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  "--no-first-run", "--no-default-browser-check", "--hide-scrollbars",
  "--enable-gpu", "--use-angle=metal", "--enable-unsafe-swiftshader", "about:blank",
], { stdio: "ignore" });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let target;
for (let i = 0; i < 50 && !target; i++) {
  await sleep(150);
  try {
    const list = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    target = list.find((t) => t.type === "page");
  } catch { /* chrome still starting */ }
}
if (!target) { chrome.kill(); throw new Error("Chrome did not start"); }

const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let seq = 0;
const pending = new Map();
const logs = [];
ws.addEventListener("message", (event) => {
  const msg = JSON.parse(event.data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  if (msg.method === "Runtime.exceptionThrown") logs.push("EXC " + (msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text));
  if (msg.method === "Runtime.consoleAPICalled" && ["error", "warning"].includes(msg.params.type)) logs.push(msg.params.type + " " + msg.params.args.map((a) => a.value ?? a.description).join(" "));
});
const send = (method, params = {}) => new Promise((resolve) => {
  const id = ++seq; pending.set(id, resolve); ws.send(JSON.stringify({ id, method, params }));
});

await send("Runtime.enable");
await send("Page.enable");
await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: scale, mobile: false });
await send("Emulation.setEmulatedMedia", { features: [
  { name: "prefers-color-scheme", value: dark ? "dark" : "light" },
  { name: "prefers-reduced-motion", value: reduced ? "reduce" : "no-preference" },
] });
if (stub) {
  await send("Page.addScriptToEvaluateOnNewDocument", { source: STUB() });
}
await send("Page.navigate", { url });
await sleep(wait);
if (evalJs) {
  const r = await send("Runtime.evaluate", { expression: evalJs, awaitPromise: true });
  if (r.result?.exceptionDetails) logs.push("EVAL " + JSON.stringify(r.result.exceptionDetails.exception?.description));
  await sleep(Number(flag("after", 600)));
}
const perfSecs = Number(flag("perf", 0));
if (perfSecs) {
  // Main-thread busy time of the renderer over the window (excludes the GPU process).
  await send("Performance.enable", { timeDomain: "timeTicks" });
  const metric = async () => Object.fromEntries((await send("Performance.getMetrics")).result.metrics.map((m) => [m.name, m.value]));
  const a = await metric(); await sleep(perfSecs * 1000); const b = await metric();
  const busy = (b.TaskDuration - a.TaskDuration) / (b.Timestamp - a.Timestamp);
  const script = (b.ScriptDuration - a.ScriptDuration) / (b.Timestamp - a.Timestamp);
  console.log(`perf ${perfSecs}s: renderer main-thread busy ${(busy * 100).toFixed(2)}% · script ${(script * 100).toFixed(2)}%`);
}
const shot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
writeFileSync(out, Buffer.from(shot.result.data, "base64"));
if (logs.length) console.log(logs.slice(0, 15).join("\n"));
console.log("saved", out);
ws.close();
chrome.kill();

function STUB() {
  return `(() => {
    const now = Date.now();
    const week = ["Mon","Tue","Wed","Thu","Fri","Sat","Sun"].map((d,i)=>({dayLabel:d, words:[820,1460,640,1980,1210,0,0][i], isToday:i===4}));
    const months = ["2026-05","2026-06","2026-07","2026-08","2026-09","2026-10"].map((m,i)=>({month:m, words:[3200,7400,9100,12800,15400,2100][i], isCurrentMonth:i===5}));
    const usage = { hasData:true, zeroEditRate30d:0.82, polishedZeroEditRate30d:0.86, rawZeroEditRate30d:0.71, totalWords:48211, totalDictations:1342, totalRecordingSeconds:21400, timeSavedSeconds:50900, typingWpm:40, speakingWpm:142, moneySavedUsd:2.14, cloudRateUsdPerMinute:0.006, currentStreak:9, bestStreak:23, thisWeekWords:6110, lastWeekWords:5200, week, months, unallocatedWords:0 };
    const timings = { transcribeMs: 182, speechModelMs: 640, polishMs: 410, totalMs: 612 };
    const history = [ { durationSeconds: 8.4, timings }, { durationSeconds: 12.1, timings } ];
    const notes = [
      { id:"1", createdAt: now-120000, updatedAt: now-120000, text:"Patient reports intermittent chest tightness on exertion, no radiation.", pinned:false, durationSeconds: 14 },
      { id:"2", createdAt: now-3600000, updatedAt: now-3600000, text:"Refactor the model registry so downloads resume after sleep.", pinned:true, durationSeconds: 9 },
    ];
    const settings = { model:"parakeet-tdt-0.6b-v3", transcriptionLocation:"local", polishEnabled:true, polishProvider:"local", polishModel:"qwen3-1.7b", cloudAuthToken:"", language:"en", vocabulary:[], corrections:[], snippets:[] };
    const handlers = {
      get_usage_stats: () => usage,
      get_transcript_history: () => history,
      get_notes: () => notes,
      get_settings: () => settings,
      get_dictation_models: () => [{ model:"parakeet-tdt-0.6b-v3", cached:true }],
      get_local_model_catalog: () => ({ polish: [], download: null, metadataError: null }),
      get_update_status: () => ({ state: "idle", currentVersion: "0.1.0" }),
      get_transcription_model_status: () => ({ state: "ready" }),
    };
    let cb = 1;
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "home" }, currentWebview: { windowLabel: "home", label: "home" } },
      transformCallback: () => cb++,
      unregisterCallback: () => {},
      convertFileSrc: (p) => p,
      invoke: async (cmd) => {
        if (cmd.startsWith("plugin:event|")) return cb++;
        if (cmd.startsWith("plugin:")) return null;
        if (cmd in handlers) return handlers[cmd]();
        throw new Error("stub: " + cmd);
      },
    };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  })();`;
}
