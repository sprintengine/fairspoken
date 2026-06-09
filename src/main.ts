import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isRegistered, register, unregisterAll } from "@tauri-apps/plugin-global-shortcut";
import { addEvent, addEventWithId, eventSeverity, type EventLevel } from "./events";
import { playRecordingStartSound, playRecordingStopSound, preloadInteractionSounds, setInteractionSoundsEnabled } from "./sounds";

type AppState = "idle" | "recording" | "transcribing" | "error";

interface BackendStatus {
  state: AppState;
  message: string;
}

interface BackendLogEvent {
  id: string;
  level: EventLevel;
  message: string;
}

interface TranscriptPreviewEvent {
  index: number;
  text: string;
  finalPreview: boolean;
}

interface TranscriptHistoryItem {
  id: string;
  createdAt: number;
  text: string;
  backend: string;
  location: string;
  durationSeconds: number;
}

interface TranscriptHistoryUpdatedEvent {
  item: TranscriptHistoryItem;
}

type RecordingShortcutMode = "toggle" | "push-to-talk";

interface ShortcutSettings {
  recordingShortcut: string;
  recordingShortcutMode: RecordingShortcutMode;
  transcriptStackShortcut: string;
  interactionSounds: boolean;
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
const PILL_DRAG_THRESHOLD_PX = 4;

const app = required<HTMLElement>("app");
const titlebar = required<HTMLElement>("titlebar");
const recordBtn = required<HTMLButtonElement>("recordBtn");
const settingsBtn = required<HTMLButtonElement>("settingsBtn");
const settingsEventBadge = required<HTMLElement>("settingsEventBadge");
const copyIndicator = required<HTMLElement>("copyIndicator");
const statusLabel = required<HTMLElement>("statusLabel");
const timerEl = required<HTMLElement>("timer");
const liveTranscriptBubble = required<HTMLElement>("liveTranscriptBubble");
const liveTranscriptText = required<HTMLElement>("liveTranscriptText");
const transcriptShelf = required<HTMLElement>("transcriptShelf");

let appState: AppState = "idle";
let timerInterval: ReturnType<typeof setInterval> | null = null;
let maxRecordingTimer: ReturnType<typeof setTimeout> | null = null;
let copyIndicatorTimer: ReturnType<typeof setTimeout> | null = null;
let copiedStatusTimer: ReturnType<typeof setTimeout> | null = null;
let recordingSeconds = 0;
let startRecordingRequestPending = false;
let stopRecordingRequestPending = false;
let cancelTranscriptionRequestPending = false;
let transcriptHistory: TranscriptHistoryItem[] = [];
let copiedTranscriptId: string | null = null;
let shelfHideTimer: ReturnType<typeof setTimeout> | null = null;
let shelfVisible = false;
let shortcutSettings: ShortcutSettings = { ...DEFAULT_SHORTCUT_SETTINGS };
let pushToTalkReleasePending = false;
let shortcutRegistrationVersion = 0;

const SHELF_VISIBLE_MS = 18_000;

function required<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`Missing #${id}`);
  return node as T;
}

function setState(state: AppState, message?: string): void {
  if (copiedStatusTimer !== null && state !== "idle") {
    clearTimeout(copiedStatusTimer);
    copiedStatusTimer = null;
  }

  appState = state;
  app.dataset.appState = state;
  recordBtn.dataset.state = state;
  recordBtn.setAttribute("aria-pressed", String(state === "recording"));
  recordBtn.setAttribute(
    "aria-label",
    state === "recording" ? "Stop recording" : state === "transcribing" ? "Cancel transcription" : "Start recording",
  );
  recordBtn.title = state === "recording" ? "Stop recording" : state === "transcribing" ? "Cancel transcription" : "Click to record";
  recordBtn.disabled = false;
  statusLabel.textContent = message ?? stateLabel(state);

  if (state !== "idle") {
    hideCopyIndicator();
  }

  if (state === "recording") {
    startTimer();
  } else {
    stopTimer();
    clearMaxRecordingTimer();
  }
}

function stateLabel(state: AppState): string {
  if (state === "recording") return "Recording";
  if (state === "transcribing") return "Transcribing";
  if (state === "error") return "Error";
  return "Ready";
}

function currentAppState(): AppState {
  return appState;
}

function canStartRecording(): boolean {
  return (appState === "idle" || appState === "error")
    && !startRecordingRequestPending
    && !stopRecordingRequestPending;
}

function isRecoverableRecordingStopError(message: string): boolean {
  return message === "Recording was too short" || message.startsWith("No speech detected");
}

