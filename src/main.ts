import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { isRegistered, register, unregisterAll } from "@tauri-apps/plugin-global-shortcut";
import { addEvent, addEventWithId, eventSeverity, type EventLevel } from "./events";

// "starting" covers the window between the start request and the backend
// confirming native capture — visible when the model or device is slow.
type AppState = "idle" | "starting" | "recording" | "transcribing" | "error";

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

type PillLayout = "idle" | "hover" | "starting" | "recording" | "transcribing" | "copied" | "error";

interface AudioLevelEvent {
  peak: number;
  rms: number;
}

interface RecordingAutoStoppedEvent {
  committed: boolean;
  message: string;
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

const recordBtn = required<HTMLButtonElement>("recordBtn");
const settingsBtn = required<HTMLButtonElement>("settingsBtn");
const settingsEventBadge = required<HTMLElement>("settingsEventBadge");
const pillHint = required<HTMLElement>("pillHint");
const statusLabel = required<HTMLElement>("statusLabel");
const timerEl = required<HTMLElement>("timer");
const liveTranscriptBubble = required<HTMLElement>("liveTranscriptBubble");
const liveTranscriptText = required<HTMLElement>("liveTranscriptText");
const transcriptShelf = required<HTMLElement>("transcriptShelf");

let appState: AppState = "idle";
let timerInterval: ReturnType<typeof setInterval> | null = null;
let maxRecordingTimer: ReturnType<typeof setTimeout> | null = null;
let copiedStatusTimer: ReturnType<typeof setTimeout> | null = null;
let copiedShowing = false;
let pillHovering = false;
let appliedPillLayout: PillLayout | null = null;
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
    copiedShowing = false;
  }

  appState = state;
  recordBtn.setAttribute("aria-pressed", String(state === "recording"));
  recordBtn.setAttribute(
    "aria-label",
    state === "recording" ? "Stop recording" : state === "starting" ? "Starting recording" : state === "transcribing" ? "Cancel transcription" : state === "error" ? "Recording failed, click to retry" : "Start recording",
  );
  recordBtn.title = state === "recording" ? "Stop recording" : state === "transcribing" ? "Cancel transcription" : state === "error" ? "Click to retry" : "Click to record";
  recordBtn.disabled = false;
  statusLabel.textContent = message ?? stateLabel(state);

  if (state === "recording") {
    startTimer();
    startMeter();
    scheduleCantHearHint();
  } else {
    stopTimer();
    clearMaxRecordingTimer();
    stopMeter();
    clearCantHearHint();
  }

  updatePillLayout();
}

/* ── Voice-reactive level meter ─────────────────────────────────
   The five wave bars are driven by real capture levels streamed from the
   backend as `audio-level` events; they sit flat through silence instead of
   pulsing on a loop. */

const waveBars = Array.from(document.querySelectorAll<HTMLElement>(".wave span"));
// Gate matches the backend's per-window speech RMS threshold, so the bars
// consider "voice" exactly what the transcriber's gap detection does.
const METER_GATE_RMS = 0.004;
const METER_FULL_RMS = 0.055;
const METER_FLOOR_SCALE = 0.16;
const METER_BAR_WEIGHTS = [0.62, 0.95, 1, 0.78, 0.88];
const reducedMotionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");

let meterTarget = 0;
let meterDisplay = 0;
let meterRafId: number | null = null;
let voiceHeard = false;
let cantHearTimer: ReturnType<typeof setTimeout> | null = null;

function meterLevelFromRms(rms: number): number {
  const normalized = Math.min(1, Math.max(0, (rms - METER_GATE_RMS) / (METER_FULL_RMS - METER_GATE_RMS)));
  // Square root for a perceptual response: quiet speech still visibly moves.
  return Math.sqrt(normalized);
}

function startMeter(): void {
  meterTarget = 0;
  meterDisplay = 0;
  if (meterRafId === null) meterFrame();
}

function stopMeter(): void {
  meterTarget = 0;
  if (meterRafId !== null) {
    cancelAnimationFrame(meterRafId);
    meterRafId = null;
  }
  resetMeterBars();
}

function meterFrame(): void {
  meterRafId = requestAnimationFrame((time) => {
    const attack = 0.45;
    const decay = reducedMotionQuery.matches ? 0.08 : 0.18;
    meterDisplay += (meterTarget - meterDisplay) * (meterTarget > meterDisplay ? attack : decay);
    // A slight per-bar shimmer while there is signal keeps the meter reading
    // as a waveform; with reduced motion the bars track level only.
    const shimmer = !reducedMotionQuery.matches && meterDisplay > 0.03;
    waveBars.forEach((bar, index) => {
      const wobble = shimmer ? 0.89 + 0.11 * Math.sin(time / 90 + index * 1.7) : 1;
      const scale = METER_FLOOR_SCALE + meterDisplay * METER_BAR_WEIGHTS[index] * wobble * (1 - METER_FLOOR_SCALE);
      bar.style.transform = `scaleY(${Math.min(1, Math.max(METER_FLOOR_SCALE, scale)).toFixed(3)})`;
    });
    if (appState === "recording" || meterDisplay > 0.02) {
      meterFrame();
    } else {
      meterRafId = null;
      resetMeterBars();
    }
  });
}

