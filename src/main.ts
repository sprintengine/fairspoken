import "./appearance"; // the pill follows the app's light or dark choice
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { resetMeterBars, setMeterLevel, startMeter, stopMeter } from "./pillMeter";
import { initPillShortcuts, loadShortcutSettings, registerGlobalShortcuts, type ShortcutSettings } from "./pillShortcuts";
import { copyOriginalTranscript, dropPolishUndoOffer, offerPolishUndo, polishUndoOffered } from "./polishUndo";
import { addEvent, addEventWithId, eventSeverity, type EventLevel } from "./events";
import { required } from "./dom";
import { errorMessage } from "./errors";

// The pill window: the recording state machine and its capsule layout. The
// wave meter, global shortcuts and the AI polish undo offer live in
// pillMeter.ts, pillShortcuts.ts and polishUndo.ts.

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

type PillLayout = "idle" | "hover" | "starting" | "recording" | "transcribing" | "copied" | "polished" | "error";

interface AudioLevelEvent {
  peak: number;
  rms: number;
}

interface RecordingAutoStoppedEvent {
  committed: boolean;
  message: string;
}

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
let pushToTalkReleasePending = false;

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
    startMeter(() => appState === "recording");
    scheduleCantHearHint();
  } else {
    stopTimer();
    clearMaxRecordingTimer();
    stopMeter();
    clearCantHearHint();
  }

  updatePillLayout();
}

/* Live "we can't hear you": if no voice crosses the gate in the first
   seconds of a recording, say so now instead of rejecting the whole
   dictation as silence after the user finishes talking. */
const CANT_HEAR_AFTER_MS = 3000;
let voiceHeard = false;
let cantHearTimer: ReturnType<typeof setTimeout> | null = null;

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
  if (polishUndoOffered()) return "polished";
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
  if (polishUndoOffered()) {
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

// The AI polish undo offer (polishUndo.ts) takes over the Copied flash.
function showPolishUndo(id: string): void {
  if (copiedStatusTimer !== null) {
    clearTimeout(copiedStatusTimer);
    copiedStatusTimer = null;
    copiedShowing = false;
  }
  offerPolishUndo(id, () => {
    if (appState === "idle") setState("idle", "Ready");
  });
  if (appState === "idle") {
    setState("idle", "AI polished");
  } else {
    updatePillLayout();
  }
}

async function undoPolishedTranscript(): Promise<void> {
  const result = await copyOriginalTranscript();
  if (result === "none" || appState !== "idle") return;
  if (result === "failed") {
    setState("idle", "Ready");
    return;
  }
  copiedShowing = true;
  setState("idle", "Original copied");
  copiedStatusTimer = setTimeout(() => {
    copiedStatusTimer = null;
    copiedShowing = false;
    if (appState === "idle") setState("idle", "Ready");
  }, 1400);
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
    showPolishUndo(event.payload.item.id);
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
  if (setMeterLevel(event.payload.rms)) markVoiceHeard();
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
initPillShortcuts(pillHint, {
  toggleRecording: () => void toggleRecording(),
  pushToTalk: (pressed) => void (pressed ? startPushToTalkRecording() : stopPushToTalkRecording()),
  toggleStack: () => void toggleTranscriptStack(),
});
void loadShortcutSettings()
  .then((settings) => registerGlobalShortcuts(settings))
  .catch((error) => addEvent("warning", errorMessage(error)));
wirePillHover();
updatePillLayout();
updateSettingsEventBadge();
resetMeterBars();