function startTimer(): void {
  if (timerInterval !== null) return;
  recordingSeconds = 0;
  timerEl.textContent = "0:00";
  timerInterval = setInterval(() => {
    recordingSeconds += 1;
    timerEl.textContent = formatTime(recordingSeconds);
  }, 1000);
}

function stopTimer(): void {
  if (timerInterval !== null) {
    clearInterval(timerInterval);
    timerInterval = null;
  }
  timerEl.textContent = "";
}

function scheduleMaxRecordingStop(maxSeconds: number): void {
  clearMaxRecordingTimer();
  maxRecordingTimer = setTimeout(() => {
    if (appState === "recording") {
      addEvent("info", `Max recording time reached (${formatTime(maxSeconds)}); transcribing`);
      void stopAndTranscribe();
    }
  }, maxSeconds * 1000);
}

function clearMaxRecordingTimer(): void {
  if (maxRecordingTimer !== null) {
    clearTimeout(maxRecordingTimer);
    maxRecordingTimer = null;
  }
}

function formatTime(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return `${minutes}:${remainder.toString().padStart(2, "0")}`;
}

function waitForPaint(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => resolve());
    });
  });
}

function hideCopyIndicator(): void {
  if (copyIndicatorTimer !== null) {
    clearTimeout(copyIndicatorTimer);
    copyIndicatorTimer = null;
  }

  copyIndicator.dataset.visible = "false";
  copyIndicator.setAttribute("aria-hidden", "true");
}

function showCopyIndicator(): void {
  hideCopyIndicator();
  copyIndicator.dataset.visible = "true";
  copyIndicator.setAttribute("aria-hidden", "false");
  copyIndicatorTimer = setTimeout(hideCopyIndicator, 20_000);
}

function showCopiedStatus(): void {
  setState("idle", "Copied");
  if (copiedStatusTimer !== null) {
    clearTimeout(copiedStatusTimer);
  }
  copiedStatusTimer = setTimeout(() => {
    copiedStatusTimer = null;
    if (appState === "idle") {
      setState("idle", "Ready");
    }
  }, 1400);
}

function updateSettingsEventBadge(): void {
  const severity = eventSeverity();
  settingsEventBadge.dataset.severity = severity ?? "none";
  settingsEventBadge.hidden = severity === null;
  settingsBtn.title = severity === "error" ? "Settings: errors recorded" : severity === "warning" ? "Settings: warnings recorded" : "Settings";
}

async function loadTranscriptHistory(): Promise<void> {
  transcriptHistory = uniqueTranscriptItems(await invoke<TranscriptHistoryItem[]>("get_transcript_history"));
  copiedTranscriptId = copiedTranscriptId ?? transcriptHistory[0]?.id ?? null;
  renderTranscriptShelf();
}

function renderTranscriptShelf(): void {
  transcriptShelf.replaceChildren();

  if (!shelfVisible) {
    return;
  }

  for (const item of transcriptHistory.slice(0, 8)) {
    const clip = document.createElement("div");
    clip.className = "transcript-clip";
    clip.dataset.id = item.id;
    clip.dataset.copied = String(item.id === copiedTranscriptId);

    const button = document.createElement("button");
    button.type = "button";
    button.className = "transcript-clip-copy";
    button.title = item.text;
    button.setAttribute("aria-label", item.id === copiedTranscriptId ? "Copied transcript clip" : "Copy transcript clip");
    const text = document.createElement("span");
    text.className = "transcript-clip-text";
    text.textContent = item.text;
    button.append(text);
    button.addEventListener("click", () => {
      void copyTranscriptItem(item.id);
    });

    const closeButton = document.createElement("button");
    closeButton.type = "button";
    closeButton.className = "transcript-clip-close";
    closeButton.setAttribute("aria-label", "Delete transcript clip");
    closeButton.title = "Delete";
    closeButton.textContent = "x";
    closeButton.addEventListener("click", () => {
      void deleteTranscriptItem(item.id);
    });

    clip.append(button, closeButton);
    transcriptShelf.append(clip);
  }
}

async function copyTranscriptItem(id: string): Promise<void> {
  try {
    const item = await invoke<TranscriptHistoryItem>("copy_transcript_history_item", { id });
    copiedTranscriptId = item.id;
    showTranscriptShelf();
    renderTranscriptShelf();
    showCopiedStatus();
    showCopyIndicator();
  } catch (error) {
    addEvent("error", error instanceof Error ? error.message : String(error));
    setState("error", "Error");
  }
}