function resetMeterBars(): void {
  for (const bar of waveBars) {
    bar.style.transform = `scaleY(${METER_FLOOR_SCALE})`;
  }
}

/* Live "we can't hear you": if no voice crosses the gate in the first
   seconds of a recording, say so now instead of rejecting the whole
   dictation as silence after the user finishes talking. */
const CANT_HEAR_AFTER_MS = 3000;

function scheduleCantHearHint(): void {
  clearCantHearHint();
  voiceHeard = false;
  cantHearTimer = setTimeout(() => {
    cantHearTimer = null;
    if (appState === "recording" && !voiceHeard) {
      statusLabel.textContent = "Can't hear you";
    }
  }, CANT_HEAR_AFTER_MS);
}

function clearCantHearHint(): void {
  if (cantHearTimer !== null) {
    clearTimeout(cantHearTimer);
    cantHearTimer = null;
  }
}

function markVoiceHeard(): void {
  if (voiceHeard) return;
  voiceHeard = true;
  if (appState === "recording" && statusLabel.textContent === "Can't hear you") {
    statusLabel.textContent = stateLabel("recording");
  }
}

function computePillLayout(): PillLayout {
  if (appState === "starting") return "starting";
  if (appState === "recording") return "recording";
  if (appState === "transcribing") return "transcribing";
  if (appState === "error") return "error";
  if (copiedShowing) return "copied";
  return pillHovering ? "hover" : "idle";
}

// The pill window is native-resized per layout state so the idle hit area
// stays tiny; the CSS visibility matrix in pill.css follows data-pill.
function updatePillLayout(): void {
  const layout = computePillLayout();
  if (layout === appliedPillLayout) return;
  appliedPillLayout = layout;
  document.body.dataset.pill = layout;
  void invoke("layout_pill_window", { state: layout }).catch((error) =>
    addEvent("warning", `Could not lay out pill window: ${error instanceof Error ? error.message : String(error)}`),
  );
}

function stateLabel(state: AppState): string {
  if (state === "starting") return "Starting";
  if (state === "recording") return "Recording";
  if (state === "transcribing") return "Transcribing";
  if (state === "error") return "Error";
  return "Ready";
}

