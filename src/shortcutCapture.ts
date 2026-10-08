import { emitTo, listen } from "@tauri-apps/api/event";
import { required } from "./dom";
import { errorMessage } from "./errors";
import type { Settings, SettingsFormHost } from "./settingsSchema";
import { accelTokens, isMacOS, keyGlyph, keyWord, normalizeShortcut } from "./shortcuts";

// Settings → Keybindings: the two shortcut chips. A chip paints its chord as
// keycaps; clicking it captures the next chord, which the pill window
// registers transactionally (shortcut-settings-request) before it is saved.

const recordingShortcutChip = required<HTMLButtonElement>("recordingShortcutChip");
const transcriptStackShortcutChip = required<HTMLButtonElement>("transcriptStackShortcutChip");
const shortcutStatus = required<HTMLElement>("shortcutStatus");
const shortcutStatusDefault = shortcutStatus.textContent ?? "";
const onMac = isMacOS();
let host: SettingsFormHost;

// Paint the keycaps and keep the chip's accessible name describing the current
// shortcut and the rebind affordance; `data-shortcut` is the form's value source.
function renderShortcutChip(chip: HTMLButtonElement, accelerator: string): void {
  const normalized = normalizeShortcut(accelerator, accelerator);
  chip.dataset.shortcut = normalized;
  const tokens = accelTokens(normalized);
  const keys = chip.querySelector<HTMLElement>(".keys");
  if (keys) {
    keys.replaceChildren(...tokens.map((token) => {
      const span = document.createElement("kbd");
      span.className = "key ds-kbd-chord-key";
      span.textContent = keyGlyph(token);
      return span;
    }));
  }
  const name = chip.dataset.label ?? "Shortcut";
  chip.setAttribute("aria-label", `${name}: ${tokens.map(keyWord).join(" ")}. Click to change.`);
}

export function renderShortcutChips(settings: Settings): void {
  renderShortcutChip(recordingShortcutChip, settings.recordingShortcut);
  renderShortcutChip(transcriptStackShortcutChip, settings.transcriptStackShortcut);
}

// `data-shortcut` on each chip is the form's value for its binding.
export function readShortcutChips(fallback: Settings): Pick<Settings, "recordingShortcut" | "transcriptStackShortcut"> {
  return {
    recordingShortcut: recordingShortcutChip.dataset.shortcut ?? fallback.recordingShortcut,
    transcriptStackShortcut: transcriptStackShortcutChip.dataset.shortcut ?? fallback.transcriptStackShortcut,
  };
}

function shortcutFromKeyboardEvent(event: KeyboardEvent): string | null {
  if (event.key === "Escape") return null;
  if (["Shift", "Control", "Alt", "Meta"].includes(event.key)) return "";

  const key = shortcutKeyName(event);
  if (!key) return "";

  const modifiers: string[] = [];
  if (event.metaKey) modifiers.push(onMac ? "Command" : "Super");
  if (event.ctrlKey) modifiers.push("Control");
  if (event.altKey) modifiers.push("Alt");
  if (event.shiftKey) modifiers.push("Shift");

  const modifierlessAllowed = /^F(?:[1-9]|1[0-9]|2[0-4])$/.test(key);
  if (modifiers.length === 0 && !modifierlessAllowed) {
    return "";
  }

  return [...modifiers, key].join("+");
}

function shortcutKeyName(event: KeyboardEvent): string {
  if (/^Key[A-Z]$/.test(event.code)) return event.code.slice(3);
  if (/^Digit[0-9]$/.test(event.code)) return event.code;
  if (/^F(?:[1-9]|1[0-9]|2[0-4])$/.test(event.code)) return event.code;
  if (/^Numpad[0-9]$/.test(event.code)) return event.code;

  const aliases: Record<string, string> = {
    Space: "Space",
    Enter: "Enter",
    Tab: "Tab",
    Backspace: "Backspace",
    Delete: "Delete",
    Insert: "Insert",
    Home: "Home",
    End: "End",
    PageUp: "PageUp",
    PageDown: "PageDown",
    ArrowUp: "ArrowUp",
    ArrowDown: "ArrowDown",
    ArrowLeft: "ArrowLeft",
    ArrowRight: "ArrowRight",
    Minus: "Minus",
    Equal: "Equal",
    BracketLeft: "BracketLeft",
    BracketRight: "BracketRight",
    Backslash: "Backslash",
    Semicolon: "Semicolon",
    Quote: "Quote",
    Comma: "Comma",
    Period: "Period",
    Slash: "Slash",
    Backquote: "Backquote",
  };
  return aliases[event.code] ?? "";
}

