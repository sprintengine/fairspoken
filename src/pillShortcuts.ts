import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { isRegistered, register, unregister } from "@tauri-apps/plugin-global-shortcut";
import { addEvent } from "./events";
import { errorMessage } from "./errors";
import { replaceShortcuts, type BindingRole, type Bindings } from "./shortcutRegistration";
import { isMacOS, normalizeShortcut, shortcutHint } from "./shortcuts";

// The pill window owns the global shortcuts: it registers them at startup
// (with platform spelling fallbacks), follows settings changes, and applies
// rebinds from Settings transactionally (shortcut-settings-request).

type RecordingShortcutMode = "toggle" | "push-to-talk";

export interface ShortcutSettings {
  recordingShortcut: string;
  recordingShortcutMode: RecordingShortcutMode;
  transcriptStackShortcut: string;
  interactionSounds: boolean;
}

/** What a bound chord does in the pill. */
export interface ShortcutActions {
  toggleRecording(): void;
  pushToTalk(pressed: boolean): void;
  toggleStack(): void;
}

const RECORD_SHORTCUT_MACOS_CANDIDATES = ["CommandOrControl+Shift+Digit1", "CommandOrControl+Shift+1", "Command+Shift+Digit1", "Command+Shift+1"];
const RECORD_SHORTCUT_DEFAULT_CANDIDATES = ["CommandOrControl+Shift+Digit1", "CommandOrControl+Shift+1", "Ctrl+Alt+Digit1", "Ctrl+Alt+1"];
const STACK_SHORTCUT_MACOS_CANDIDATES = ["CommandOrControl+Shift+Digit2", "CommandOrControl+Shift+2", "Command+Shift+Digit2", "Command+Shift+2"];
const STACK_SHORTCUT_DEFAULT_CANDIDATES = ["CommandOrControl+Shift+Digit2", "CommandOrControl+Shift+2", "Ctrl+Alt+Digit2", "Ctrl+Alt+2"];
const DEFAULT_SHORTCUT_SETTINGS: ShortcutSettings = {
  recordingShortcut: "CommandOrControl+Shift+Digit1",
  recordingShortcutMode: "toggle",
  transcriptStackShortcut: "CommandOrControl+Shift+Digit2",
  interactionSounds: true,
};

let shortcutSettings: ShortcutSettings = { ...DEFAULT_SHORTCUT_SETTINGS };
let shortcutRegistrationQueue: Promise<void> = Promise.resolve();
let activeBindings: Bindings = { recording: "", stack: "" };
let actions: ShortcutActions;
let pillHint: HTMLElement;

function normalizeShortcutSettings(settings: Partial<ShortcutSettings>): ShortcutSettings {
  return {
    recordingShortcut: normalizeShortcut(settings.recordingShortcut ?? DEFAULT_SHORTCUT_SETTINGS.recordingShortcut),
    recordingShortcutMode: settings.recordingShortcutMode === "push-to-talk" ? "push-to-talk" : "toggle",
    transcriptStackShortcut: normalizeShortcut(settings.transcriptStackShortcut ?? DEFAULT_SHORTCUT_SETTINGS.transcriptStackShortcut),
    interactionSounds: settings.interactionSounds !== false,
  };
}

function shortcutCandidates(configured: string, defaults: string[]): string[] {
  const normalized = normalizeShortcut(configured);
  if (!normalized) return defaults;
  if (defaults.includes(normalized)) return defaults;
  return [normalized];
}

