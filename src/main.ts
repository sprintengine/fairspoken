import "./appearance"; // the pill follows the app's light or dark choice
import { replaceShortcuts, type BindingRole, type Bindings } from "./shortcutRegistration";
import { isMacOS, normalizeShortcut, shortcutHint } from "./shortcuts";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { isRegistered, register, unregister } from "@tauri-apps/plugin-global-shortcut";
import { addEvent, addEventWithId, eventSeverity, type EventLevel } from "./events";
import { required } from "./dom";
import { errorMessage } from "./errors";

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

// The pill only reacts to history events (the polish undo offer); the copied
// messages themselves are listed by the transcript-shelf window.
interface TranscriptHistoryItem {
  id: string;
  polished?: boolean;
  rawText?: string | null;
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

type PillLayout = "idle" | "hover" | "starting" | "recording" | "transcribing" | "copied" | "polished" | "error";

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
const undoPolishBtn = required<HTMLButtonElement>("undoPolishBtn");
const settingsBtn = required<HTMLButtonElement>("settingsBtn");
const settingsEventBadge = required<HTMLElement>("settingsEventBadge");
const pillHint = required<HTMLElement>("pillHint");
const statusLabel = required<HTMLElement>("statusLabel");
const timerEl = required<HTMLElement>("timer");

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
let shortcutSettings: ShortcutSettings = { ...DEFAULT_SHORTCUT_SETTINGS };
let pushToTalkReleasePending = false;
let shortcutRegistrationQueue: Promise<void> = Promise.resolve();
let activeBindings: Bindings = { recording: "", stack: "" };
const POLISH_UNDO_WINDOW_MS = 8_000;
let polishUndoOffer: { id: string } | null = null;
let polishUndoTimer: ReturnType<typeof setTimeout> | null = null;

function setState(state: AppState, message?: string): void {
  if (copiedStatusTimer !== null && state !== "idle") {
    clearTimeout(copiedStatusTimer);
    copiedStatusTimer = null;
    copiedShowing = false;
  }
  // Leaving idle (next recording, error, …) withdraws the undo affordance.
  if (state !== "idle") {
    dropPolishUndoOffer();
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
// Conversational speech sits well below the old 0.055 ceiling, so the bars
// spent their time near the floor and barely moved. This is a normal speaking
// voice at full scale; louder speech simply clamps.
const METER_FULL_RMS = 0.026;
const METER_FLOOR_SCALE = 0.12;
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
  if (polishUndoOffer !== null) return "polished";
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
    addEvent("warning", `Could not lay out pill window: ${errorMessage(error)}`),
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

function showCopiedStatus(): void {
  // The polished state carries the undo affordance instead of the Copied
  // flash — but this function is the success path's ONLY transition out of
  // "transcribing", and the history event that sets the offer arrives
  // BEFORE the stop invoke resolves. So the offer must still complete the
  // idle transition here, or the pill wedges in "Transcribing" forever.
  if (polishUndoOffer !== null) {
    setState("idle", "AI polished");
    return;
  }
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

/* ── AI polish undo affordance ─────────────────────────────────
   After a polished transcript pastes, the pill shows "AI polished" with an
   Undo button for 8 s (or until the next recording). Undo copies the RAW
   transcript to the clipboard — deliberately not a synthetic ⌘Z+⌘V or AX
   replacement, both rejected as fragile. */

function offerPolishUndo(id: string): void {
  if (copiedStatusTimer !== null) {
    clearTimeout(copiedStatusTimer);
    copiedStatusTimer = null;
    copiedShowing = false;
  }
  polishUndoOffer = { id };
  if (polishUndoTimer !== null) clearTimeout(polishUndoTimer);
  polishUndoTimer = setTimeout(() => {
    dropPolishUndoOffer();
    if (appState === "idle") setState("idle", "Ready");
  }, POLISH_UNDO_WINDOW_MS);
  if (appState === "idle") {
    setState("idle", "AI polished");
  } else {
    updatePillLayout();
  }
}

/** Withdraw the offer without touching the pill state (callers decide). */
function dropPolishUndoOffer(): void {
  if (polishUndoTimer !== null) {
    clearTimeout(polishUndoTimer);
    polishUndoTimer = null;
  }
  polishUndoOffer = null;
}

async function undoPolishedTranscript(): Promise<void> {
  const offer = polishUndoOffer;
  if (offer === null) return;
  dropPolishUndoOffer();
  try {
    await invoke("copy_original_transcript", { id: offer.id });
    addEvent("info", "Original transcript copied — paste to replace.");
    if (appState === "idle") {
      copiedShowing = true;
      setState("idle", "Original copied");
      copiedStatusTimer = setTimeout(() => {
        copiedStatusTimer = null;
        copiedShowing = false;
        if (appState === "idle") setState("idle", "Ready");
      }, 1400);
    }
  } catch (error) {
    addEvent("warning", errorMessage(error));
    if (appState === "idle") setState("idle", "Ready");
  }
}

function updateSettingsEventBadge(): void {
  const severity = eventSeverity();
  settingsEventBadge.dataset.severity = severity ?? "none";
  settingsEventBadge.hidden = severity === null;
  settingsBtn.title = severity === "error" ? "Settings: errors recorded" : severity === "warning" ? "Settings: warnings recorded" : "Settings";
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
    scheduleMaxRecordingStop(maxRecordingSeconds);
    return true;
  } catch (error) {
    startRecordingRequestPending = false;
    stopRecordingRequestPending = false;
    const message = errorMessage(error);
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
  // The stop click plays from the backend once capture actually stops. The
  // Transcribing state is set (and its pill layout requested) before the stop
  // is sent; the frame paints while the request is in flight rather than
  // delaying it by two animation frames.
  setState("transcribing");

  try {
    const transcript = await invoke<string>("stop_and_transcribe");
    if (cancelTranscriptionRequestPending) {
      setState("idle", "Ready");
      return;
    }
    if (transcript) {
      addEvent("info", "Transcript copied to clipboard");
      showCopiedStatus();
    } else {
      addEvent("warning", "No transcript returned");
      setState("idle", "Ready");
    }
  } catch (error) {
    const message = errorMessage(error);
    if (message === "Transcription was cancelled") {
      addEvent("info", "Transcription cancelled");
      setState("idle", "Ready");
    } else if (isRecoverableRecordingStopError(message)) {
      addEvent("warning", message);
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
    addEvent("warning", errorMessage(error));
  }
}

async function toggleTranscriptStack(): Promise<void> {
  try {
    await emit("transcript-shelf-toggle");
  } catch (error) {
    addEvent("warning", errorMessage(error));
  }
}

// The idle capsule is nearly invisible; entering the (tiny) pill window
// expands it to the hover layout with the wordmark, shortcut hint, and gear.
function wirePillHover(): void {
  // Native resizes can move the capsule out from under a stationary cursor
  // without a mouseleave ever firing; reconcile while the hover layout is up,
  // and only then.
  let hoverPoll: ReturnType<typeof setInterval> | null = null;
  const endHover = () => {
    pillHovering = false;
    if (hoverPoll !== null) {
      clearInterval(hoverPoll);
      hoverPoll = null;
    }
    updatePillLayout();
  };
  document.body.addEventListener("mouseenter", () => {
    pillHovering = true;
    if (hoverPoll === null) {
      hoverPoll = setInterval(() => {
        if (!document.body.matches(":hover")) endHover();
      }, 1000);
    }
    updatePillLayout();
  });
  document.body.addEventListener("mouseleave", endHover);
}

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
      if (event.state === "Pressed") void toggleTranscriptStack();
    } else if (shortcutSettings.recordingShortcutMode === "push-to-talk") {
      if (event.state === "Pressed") void startPushToTalkRecording();
      else void stopPushToTalkRecording();
    } else if (event.state === "Pressed") void toggleRecording();
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
async function registerGlobalShortcuts(settings = shortcutSettings): Promise<void> {
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

async function loadShortcutSettings(): Promise<ShortcutSettings> {
  try {
    return normalizeShortcutSettings(await invoke<ShortcutSettings>("get_settings"));
  } catch (error) {
    addEvent("warning", `Could not load shortcut settings; using defaults: ${errorMessage(error)}`);
    return { ...DEFAULT_SHORTCUT_SETTINGS };
  }
}

recordBtn.addEventListener("click", () => {
  void toggleRecording();
});

undoPolishBtn.addEventListener("click", () => {
  void undoPolishedTranscript();
});

settingsBtn.addEventListener("click", () => {
  void invoke("open_home_window", { screen: "settings" });
});

window.addEventListener("fairspoken-events-updated", updateSettingsEventBadge);
window.addEventListener("storage", (event) => {
  if (event.key === "fairspoken-events") {
    updateSettingsEventBadge();
  }
});

void listen<BackendLogEvent>("backend-event", (event) => {
  addEventWithId(event.payload.id, event.payload.level, event.payload.message);
}).catch((error) => addEvent("warning", errorMessage(error)));

void listen<TranscriptHistoryUpdatedEvent>("transcript-history-updated", (event) => {
  if (event.payload.item.polished && event.payload.item.rawText) {
    offerPolishUndo(event.payload.item.id);
  }
}).catch((error) => addEvent("warning", errorMessage(error)));

void listen("transcript-copied", () => {
  // Recopies can arrive from the home window at any time; only flash the
  // Copied capsule when the pill is not busy recording or transcribing.
  if (appState === "idle") {
    showCopiedStatus();
  }
}).catch((error) => addEvent("warning", errorMessage(error)));

void listen<ShortcutSettings>("settings-updated", (event) => {
  void registerGlobalShortcuts(event.payload);
}).catch((error) => addEvent("warning", errorMessage(error)));

// Emitted by the Rust CGEventTap when hold-Fn push-to-talk is enabled.
void listen<{ pressed: boolean }>("fn-push-to-talk", (event) => {
  if (event.payload.pressed) {
    void startPushToTalkRecording();
  } else {
    void stopPushToTalkRecording();
  }
}).catch((error) => addEvent("warning", errorMessage(error)));

// Real capture levels for the recording meter (~25 Hz while recording).
void listen<AudioLevelEvent>("audio-level", (event) => {
  meterTarget = meterLevelFromRms(event.payload.rms);
  if (meterTarget > 0) markVoiceHeard();
}).catch((error) => addEvent("warning", errorMessage(error)));

// The capture stream died mid-recording (e.g. microphone unplugged). Stop
// through the normal pipeline so whatever audio was captured is preserved.
void listen<string>("recording-stream-error", (event) => {
  addEvent("error", `Microphone stream failed: ${event.payload}`);
  if (appState === "recording" || appState === "starting") {
    void stopAndTranscribe();
  }
}).catch((error) => addEvent("warning", errorMessage(error)));

// The backend watchdog committed (or failed) a recording the webview never
// stopped; resync the pill instead of showing a stuck recording state.
void listen<RecordingAutoStoppedEvent>("recording-auto-stopped", (event) => {
  addEvent(event.payload.committed ? "info" : "warning", event.payload.message);
  if (appState !== "recording" && appState !== "transcribing") return;
  stopRecordingRequestPending = false;
  cancelTranscriptionRequestPending = false;
  if (event.payload.committed) {
    showCopiedStatus();
  } else {
    setState("idle", "Ready");
  }
}).catch((error) => addEvent("warning", errorMessage(error)));

void loadBackendStatus().catch((error) => {
  addEvent("error", errorMessage(error));
  setState("error", "Error");
});
void loadShortcutSettings()
  .then((settings) => registerGlobalShortcuts(settings))
  .catch((error) => addEvent("warning", errorMessage(error)));
wirePillHover();
updatePillLayout();
updateSettingsEventBadge();
resetMeterBars();