let cancelCapture: (() => void) | null = null;
let shortcutSavePending = false;
function canonicalShortcut(shortcut: string): string {
  return accelTokens(shortcut).map((part) => {
    if (["CommandOrControl", "CmdOrCtrl"].includes(part)) return onMac ? "meta" : "control";
    if (["Command", "Cmd", "Meta", "Super"].includes(part)) return "meta";
    if (["Control", "Ctrl"].includes(part)) return "control";
    if (["Alt", "Option"].includes(part)) return "alt";
    return part.replace(/^Digit/, "").toLowerCase();
  }).sort().join("+");
}
async function saveShortcutSettings(patch: { recordingShortcut?: string; transcriptStackShortcut?: string }): Promise<void> {
  const requestId = crypto.randomUUID();
  await new Promise<void>((resolve, reject) => {
    let unlisten: (() => void) | undefined;
    let settled = false;
    const timer = setTimeout(() => { settled = true; unlisten?.(); reject(new Error("Shortcut registration did not respond. Reopen Settings to check the active binding.")); }, 10000);
    void listen<{ requestId: string; ok: boolean; error?: string }>("shortcut-settings-result", ({ payload }) => {
      if (settled || payload.requestId !== requestId) return;
      settled = true;
      clearTimeout(timer); unlisten?.();
      if (payload.ok) resolve(); else reject(new Error(payload.error ?? "Shortcut registration failed"));
    }).then((off) => {
      if (settled) { off(); return; }
      unlisten = off;
      return emitTo("main", "shortcut-settings-request", { requestId, patch });
    }).catch((error) => { settled = true; clearTimeout(timer); unlisten?.(); reject(error); });
  });
}
function beginShortcutCapture(chip: HTMLButtonElement, label: string): void {
  if (shortcutSavePending) return;
  cancelCapture?.();
  chip.classList.add("capturing");
  shortcutStatus.textContent = "Press a key combination, or Esc to cancel.";
  const stopCapture = (message?: string) => {
    window.removeEventListener("keydown", onKeyDown, true);
    chip.classList.remove("capturing");
    shortcutStatus.textContent = message ?? shortcutStatusDefault;
    cancelCapture = null;
  };
  cancelCapture = () => stopCapture();
  const onKeyDown = (event: KeyboardEvent) => {
    event.preventDefault(); event.stopPropagation();
    const shortcut = shortcutFromKeyboardEvent(event);
    if (shortcut === null) { stopCapture(); return; }
    if (!shortcut) { shortcutStatus.textContent = "Use a modifier key or a function key."; return; }
    if (canonicalShortcut(shortcut) === canonicalShortcut(chip.dataset.shortcut ?? "")) {
      stopCapture("This shortcut is already assigned to this action.");
      return;
    }
    const other = chip === recordingShortcutChip ? transcriptStackShortcutChip : recordingShortcutChip;
    if (canonicalShortcut(shortcut) === canonicalShortcut(other.dataset.shortcut ?? "")) {
      shortcutStatus.textContent = "That combination is already assigned to the other action. Choose another.";
      return;
    }
    const patch = chip === recordingShortcutChip ? { recordingShortcut: shortcut } : { transcriptStackShortcut: shortcut };
    stopCapture("Registering shortcut…");
    shortcutSavePending = true;
    recordingShortcutChip.disabled = transcriptStackShortcutChip.disabled = true;
    void saveShortcutSettings(patch).then(() => {
      host.setCurrent({ ...host.current(), ...patch });
      host.applyToForm(host.current());
      shortcutStatus.textContent = `${label} shortcut saved: ${accelTokens(shortcut).map(keyWord).join(" + ")}.`;
    }).catch((error) => {
      host.applyToForm(host.current());
      shortcutStatus.textContent = errorMessage(error);
      host.reportError(error);
    }).finally(() => {
      shortcutSavePending = false;
      recordingShortcutChip.disabled = transcriptStackShortcutChip.disabled = false;
    });
  };
  window.addEventListener("keydown", onKeyDown, true);
}

export function cancelShortcutCapture(): void {
  cancelCapture?.();
}

export function initShortcutCapture(formHost: SettingsFormHost): void {
  host = formHost;
  recordingShortcutChip.addEventListener("click", () => {
    beginShortcutCapture(recordingShortcutChip, "Recording");
  });
  transcriptStackShortcutChip.addEventListener("click", () => {
    beginShortcutCapture(transcriptStackShortcutChip, "Copied messages");
  });
  // A capture never outlives the Keybindings page being on screen.
  const settingsScreen = required<HTMLElement>("screen-settings");
  document.addEventListener("home-screen-changed", () => {
    if (!settingsScreen.classList.contains("active")) cancelShortcutCapture();
  });
  document.addEventListener("settings-category-changed", cancelShortcutCapture);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") cancelShortcutCapture();
  });
  new MutationObserver(() => {
    if (!settingsScreen.classList.contains("active")) cancelShortcutCapture();
  }).observe(settingsScreen, { attributes: true, attributeFilter: ["class", "hidden"] });
}
