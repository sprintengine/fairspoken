import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./updates.css";
import {
  buttonView,
  CHANNEL_HELP,
  downloadFraction,
  lastCheckedText,
  settingsView,
  toastFor,
  updatePending,
  type ButtonAction,
  type Channel,
  type UpdateStatus,
} from "./updateModel";

// The update UI in the home window: the sidebar button, the "available" toast,
// the Settings rail and Updates badges, and Settings → Updates. The backend
// (src-tauri/src/updates.rs) checks on its own schedule and reports every
// change as `update-status`; everything here renders that one status.

const button = document.querySelector<HTMLButtonElement>("#updateButton")!;
const settingsBadge = document.querySelector<HTMLElement>("#settingsUpdateBadge")!;
const settingsRail = document.querySelector<HTMLElement>("#settingsRailButton")!;
const navBadge = document.querySelector<HTMLElement>("#updatesNavBadge")!;
const navItem = document.querySelector<HTMLElement>("#updatesNavItem")!;
const versionLabel = document.querySelector<HTMLElement>("#appVersion")!;
const channelLabel = document.querySelector<HTMLElement>("#appChannel")!;
const statusLabel = document.querySelector<HTMLElement>("#updateStatus")!;
const progress = document.querySelector<HTMLElement>("#updateProgress")!;
const lastChecked = document.querySelector<HTMLElement>("#updateLastChecked")!;
const action = document.querySelector<HTMLButtonElement>("#updateAction")!;
const channelSeg = document.querySelector<HTMLElement>("#updateChannelSeg")!;
const stableNote = document.querySelector<HTMLElement>("#updateStableNote")!;
document.querySelector<HTMLElement>("#stableHelp")!.textContent = CHANNEL_HELP.stable;
document.querySelector<HTMLElement>("#nightlyHelp")!.textContent = CHANNEL_HELP.nightly;

const SEEN_KEY = "fairspoken.updateToastsSeen";
let current: UpdateStatus | null = null;
/** The user pressed Check: report "up to date" (or the failure) once it lands. */
let manualCheck = false;
let channelPending = false;

function seenToasts(): Set<string> {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(SEEN_KEY) ?? "[]");
    return new Set(Array.isArray(raw) ? raw.filter((key): key is string => typeof key === "string") : []);
  } catch {
    return new Set();
  }
}

function markToastSeen(key: string): void {
  const seen = [...seenToasts(), key].slice(-20);
  try { localStorage.setItem(SEEN_KEY, JSON.stringify([...new Set(seen)])); } catch { /* best effort */ }
}

// ── Toast ──────────────────────────────────────────────────────────────
const region = document.createElement("div");
region.className = "ds-toast-region";
document.body.append(region);
let toast: HTMLElement | null = null;
let toastTimer: ReturnType<typeof setTimeout> | undefined;

function dismissToast(): void {
  clearTimeout(toastTimer);
  toast?.remove();
  toast = null;
}

const GLYPH = '<svg class="ds-toast-glyph" aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M12 4v10.5M7.5 10 12 14.5 16.5 10"/><path d="M4.5 14.5v3.5a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2v-3.5"/></svg>';

function showToast(options: {
  title: string;
  description?: string;
  actions?: { label: string; primary?: boolean; run: () => void }[];
  autoDismissMs?: number;
}): void {
  dismissToast();
  const element = document.createElement("div");
  element.className = "ds-toast";
  element.setAttribute("role", "status");
  element.setAttribute("aria-live", "polite");
  element.innerHTML = GLYPH;
  const content = document.createElement("div");
  content.className = "ds-toast-content";
  const title = document.createElement("div");
  title.className = "ds-toast-title";
  title.textContent = options.title;
  content.append(title);
  if (options.description) {
    const description = document.createElement("div");
    description.className = "ds-toast-description";
    description.textContent = options.description;
    content.append(description);
  }
  if (options.actions?.length) {
    const row = document.createElement("div");
    row.className = "ds-toast-actions";
    for (const item of options.actions) {
      const control = document.createElement("button");
      control.type = "button";
      control.className = `btn btn-sm ${item.primary ? "btn-primary" : "btn-ghost"}`;
      control.textContent = item.label;
      control.addEventListener("click", () => {
        dismissToast();
        item.run();
      });
      row.append(control);
    }
    content.append(row);
  }
  element.append(content);
  region.append(element);
  toast = element;
  if (options.autoDismissMs) toastTimer = setTimeout(dismissToast, options.autoDismissMs);
}

// ── Actions ────────────────────────────────────────────────────────────
function reportError(error: unknown): void {
  showToast({ title: "Update failed", description: String(error instanceof Error ? error.message : error), autoDismissMs: 6000 });
}

async function run(kind: ButtonAction): Promise<void> {
  switch (kind) {
    case "check":
      manualCheck = true;
      render(await invoke<UpdateStatus>("check_for_updates"));
      return;
    case "install":
      dismissToast();
      render(await invoke<UpdateStatus>("download_and_install_update"));
      return;
    case "restart":
      await invoke("restart_to_update");
      return;
    case null:
      return;
  }
}

function perform(kind: ButtonAction): void {
  run(kind).catch((error: unknown) => {
    manualCheck = false;
    reportError(error);
    if (current) render(current);
  });
}

button.addEventListener("click", () => {
  if (current) perform(buttonView(current).action);
});
action.addEventListener("click", () => {
  if (!current) return;
  const view = settingsView(current, manualCheck);
  if (view.button && !view.button.busy) perform(view.button.action);
});