async function deleteTranscriptItem(id: string): Promise<void> {
  const clip = transcriptShelf.querySelector<HTMLElement>(`.transcript-clip[data-id="${CSS.escape(id)}"]`);
  clip?.setAttribute("data-removing", "true");

  window.setTimeout(async () => {
    try {
      await invoke("delete_transcript_history_item", { id });
      transcriptHistory = transcriptHistory.filter((item) => item.id !== id);
      if (copiedTranscriptId === id) {
        copiedTranscriptId = transcriptHistory[0]?.id ?? null;
      }
      renderTranscriptShelf();
    } catch (error) {
      addEvent("error", error instanceof Error ? error.message : String(error));
      clip?.removeAttribute("data-removing");
    }
  }, 170);
}

function addOrReplaceTranscriptItem(item: TranscriptHistoryItem): void {
  transcriptHistory = uniqueTranscriptItems([item, ...transcriptHistory]).slice(0, 50);
  copiedTranscriptId = item.id;
  showTranscriptShelf();
  renderTranscriptShelf();
}

function uniqueTranscriptItems(items: TranscriptHistoryItem[]): TranscriptHistoryItem[] {
  const seen = new Set<string>();
  const unique: TranscriptHistoryItem[] = [];
  for (const item of items) {
    const key = item.text.replace(/\s+/g, " ").trim().toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    unique.push(item);
  }
  return unique;
}

function resetLiveTranscript(): void {
  liveTranscriptText.textContent = "";
  liveTranscriptBubble.hidden = true;
}

function showLiveTranscript(text: string): void {
  const preview = recentTranscriptText(text);
  if (!preview) return;
  if (shelfHideTimer !== null) {
    clearTimeout(shelfHideTimer);
    shelfHideTimer = null;
  }
  liveTranscriptText.textContent = preview;
  liveTranscriptBubble.hidden = false;
  liveTranscriptText.scrollTop = liveTranscriptText.scrollHeight;
}

function showTranscriptShelf(): void {
  shelfVisible = true;
  if (shelfHideTimer !== null) {
    clearTimeout(shelfHideTimer);
  }
  shelfHideTimer = setTimeout(hideTranscriptShelf, SHELF_VISIBLE_MS);
}

function hideTranscriptShelf(): void {
  shelfHideTimer = null;
  shelfVisible = false;
  renderTranscriptShelf();
}

function recentTranscriptText(text: string): string {
  const normalized = text.replace(/\s+/g, " ").trim();
  if (normalized.length <= 320) return normalized;

  const tail = normalized.slice(-320);
  const sentenceStart = tail.search(/[.!?]\s+[A-Z0-9]/);
  return sentenceStart >= 0 ? tail.slice(sentenceStart + 2).trim() : tail.trim();
}

async function loadBackendStatus(): Promise<void> {
  const status = await invoke<BackendStatus>("get_app_status");
  setState(status.state, status.message);
}

async function toggleRecording(): Promise<void> {
  if (appState === "transcribing") {
    await cancelTranscription();
    return;
  }
  if (stopRecordingRequestPending) return;

  if (appState === "recording") {
    await stopAndTranscribe();
    return;
  }

  await startRecording();
}

async function startRecording(): Promise<boolean> {
  if (!canStartRecording()) return false;

  try {
    startRecordingRequestPending = true;
    const maxRecordingSeconds = await invoke<number>("start_recording");
    startRecordingRequestPending = false;
    cancelTranscriptionRequestPending = false;
    playRecordingStartSound();
    setState("recording");
    resetLiveTranscript();
    scheduleMaxRecordingStop(maxRecordingSeconds);
    return true;
  } catch (error) {
    startRecordingRequestPending = false;
    stopRecordingRequestPending = false;
    const message = error instanceof Error ? error.message : String(error);
    addEvent("error", message);
    await refreshStateAfterStartError();
    return false;
  }
}

async function startPushToTalkRecording(): Promise<void> {
  if (!canStartRecording()) return;
  pushToTalkReleasePending = false;
  const started = await startRecording();
  if (started && pushToTalkReleasePending && currentAppState() === "recording") {
    pushToTalkReleasePending = false;
    await stopAndTranscribe();
  }
}

async function stopPushToTalkRecording(): Promise<void> {
  if (startRecordingRequestPending) {
    pushToTalkReleasePending = true;
    return;
  }
  pushToTalkReleasePending = false;
  if (appState === "recording" && !stopRecordingRequestPending) {
    await stopAndTranscribe();
  }
}