// Reduce a backend error to something the 280px error capsule can carry;
// the full message still lands in the Activity log.
function shortErrorMessage(message: string): string {
  const normalized = message.replace(/\s+/g, " ").trim();
  const lower = normalized.toLowerCase();
  if (lower.includes("microphone permission")) return "Mic access denied";
  if (lower.includes("no default input device") || lower.includes("input device")) return "No microphone found";
  if (lower.includes("model files are missing")) return "Model not downloaded";
  if (lower.includes("allowance used")) return "Cloud allowance used — see Settings";
  if (lower.includes("cloud sign-in expired")) return "Cloud sign-in expired — see Settings";
  if (!normalized) return "Error";
  return normalized.length <= 40 ? normalized : `${normalized.slice(0, 39)}…`;
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
  return (
    message === "Recording was too short"
    || message.startsWith("No speech detected")
    // The backend watchdog (or a stream failure) already stopped this
    // recording; the pill just returns to Ready.
    || message === "No recording in progress"
  );
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

function showCopiedStatus(): void {
  if (copiedStatusTimer !== null) {
    clearTimeout(copiedStatusTimer);
  }
  copiedShowing = true;
  setState("idle", "Copied");
  copiedStatusTimer = setTimeout(() => {
    copiedStatusTimer = null;
    copiedShowing = false;
    if (appState === "idle") {
      setState("idle", "Ready");
    } else {
      updatePillLayout();
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
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    addEvent("error", message);
    setState("error", shortErrorMessage(message));
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
    // The start click plays from the backend once capture is confirmed live.
    setState("starting");
    const maxRecordingSeconds = await invoke<number>("start_recording");
    startRecordingRequestPending = false;
    cancelTranscriptionRequestPending = false;
    setState("recording");
    resetLiveTranscript();
    scheduleMaxRecordingStop(maxRecordingSeconds);
    return true;
  } catch (error) {
    startRecordingRequestPending = false;
    stopRecordingRequestPending = false;
    const message = error instanceof Error ? error.message : String(error);
    addEvent("error", message);
    await refreshStateAfterStartError(message);
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

async function refreshStateAfterStartError(message: string): Promise<void> {
  try {
    const status = await invoke<BackendStatus>("get_app_status");
    if (status.state === "recording") {
      setState("recording", status.message);
      addEvent("warning", "Recovered recording state after a UI error");
    } else {
      setState("error", shortErrorMessage(message));
    }
  } catch {
    setState("error", shortErrorMessage(message));
  }
}

async function stopAndTranscribe(): Promise<void> {
  if (stopRecordingRequestPending) return;
  stopRecordingRequestPending = true;
  cancelTranscriptionRequestPending = false;
  clearMaxRecordingTimer();
  // The stop click plays from the backend once capture actually stops.
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
      setState("error", shortErrorMessage(message));
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

// The idle capsule is nearly invisible; entering the (tiny) pill window
// expands it to the hover layout with the wordmark, shortcut hint, and gear.
function wirePillHover(): void {
  document.body.addEventListener("mouseenter", () => {
    pillHovering = true;
    updatePillLayout();
  });
  document.body.addEventListener("mouseleave", () => {
    pillHovering = false;
    updatePillLayout();
  });
  // Native resizes can move the capsule out from under a stationary cursor
  // without a mouseleave ever firing; reconcile while the hover layout is up.
  setInterval(() => {
    if (pillHovering && !document.body.matches(":hover")) {
      pillHovering = false;
      updatePillLayout();
    }
  }, 1000);
}

function isMacOS(): boolean {
  return navigator.platform.toLowerCase().includes("mac");
}

const MAC_SHORTCUT_GLYPHS: Record<string, string> = {
  CommandOrControl: "⌘",
  Command: "⌘",
  Super: "⌘",
  Control: "⌃",
  Ctrl: "⌃",
  Shift: "⇧",
  Alt: "⌥",
  Option: "⌥",
};

function shortcutHint(shortcut: string): string {
  const parts = normalizeShortcut(shortcut)
    .split("+")
    .map((part) => part.replace(/^Digit/, "").replace(/^Key/, ""));
  if (isMacOS()) {
    return parts.map((part) => MAC_SHORTCUT_GLYPHS[part] ?? part).join("");
  }
  return parts.map((part) => (part === "CommandOrControl" ? "Ctrl" : part)).join("+");
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
  pillHint.textContent = shortcutHint(normalized.recordingShortcut);

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
  // Recopies can arrive from the home window at any time; only flash the
  // Copied capsule when the pill is not busy recording or transcribing.
  if (appState === "idle") {
    showCopiedStatus();
  }
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

void listen<ShortcutSettings>("settings-updated", (event) => {
  void registerGlobalShortcuts(event.payload);
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

// Emitted by the Rust CGEventTap when hold-Fn push-to-talk is enabled.
void listen<{ pressed: boolean }>("fn-push-to-talk", (event) => {
  if (event.payload.pressed) {
    void startPushToTalkRecording();
  } else {
    void stopPushToTalkRecording();
  }
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

// Real capture levels for the recording meter (~25 Hz while recording).
void listen<AudioLevelEvent>("audio-level", (event) => {
  meterTarget = meterLevelFromRms(event.payload.rms);
  if (meterTarget > 0) markVoiceHeard();
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

// The capture stream died mid-recording (e.g. microphone unplugged). Stop
// through the normal pipeline so whatever audio was captured is preserved.
void listen<string>("recording-stream-error", (event) => {
  addEvent("error", `Microphone stream failed: ${event.payload}`);
  if (appState === "recording" || appState === "starting") {
    void stopAndTranscribe();
  }
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

// The backend watchdog committed (or failed) a recording the webview never
// stopped; resync the pill instead of showing a stuck recording state.
void listen<RecordingAutoStoppedEvent>("recording-auto-stopped", (event) => {
  addEvent(event.payload.committed ? "info" : "warning", event.payload.message);
  if (appState !== "recording" && appState !== "transcribing") return;
  stopRecordingRequestPending = false;
  cancelTranscriptionRequestPending = false;
  resetLiveTranscript();
  if (event.payload.committed) {
    void loadTranscriptHistory().catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));
    showCopiedStatus();
  } else {
    setState("idle", "Ready");
  }
}).catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));

void loadBackendStatus().catch((error) => {
  addEvent("error", error instanceof Error ? error.message : String(error));
  setState("error", "Error");
});
void loadTranscriptHistory().catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));
void loadShortcutSettings()
  .then((settings) => registerGlobalShortcuts(settings))
  .catch((error) => addEvent("warning", error instanceof Error ? error.message : String(error)));
wirePillHover();
updatePillLayout();
updateSettingsEventBadge();
resetMeterBars();