for (const option of channelSeg.querySelectorAll<HTMLButtonElement>("button[data-value]")) {
  option.addEventListener("click", () => {
    const next = option.dataset.value as Channel;
    if (!current || channelPending || next === current.channel) return;
    channelPending = true;
    renderChannel(next);
    invoke<UpdateStatus>("set_update_channel", { channel: next })
      .then(render)
      .catch((error: unknown) => {
        reportError(error);
        if (current) render(current);
      })
      .finally(() => {
        channelPending = false;
        if (current) renderChannel(current.channel);
      });
  });
}

// ── Rendering ──────────────────────────────────────────────────────────
function renderChannel(channel: Channel): void {
  for (const option of channelSeg.querySelectorAll<HTMLButtonElement>("button[data-value]")) {
    option.setAttribute("aria-pressed", String(option.dataset.value === channel));
    option.disabled = channelPending || current?.state === "disabled";
  }
}

function renderButton(status: UpdateStatus): void {
  const view = buttonView(status);
  button.hidden = view.kind === "hidden";
  button.dataset.state = view.kind;
  button.dataset.icon = view.icon;
  if (view.badge) button.dataset.badge = view.badge;
  else delete button.dataset.badge;
  if (view.kind === "downloading") {
    button.dataset.progress = view.progress === null ? "indeterminate" : "determinate";
    button.style.setProperty("--update-ring-offset", String(100 - Math.round((view.progress ?? 0) * 100)));
  } else {
    delete button.dataset.progress;
  }
  button.disabled = view.disabled;
  button.setAttribute("aria-label", view.label);
  button.dataset.tooltip = view.label;
  // A tooltip already showing for this button follows the state.
  if (button.getAttribute("aria-describedby")) {
    const tip = document.getElementById(button.getAttribute("aria-describedby")!);
    if (tip) tip.textContent = view.label;
  }
}

function renderBadges(status: UpdateStatus): void {
  const pending = updatePending(status);
  settingsBadge.hidden = !pending;
  navBadge.hidden = !pending;
  const what = status.state === "ready" ? "update ready to install" : "update available";
  settingsRail.setAttribute("aria-label", pending ? `Settings — 1 ${what}` : "Settings");
  settingsRail.dataset.tooltip = pending ? `Settings — 1 ${what}` : "Settings";
  navItem.setAttribute("aria-label", pending ? `Updates — 1 ${what}` : "Updates");
}

function renderSettings(status: UpdateStatus): void {
  const view = settingsView(status, manualCheck);
  versionLabel.textContent = status.currentVersion;
  channelLabel.textContent = status.buildChannel === "nightly" ? "Nightly build" : "Stable build";
  statusLabel.textContent = view.statusText;
  statusLabel.dataset.error = String(view.error);
  const fraction = downloadFraction(status);
  progress.hidden = status.state !== "downloading";
  progress.style.setProperty("--update-progress", `${Math.round((fraction ?? 0) * 100)}%`);
  lastChecked.textContent = status.state === "disabled" ? "" : lastCheckedText(status, Date.now());
  action.hidden = view.button === null;
  if (view.button) {
    action.textContent = view.button.label;
    action.classList.toggle("btn-primary", view.button.primary);
    action.disabled = view.button.busy;
    if (view.button.busy) action.setAttribute("aria-busy", "true");
    else action.removeAttribute("aria-busy");
  }
  stableNote.hidden = view.stableNote === null;
  stableNote.textContent = view.stableNote ?? "";
  if (!channelPending) renderChannel(status.channel);
}

function render(status: UpdateStatus): void {
  const previous = current;
  current = status;
  renderButton(status);
  renderBadges(status);
  renderSettings(status);

  const settled = status.state !== "checking" && status.state !== "idle";
  const offer = toastFor(status, seenToasts(), manualCheck);
  if (offer && previous?.state !== "available") {
    markToastSeen(offer.key);
    showToast({
      title: offer.title,
      description: offer.description,
      actions: [
        { label: "Later", run: () => {} },
        { label: "Update", primary: true, run: () => perform("install") },
      ],
    });
  } else if (status.state === "ready" && previous?.state === "downloading") {
    showToast({
      title: status.switchToStable ? `Stable ${status.version} is installed` : `Fairspoken ${status.version} is installed`,
      description: "Restart Fairspoken to finish updating.",
      actions: [
        { label: "Later", run: () => {} },
        { label: "Restart", primary: true, run: () => perform("restart") },
      ],
    });
  } else if (status.state === "failed" && previous?.state === "downloading") {
    showToast({ title: "The update did not install", description: status.message, autoDismissMs: 8000 });
  } else if (manualCheck && status.state === "upToDate") {
    showToast({
      title: "You're up to date",
      description: `Fairspoken ${status.currentVersion} is the latest ${status.channel} release.`,
      autoDismissMs: 4000,
    });
  } else if (manualCheck && status.state === "failed") {
    showToast({ title: "Couldn't check for updates", description: status.message, autoDismissMs: 6000 });
  }
  if (manualCheck && settled) manualCheck = false;
}

// Relative "last checked" text ages while the window is open.
setInterval(() => {
  if (current) lastChecked.textContent = current.state === "disabled" ? "" : lastCheckedText(current, Date.now());
}, 60_000);

void listen<UpdateStatus>("update-status", (event) => render(event.payload));
void invoke<UpdateStatus>("get_update_status").then(render);