async function registerBinding(role: BindingRole, shortcut: string): Promise<void> {
  await register(shortcut, (event) => {
    if (activeBindings[role] !== shortcut) return;
    if (role === "stack") {
      if (event.state === "Pressed") actions.toggleStack();
    } else if (shortcutSettings.recordingShortcutMode === "push-to-talk") {
      if (event.state === "Pressed") actions.pushToTalk(true);
      else actions.pushToTalk(false);
    } else if (event.state === "Pressed") actions.toggleRecording();
  });
  try {
    if (!await isRegistered(shortcut)) throw new Error(`Registration was not confirmed: ${shortcut}`);
  } catch (error) {
    try { await unregister(shortcut); }
    catch (cleanup) { throw new Error(`${String(error)}; could not release unconfirmed shortcut: ${String(cleanup)}`); }
    throw error;
  }
}
function queueShortcuts(operation: () => Promise<void>): Promise<void> {
  const next = shortcutRegistrationQueue.then(operation);
  shortcutRegistrationQueue = next.catch(() => {});
  return next;
}
export async function registerGlobalShortcuts(settings = shortcutSettings): Promise<void> {
  return queueShortcuts(async () => {
    const normalized = normalizeShortcutSettings(settings);
    const configured = { recording: normalized.recordingShortcut, stack: normalized.transcriptStackShortcut };
    // Only startup uses platform spelling fallbacks. Settings changes are
    // validated by the transactional request below, never silently substituted.
    for (const role of ["recording", "stack"] as const) {
      if (activeBindings[role]) continue;
      const defaults = role === "recording"
        ? isMacOS() ? RECORD_SHORTCUT_MACOS_CANDIDATES : RECORD_SHORTCUT_DEFAULT_CANDIDATES
        : isMacOS() ? STACK_SHORTCUT_MACOS_CANDIDATES : STACK_SHORTCUT_DEFAULT_CANDIDATES;
      const failures: string[] = [];
      for (const shortcut of shortcutCandidates(configured[role], defaults)) {
        try { await registerBinding(role, shortcut); activeBindings[role] = shortcut; break; }
        catch (error) { failures.push(String(error)); }
      }
      if (!activeBindings[role]) addEvent("warning", `${role} shortcut is unavailable: ${failures.join("; ")}`);
    }
    shortcutSettings = normalized;
    pillHint.textContent = shortcutHint(activeBindings.recording || normalized.recordingShortcut);
  });
}

type ShortcutPatch = { recordingShortcut?: string; transcriptStackShortcut?: string };

export function initPillShortcuts(hint: HTMLElement, shortcutActions: ShortcutActions): void {
  pillHint = hint;
  actions = shortcutActions;
  void listen<{ requestId: string; patch: ShortcutPatch }>("shortcut-settings-request", ({ payload }) => {
    void queueShortcuts(async () => {
      let result: { requestId: string; ok: boolean; error?: string };
      try {
        const current = await invoke<ShortcutSettings>("get_settings");
        const next = normalizeShortcutSettings({ ...current, ...payload.patch });
        const previous = { ...activeBindings };
        const actual = {
          recording: payload.patch.recordingShortcut === undefined ? previous.recording || next.recordingShortcut : next.recordingShortcut,
          stack: payload.patch.transcriptStackShortcut === undefined ? previous.stack || next.transcriptStackShortcut : next.transcriptStackShortcut,
        };
        await replaceShortcuts(previous, actual,
          { register: registerBinding, unregister },
          () => invoke("save_shortcut_settings", { patch: payload.patch }));
        activeBindings = actual;
        // settings-updated queued during persistence refreshes mode/sounds from
        // the authoritative snapshot, without registering these chords again.
        shortcutSettings = next;
        pillHint.textContent = shortcutHint(next.recordingShortcut);
        result = { requestId: payload.requestId, ok: true };
      } catch (error) {
        result = { requestId: payload.requestId, ok: false, error: String(error) };
      }
      // Notification failure cannot turn a committed registration into a failure.
      await emit("shortcut-settings-result", result).catch((error) => addEvent("warning", String(error)));
    }).catch((error) => addEvent("error", String(error)));
  }).catch((error) => addEvent("warning", String(error)));
}

export async function loadShortcutSettings(): Promise<ShortcutSettings> {
  try {
    return normalizeShortcutSettings(await invoke<ShortcutSettings>("get_settings"));
  } catch (error) {
    addEvent("warning", `Could not load shortcut settings; using defaults: ${errorMessage(error)}`);
    return { ...DEFAULT_SHORTCUT_SETTINGS };
  }
}

