// node scripts/test-update-model.mjs — the update UI's state machine
// (src/updateModel.ts): every sidebar button state, the toast rule and the
// Settings → Updates copy.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";
const source = await readFile(new URL("../src/updateModel.ts", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ES2020 } }).outputText;
const model = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`);
const { buttonView, settingsView, toastFor, updatePending, relativeTime, lastCheckedText } = model;

const base = { currentVersion: "0.3.0", channel: "stable", buildChannel: "stable", lastCheckedAt: null };
const status = (state, extra = {}) => ({ ...base, state, ...extra });

// Sidebar button: the six states of the contract (plus hidden in dev builds).
{
  const idle = buttonView(status("idle"));
  assert.deepEqual([idle.kind, idle.icon, idle.badge, idle.action, idle.disabled], ["idle", "sync", null, "check", false]);
  assert.match(idle.label, /Check for updates — Fairspoken 0\.3\.0 \(Stable\)/);

  const upToDate = buttonView(status("upToDate"));
  assert.deepEqual([upToDate.kind, upToDate.icon, upToDate.action], ["idle", "sync", "check"]);
  assert.match(upToDate.label, /^Up to date/);

  const checking = buttonView(status("checking"));
  assert.deepEqual([checking.kind, checking.icon, checking.badge, checking.action, checking.disabled], ["checking", "sync", null, null, true]);

  const available = buttonView(status("available", { version: "0.3.1", switchToStable: false, channel: "nightly" }));
  assert.deepEqual([available.kind, available.icon, available.badge, available.action], ["available", "download", "count", "install"]);
  assert.equal(available.label, "Update available: Fairspoken 0.3.1 (Nightly) — click to install");

  const downloading = buttonView(status("downloading", { version: "0.3.1", switchToStable: false, downloaded: 42, total: 100 }));
  assert.deepEqual([downloading.kind, downloading.icon, downloading.badge, downloading.action, downloading.disabled], ["downloading", "download", null, null, true]);
  assert.equal(downloading.progress, 0.42);
  assert.match(downloading.label, /Downloading Fairspoken 0\.3\.1 \(Stable\) — 42%/);
  const unsized = buttonView(status("downloading", { version: "0.3.1", switchToStable: false, downloaded: 42, total: null }));
  assert.equal(unsized.progress, null);
  assert.match(unsized.label, /Downloading .*…$/);
  const installing = buttonView(status("downloading", { version: "0.3.1", switchToStable: false, downloaded: 100, total: 100 }));
  assert.match(installing.label, /^Installing/);

  const ready = buttonView(status("ready", { version: "0.3.1", switchToStable: false }));
  assert.deepEqual([ready.kind, ready.icon, ready.badge, ready.action], ["ready", "restart", "dot", "restart"]);
  assert.match(ready.label, /click to restart$/);

  const failed = buttonView(status("failed", { message: "Could not check for updates: offline" }));
  assert.deepEqual([failed.kind, failed.icon, failed.badge, failed.action], ["error", "sync", "warning", "check"]);
  assert.match(failed.label, /offline — click to retry$/);

  assert.equal(buttonView(status("disabled")).kind, "hidden");
}

// The sidebar row's visible title and detail lines.
{
  const lines = (view) => [view.title, view.detail];
  assert.deepEqual(lines(buttonView(status("idle"))), ["Check for updates", "0.3.0 · Stable"]);
  assert.deepEqual(lines(buttonView(status("checking"))), ["Checking…", "0.3.0 · Stable"]);
  assert.deepEqual(lines(buttonView(status("available", { version: "0.3.1", switchToStable: false }))), ["Update available", "Fairspoken 0.3.1"]);
  assert.deepEqual(lines(buttonView(status("downloading", { version: "0.3.1", switchToStable: false, downloaded: 42, total: 100 }))), ["Downloading… 42%", "Fairspoken 0.3.1"]);
  assert.equal(buttonView(status("downloading", { version: "0.3.1", switchToStable: false, downloaded: 100, total: 100 })).title, "Installing…");
  assert.deepEqual(lines(buttonView(status("ready", { version: "0.3.1", switchToStable: true }))), ["Restart to update", "Stable 0.3.1"]);
  assert.deepEqual(lines(buttonView(status("failed", { message: "offline" }))), ["Update check failed", "offline"]);
}

// Nightly → stable is labelled as a switch, everywhere it is named.
{
  const nightly = { currentVersion: "0.4.0-nightly.20261005.4", buildChannel: "nightly", channel: "stable" };
  const offer = status("available", { ...nightly, version: "0.3.0", switchToStable: true });
  assert.equal(buttonView(offer).label, "Switch to stable 0.3.0 — click to install");
  assert.equal(toastFor(offer, new Set(), false).title, "Switch to stable 0.3.0");
  assert.equal(settingsView(offer, false).button.label, "Switch to stable 0.3.0");
  assert.match(settingsView(status("idle", nightly), false).stableNote, /nightly build/);
  assert.equal(settingsView(status("idle", { ...nightly, channel: "nightly" }), false).stableNote, null);
  assert.equal(settingsView(status("idle"), false).stableNote, null);
}

// Toast: once per channel+version, a manual check repeats it, and only for offers.
{
  const offer = status("available", { version: "0.3.1", switchToStable: false });
  const first = toastFor(offer, new Set(), false);
  assert.equal(first.title, "Fairspoken 0.3.1 is available");
  assert.equal(toastFor(offer, new Set([first.key]), false), null);
  assert.notEqual(toastFor(offer, new Set([first.key]), true), null);
  assert.notEqual(toastFor({ ...offer, version: "0.3.2" }, new Set([first.key]), false), null);
  for (const state of ["idle", "checking", "upToDate", "ready", "failed"]) {
    assert.equal(toastFor(status(state, { version: "0.3.1", message: "x" }), new Set(), true), null);
  }
}

// Settings badges while an update waits for the user.
assert.equal(updatePending(status("available", { version: "1", switchToStable: false })), true);
assert.equal(updatePending(status("ready", { version: "1", switchToStable: false })), true);
for (const state of ["idle", "checking", "upToDate", "failed", "disabled"]) assert.equal(updatePending(status(state)), false);
assert.equal(updatePending(null), false);

// Settings → Updates copy and its one button.
{
  assert.equal(settingsView(status("upToDate"), true).statusText, "You're up to date.");
  assert.equal(settingsView(status("upToDate"), false).statusText, "You're on the latest version.");
  assert.equal(settingsView(status("checking"), true).button.busy, true);
  assert.equal(settingsView(status("disabled"), false).button, null);
  const ready = settingsView(status("ready", { version: "0.3.1", switchToStable: false }), false);
  assert.deepEqual([ready.button.label, ready.button.action, ready.button.primary], ["Restart to update", "restart", true]);
  const failed = settingsView(status("failed", { message: "offline" }), false);
  assert.deepEqual([failed.statusText, failed.error, failed.button.action], ["offline", true, "check"]);
  assert.equal(settingsView(status("downloading", { version: "0.3.1", switchToStable: false, downloaded: 5, total: 10 }), false).statusText, "Downloading 0.3.1… 50%");
}

// Last checked.
{
  const now = Date.UTC(2026, 9, 5, 12);
  assert.equal(relativeTime(now - 10_000, now), "just now");
  assert.equal(relativeTime(now - 60_000, now), "1 minute ago");
  assert.equal(relativeTime(now - 5 * 60_000, now), "5 minutes ago");
  assert.equal(relativeTime(now - 3 * 3_600_000, now), "3 hours ago");
  assert.equal(relativeTime(now - 3 * 86_400_000, now), "3 days ago");
  assert.equal(relativeTime(now + 5_000, now), "just now");
  assert.equal(lastCheckedText(status("idle"), now), "Not checked yet");
  assert.equal(lastCheckedText(status("idle", { lastCheckedAt: now - 120_000 }), now), "Last checked 2 minutes ago");
}

console.log("Update model: six button states, nightly→stable labels, toast once per version, Settings badges and copy passed.");
