import { invoke } from "@tauri-apps/api/core";
import { register } from "@tauri-apps/plugin-global-shortcut";
import { addEvent, eventSeverity } from "./events";

type AppState = "idle" | "recording" | "transcribing" | "error";

interface BackendStatus {
  state: AppState;
  message: string;
}

const RECORD_SHORTCUT_MACOS = "Command+Shift+1";
const RECORD_SHORTCUT_DEFAULT = "Ctrl+Alt+1";

const app = required<HTMLElement>("app");
const recordBtn = required<HTMLButtonElement>("recordBtn");
const settingsBtn = required<HTMLButtonElement>("settingsBtn");
const settingsEventBadge = required<HTMLElement>("settingsEventBadge");
const copyIndicator = required<HTMLElement>("copyIndicator");
const statusLabel = required<HTMLElement>("statusLabel");
const timerEl = required<HTMLElement>("timer");

let appState: AppState = "idle";
let timerInterval: ReturnType<typeof setInterval> | null = null;
let maxRecordingTimer: ReturnType<typeof setTimeout> | null = null;
let copyIndicatorTimer: ReturnType<typeof setTimeout> | null = null;
let copiedStatusTimer: ReturnType<typeof setTimeout> | null = null;
let recordingSeconds = 0;
let startRecordingRequestPending = false;
let stopRecordingRequestPending = false;

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
  recordBtn.setAttribute("aria-label", state === "recording" ? "Stop recording" : "Start recording");
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

async function loadBackendStatus(): Promise<void> {
  const status = await invoke<BackendStatus>("get_app_status");
  setState(status.state, status.message);
}

async function toggleRecording(): Promise<void> {
  if (appState === "transcribing") return;
  if (appState === "idle" && startRecordingRequestPending) return;

  try {
    if (appState === "recording") {
      await stopAndTranscribe();
      return;
    }

    startRecordingRequestPending = true;
    const maxRecordingSeconds = await invoke<number>("start_recording");
    startRecordingRequestPending = false;
    setState("recording");
    scheduleMaxRecordingStop(maxRecordingSeconds);
  } catch (error) {
    startRecordingRequestPending = false;
    stopRecordingRequestPending = false;
    addEvent("error", error instanceof Error ? error.message : String(error));
    setState("error", "Error");
  }
}

async function stopAndTranscribe(): Promise<void> {
  if (stopRecordingRequestPending) return;
  stopRecordingRequestPending = true;
  clearMaxRecordingTimer();
  setState("transcribing");
  await waitForPaint();

  try {
    const transcript = await invoke<string>("stop_and_transcribe");
    if (transcript) {
      addEvent("info", "Transcript copied to clipboard");
      showCopiedStatus();
      showCopyIndicator();
    } else {
      addEvent("warning", "No transcript returned");
      setState("idle", "Ready");
    }
  } catch (error) {
    addEvent("error", error instanceof Error ? error.message : String(error));
    setState("error", "Error");
  } finally {
    stopRecordingRequestPending = false;
  }
}

function isMacOS(): boolean {
  return navigator.platform.toLowerCase().includes("mac");
}

async function registerRecordingShortcut(): Promise<void> {
  const shortcut = isMacOS() ? RECORD_SHORTCUT_MACOS : RECORD_SHORTCUT_DEFAULT;

  try {
    await register(shortcut, (event) => {
      if (event.state === "Pressed") {
        void toggleRecording();
      }
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    addEvent("warning", `Recording shortcut ${shortcut} is unavailable: ${message}`);
  }
}

recordBtn.addEventListener("click", () => {
  void toggleRecording();
});

settingsBtn.addEventListener("click", () => {
  void invoke("open_settings_window");
});

window.addEventListener("multivoice-events-updated", updateSettingsEventBadge);
window.addEventListener("storage", (event) => {
  if (event.key === "multivoice-tauri-events") {
    updateSettingsEventBadge();
  }
});

void loadBackendStatus().catch((error) => {
  addEvent("error", error instanceof Error ? error.message : String(error));
  setState("error", "Error");
});
void registerRecordingShortcut();
updateSettingsEventBadge();