async function refreshStateAfterStartError(): Promise<void> {
  try {
    const status = await invoke<BackendStatus>("get_app_status");
    if (status.state === "recording") {
      setState("recording", status.message);
      addEvent("warning", "Recovered recording state after a UI error");
    } else {
      setState("error", "Error");
    }
  } catch {
    setState("error", "Error");
  }
}

async function stopAndTranscribe(): Promise<void> {
  if (stopRecordingRequestPending) return;
  stopRecordingRequestPending = true;
  cancelTranscriptionRequestPending = false;
  clearMaxRecordingTimer();
  playRecordingStopSound();
  setState("transcribing");
  await waitForPaint();

  try {
    const transcript = await invoke<string>("stop_and_transcribe");
    if (cancelTranscriptionRequestPending) {
      setState("idle", "Ready");
      return;
    }
    if (transcript) {
      addEvent("info", "Transcript copied to clipboard");
      void loadTranscriptHistory().catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));
      resetLiveTranscript();
      showCopiedStatus();
      showCopyIndicator();
    } else {
      addEvent("warning", "No transcript returned");
      setState("idle", "Ready");
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (message === "Transcription was cancelled") {
      addEvent("info", "Transcription cancelled");
      resetLiveTranscript();
      setState("idle", "Ready");
    } else if (isRecoverableRecordingStopError(message)) {
      addEvent("warning", message);
      resetLiveTranscript();
      setState("idle", "Ready");
    } else {
      addEvent("error", message);
      setState("error", "Error");
    }
  } finally {
    stopRecordingRequestPending = false;
    cancelTranscriptionRequestPending = false;
  }
}

async function cancelTranscription(): Promise<void> {
  if (cancelTranscriptionRequestPending || !stopRecordingRequestPending) return;
  cancelTranscriptionRequestPending = true;
  setState("transcribing", "Canceling");

  try {
    await invoke("cancel_transcription");
  } catch (error) {
    addEvent("warning", error instanceof Error ? error.message : String(error));
  }
}

async function toggleTranscriptStack(): Promise<void> {
  try {
    await emit("transcript-shelf-toggle");
  } catch (error) {
    addEvent("warning", error instanceof Error ? error.message : String(error));
  }
}

// The pill is a frameless, always-on-top HUD. A press that moves drags the
// window; a press that stays put is a click that toggles the copied-message
// stack. We drive both from JS (rather than a CSS drag region) so the click
// is delivered reliably on macOS instead of being swallowed by the OS drag.
function wirePillPointer(): void {
  const appWindow = getCurrentWindow();
  let press: { x: number; y: number; dragging: boolean } | null = null;

  titlebar.addEventListener("mousedown", (event) => {
    if (event.button !== 0) return;
    if ((event.target as HTMLElement).closest(".logo-btn, .settings-btn")) return;
    press = { x: event.clientX, y: event.clientY, dragging: false };
  });

  window.addEventListener("mousemove", (event) => {
    if (press === null || press.dragging) return;
    if (Math.abs(event.clientX - press.x) > PILL_DRAG_THRESHOLD_PX || Math.abs(event.clientY - press.y) > PILL_DRAG_THRESHOLD_PX) {
      press.dragging = true;
      void appWindow.startDragging();
    }
  });

  window.addEventListener("mouseup", () => {
    if (press === null) return;
    const wasClick = !press.dragging;
    press = null;
    if (wasClick) void toggleTranscriptStack();
  });
}

function isMacOS(): boolean {
  return navigator.platform.toLowerCase().includes("mac");
}

function normalizeShortcutSettings(settings: Partial<ShortcutSettings>): ShortcutSettings {
  return {
    recordingShortcut: normalizeShortcut(settings.recordingShortcut ?? DEFAULT_SHORTCUT_SETTINGS.recordingShortcut),
    recordingShortcutMode: settings.recordingShortcutMode === "push-to-talk" ? "push-to-talk" : "toggle",
    transcriptStackShortcut: normalizeShortcut(settings.transcriptStackShortcut ?? DEFAULT_SHORTCUT_SETTINGS.transcriptStackShortcut),
    interactionSounds: settings.interactionSounds !== false,
  };
}

function normalizeShortcut(shortcut: string): string {
  return shortcut
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean)
    .join("+");
}

function shortcutCandidates(configured: string, defaults: string[]): string[] {
  const normalized = normalizeShortcut(configured);
  if (!normalized) return defaults;
  if (defaults.includes(normalized)) return defaults;
  return [normalized];
}

