import { invoke } from "@tauri-apps/api/core";
import { addEvent, eventSeverity } from "./events";

type AppState = "idle" | "recording" | "transcribing" | "error";

interface BackendStatus {
  state: AppState;
  message: string;
}

const app = required<HTMLElement>("app");
const recordBtn = required<HTMLButtonElement>("recordBtn");
const settingsBtn = required<HTMLButtonElement>("settingsBtn");
const settingsEventBadge = required<HTMLElement>("settingsEventBadge");
const copyIndicator = required<HTMLElement>("copyIndicator");
const statusLabel = required<HTMLElement>("statusLabel");
const timerEl = required<HTMLElement>("timer");

let appState: AppState = "idle";
let timerInterval: ReturnType<typeof setInterval> | null = null;
let copyIndicatorTimer: ReturnType<typeof setTimeout> | null = null;
let copiedStatusTimer: ReturnType<typeof setTimeout> | null = null;
let recordingSeconds = 0;

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

function formatTime(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return `${minutes}:${remainder.toString().padStart(2, "0")}`;
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

  try {
    if (appState === "recording") {
      setState("transcribing");
      const transcript = await invoke<string>("stop_and_transcribe");
      if (transcript) {
        addEvent("info", "Transcript copied to clipboard");
        showCopiedStatus();
        showCopyIndicator();
      } else {
        addEvent("warning", "No transcript returned");
        setState("idle", "Ready");
      }
      return;
    }

    await invoke("start_recording");
    setState("recording");
  } catch (error) {
    addEvent("error", error instanceof Error ? error.message : String(error));
    setState("error", "Error");
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
updateSettingsEventBadge();