async function registerShortcut(
  label: string,
  shortcuts: string[],
  handler: Parameters<typeof register>[1],
): Promise<void> {
  const failures: string[] = [];

  for (const shortcut of shortcuts) {
    try {
      await register(shortcut, handler);

      const registered = await isRegistered(shortcut);
      if (registered) {
        addEvent("info", `${label} shortcut registered: ${shortcut}`);
        return;
      }

      failures.push(`${shortcut}: registration was not confirmed`);
    } catch (error) {
      failures.push(`${shortcut}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  addEvent("warning", `${label} shortcut is unavailable: ${failures.join("; ")}`);
}

async function registerRecordingShortcut(settings: ShortcutSettings): Promise<void> {
  const defaults = isMacOS() ? RECORD_SHORTCUT_MACOS_CANDIDATES : RECORD_SHORTCUT_DEFAULT_CANDIDATES;
  const shortcuts = shortcutCandidates(settings.recordingShortcut, defaults);
  await registerShortcut("Recording", shortcuts, (event) => {
    if (settings.recordingShortcutMode === "push-to-talk") {
      if (event.state === "Pressed") {
        void startPushToTalkRecording();
      } else {
        void stopPushToTalkRecording();
      }
      return;
    }

    if (event.state === "Pressed") {
      void toggleRecording();
    }
  });
}

async function registerStackShortcut(settings: ShortcutSettings): Promise<void> {
  const defaults = isMacOS() ? STACK_SHORTCUT_MACOS_CANDIDATES : STACK_SHORTCUT_DEFAULT_CANDIDATES;
  const shortcuts = shortcutCandidates(settings.transcriptStackShortcut, defaults);
  await registerShortcut("Copied-messages", shortcuts, (event) => {
    if (event.state === "Pressed") {
      void toggleTranscriptStack();
    }
  });
}

async function registerGlobalShortcuts(settings = shortcutSettings): Promise<void> {
  const version = ++shortcutRegistrationVersion;
  const normalized = normalizeShortcutSettings(settings);
  shortcutSettings = normalized;
  setInteractionSoundsEnabled(normalized.interactionSounds);

  try {
    await unregisterAll();
  } catch (error) {
    addEvent("warning", `Could not clear existing shortcuts before registration: ${error instanceof Error ? error.message : String(error)}`);
  }

  if (version !== shortcutRegistrationVersion) return;
  await registerRecordingShortcut(normalized);
  if (version !== shortcutRegistrationVersion) return;
  await registerStackShortcut(normalized);
}

async function loadShortcutSettings(): Promise<ShortcutSettings> {
  try {
    return normalizeShortcutSettings(await invoke<ShortcutSettings>("get_settings"));
  } catch (error) {
    addEvent("warning", `Could not load shortcut settings; using defaults: ${error instanceof Error ? error.message : String(error)}`);
    return { ...DEFAULT_SHORTCUT_SETTINGS };
  }
}

recordBtn.addEventListener("click", () => {
  void toggleRecording();
});

settingsBtn.addEventListener("click", () => {
  void invoke("open_home_window", { screen: "settings" });
});

window.addEventListener("multivoice-events-updated", updateSettingsEventBadge);
window.addEventListener("storage", (event) => {
  if (event.key === "multivoice-tauri-events") {
    updateSettingsEventBadge();
  }
});

void listen<BackendLogEvent>("backend-event", (event) => {
  addEventWithId(event.payload.id, event.payload.level, event.payload.message);
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

void listen<TranscriptPreviewEvent>("transcript-preview", (event) => {
  showLiveTranscript(event.payload.text);
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

void listen<TranscriptHistoryUpdatedEvent>("transcript-history-updated", (event) => {
  addOrReplaceTranscriptItem(event.payload.item);
  resetLiveTranscript();
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

void listen<TranscriptHistoryItem>("transcript-copied", (event) => {
  copiedTranscriptId = event.payload.id;
  renderTranscriptShelf();
  showCopyIndicator();
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

void listen<ShortcutSettings>("settings-updated", (event) => {
  void registerGlobalShortcuts(event.payload);
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

void loadBackendStatus().catch((error) => {
  addEvent("error", error instanceof Error ? error.message : String(error));
  setState("error", "Error");
});
void loadTranscriptHistory().catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));
void loadShortcutSettings()
  .then((settings) => registerGlobalShortcuts(settings))
  .catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));
wirePillPointer();
updateSettingsEventBadge();
preloadInteractionSounds();
