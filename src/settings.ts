import { invoke } from "@tauri-apps/api/core";
import { addEvent, clearEvents, eventSeverity, readEvents, type AppEvent } from "./events";

type WhisperModel = "tiny" | "base" | "small" | "medium" | "large-v2" | "large-v3";
type TranscriptionBackend = "whisper" | "sherpa-streaming";
type SherpaModel = "streaming-zipformer-en-2023-06-26-int8";

interface Settings {
  transcriptionBackend: TranscriptionBackend;
  model: WhisperModel;
  sherpaModel: SherpaModel;
  language: string;
  alwaysOnTop: boolean;
  maxRecordingSeconds: number;
  audioDevice?: string;
  noiseSuppression?: boolean;
  echoCancellation?: boolean;
  inputGain?: number;
  postProcess?: boolean;
}

interface ModelStatus {
  backend?: TranscriptionBackend;
  model: WhisperModel | SherpaModel;
  cached: boolean;
  message: string;
  modelPath: string;
}

const DEFAULTS: Settings = {
  transcriptionBackend: "whisper",
  model: "base",
  sherpaModel: "streaming-zipformer-en-2023-06-26-int8",
  language: "en",
  alwaysOnTop: true,
  maxRecordingSeconds: 120,
  audioDevice: "",
  noiseSuppression: true,
  echoCancellation: true,
  inputGain: 2,
  postProcess: true,
};

const closeBtn = required<HTMLButtonElement>("settingsClose");
const refreshBtn = required<HTMLButtonElement>("refreshDevices");
const engineSelect = required<HTMLSelectElement>("engineSelect");
const modelSelect = required<HTMLSelectElement>("modelSelect");
const langSelect = required<HTMLSelectElement>("langSelect");
const audioDeviceSelect = required<HTMLSelectElement>("audioDeviceSelect");
const noiseSuppression = required<HTMLInputElement>("noiseSuppression");
const echoCancellation = required<HTMLInputElement>("echoCancellation");
const inputGain = required<HTMLSelectElement>("inputGain");
const postProcess = required<HTMLInputElement>("postProcess");
const alwaysOnTop = required<HTMLInputElement>("alwaysOnTop");
const maxRecordingSeconds = required<HTMLInputElement>("maxRecordingSeconds");
const inputMeter = required<HTMLElement>("inputMeter");
const modelDownload = required<HTMLElement>("modelDownload");
const modelDownloadStatus = required<HTMLElement>("modelDownloadStatus");
const modelDownloadBar = required<HTMLElement>("modelDownloadBar");
const modelSize = required<HTMLElement>("modelSize");
const modelPrepare = required<HTMLButtonElement>("modelPrepare");
const eventLog = required<HTMLElement>("eventLog");
const eventCount = required<HTMLElement>("eventCount");
const eventSummary = required<HTMLElement>("eventSummary");
const clearEventsButton = required<HTMLButtonElement>("clearEventsButton");
const tabButtons = Array.from(document.querySelectorAll<HTMLButtonElement>(".settings-tab"));
const tabPanels = Array.from(document.querySelectorAll<HTMLElement>(".settings-tab-panel"));

const MODEL_MEMORY_FOOTPRINTS: Record<WhisperModel, string> = {
  tiny: "RAM ~0.7G",
  base: "RAM ~1.2G",
  small: "RAM ~2.5G",
  medium: "RAM ~8G",
  "large-v2": "RAM ~16G",
  "large-v3": "RAM ~16G",
};

const SHERPA_MODEL_FOOTPRINTS: Record<SherpaModel, string> = {
  "streaming-zipformer-en-2023-06-26-int8": "RAM ~0.8G",
};

const WHISPER_MODEL_OPTIONS: Array<[WhisperModel, string]> = [
  ["tiny", "Tiny"],
  ["base", "Base"],
  ["small", "Small"],
  ["medium", "Medium"],
  ["large-v2", "Large v2"],
  ["large-v3", "Large v3"],
];

const SHERPA_MODEL_OPTIONS: Array<[SherpaModel, string]> = [
  ["streaming-zipformer-en-2023-06-26-int8", "Zipformer English int8"],
];

let currentSettings: Settings = { ...DEFAULTS };
let meterStream: MediaStream | null = null;
let meterContext: AudioContext | null = null;
let meterAnimation: number | null = null;
let meterSessionId = 0;

function required<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`Missing #${id}`);
  return node as T;
}

function normalizeSettings(settings: Partial<Settings>): Settings {
  const seconds = Number(settings.maxRecordingSeconds ?? DEFAULTS.maxRecordingSeconds);
  return {
    ...DEFAULTS,
    ...settings,
    inputGain: Math.max(1, Math.min(6, Number(settings.inputGain ?? DEFAULTS.inputGain))),
    maxRecordingSeconds: Math.max(10, Math.min(300, Math.round(seconds || DEFAULTS.maxRecordingSeconds))),
  };
}

function applyToForm(settings: Settings): void {
  engineSelect.value = settings.transcriptionBackend;
  renderModelOptions(settings.transcriptionBackend);
  modelSelect.value = selectedModel(settings);
  updateModelSize(settings.transcriptionBackend, selectedModel(settings));
  langSelect.value = settings.language;
  audioDeviceSelect.value = settings.audioDevice ?? "";
  noiseSuppression.checked = settings.noiseSuppression ?? true;
  echoCancellation.checked = settings.echoCancellation ?? true;
  inputGain.value = String(settings.inputGain ?? 2);
  postProcess.checked = settings.postProcess ?? true;
  alwaysOnTop.checked = settings.alwaysOnTop;
  maxRecordingSeconds.value = String(settings.maxRecordingSeconds);
}

function readFromForm(): Settings {
  const backend = engineSelect.value as TranscriptionBackend;
  return normalizeSettings({
    transcriptionBackend: backend,
    model: backend === "whisper" ? (modelSelect.value as WhisperModel) : currentSettings.model,
    sherpaModel: backend === "sherpa-streaming" ? (modelSelect.value as SherpaModel) : currentSettings.sherpaModel,
    language: langSelect.value,
    audioDevice: audioDeviceSelect.value,
    noiseSuppression: noiseSuppression.checked,
    echoCancellation: echoCancellation.checked,
    inputGain: Number(inputGain.value),
    postProcess: postProcess.checked,
    alwaysOnTop: alwaysOnTop.checked,
    maxRecordingSeconds: Number(maxRecordingSeconds.value),
  });
}

async function persistSettings(): Promise<boolean> {
  const nextSettings = readFromForm();
  try {
    await invoke("save_settings", { settings: nextSettings });
    currentSettings = nextSettings;
    addEvent("info", "Settings saved");
    return true;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    addEvent("error", message);
    applyToForm(currentSettings);
    return false;
  }
}

function reportAsyncError(error: unknown): void {
  addEvent("error", error instanceof Error ? error.message : String(error));
}

function setModelDownloadStatus(status: string, percentage: number): void {
  const isError = percentage < 0;
  const clamped = isError ? 0 : Math.max(0, Math.min(100, Math.round(percentage)));

  modelDownload.dataset.state = isError ? "error" : clamped >= 100 ? "ready" : clamped > 0 ? "loading" : "idle";
  modelDownloadStatus.textContent = isError ? status : `${status} ${clamped}%`;
  modelDownloadBar.style.transform = `scaleX(${clamped / 100})`;
}

function setModelCacheStatus(status: ModelStatus): void {
  modelDownload.dataset.state = status.cached ? "ready" : "idle";
  modelDownloadStatus.textContent = formatModelStatus(status);
  modelDownloadStatus.title = status.message;
  modelDownloadBar.style.transform = `scaleX(${status.cached ? 1 : 0})`;
  modelPrepare.textContent = status.cached ? "Ready" : "Prepare";
  modelPrepare.disabled = status.cached;
}

function formatModelStatus(status: ModelStatus): string {
  if (status.cached) return "Downloaded";
  if (/checksum|failed/i.test(status.message)) return "Cache needs repair";
  return "Not downloaded";
}

async function requestModelStatus(): Promise<void> {
  const backend = engineSelect.value as TranscriptionBackend;
  const model = modelSelect.value as WhisperModel | SherpaModel;
  updateModelSize(backend, model);
  modelPrepare.textContent = "Checking";
  modelPrepare.disabled = true;
  modelDownload.dataset.state = "loading";
  modelDownloadStatus.textContent = "Checking model...";
  modelDownloadBar.style.transform = "scaleX(0.01)";
  const status = await invoke<ModelStatus>("get_transcription_model_status", {
    request: modelRequest(backend, model),
  });
  addEvent(status.cached ? "info" : "warning", status.message);
  setModelCacheStatus(status);
}

async function beginModelPreload(): Promise<void> {
  const backend = engineSelect.value as TranscriptionBackend;
  const model = modelSelect.value as WhisperModel | SherpaModel;
  updateModelSize(backend, model);
  setModelDownloadStatus("Downloading model...", 1);
  modelPrepare.textContent = "Preparing";
  modelPrepare.disabled = true;
  try {
    const status = await invoke<ModelStatus>("prepare_transcription_model", {
      request: modelRequest(backend, model),
    });
    addEvent("info", status.message);
    setModelCacheStatus(status);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    addEvent("error", error instanceof Error ? error.message : String(error));
    modelDownload.dataset.state = "error";
    modelDownloadStatus.textContent = "Download failed";
    modelDownloadStatus.title = message;
    modelPrepare.textContent = "Retry";
    modelPrepare.disabled = false;
  }
}

function updateModelSize(backend: TranscriptionBackend, model: WhisperModel | SherpaModel): void {
  modelSize.textContent =
    backend === "whisper" ? MODEL_MEMORY_FOOTPRINTS[model as WhisperModel] : SHERPA_MODEL_FOOTPRINTS[model as SherpaModel];
}

function renderModelOptions(backend: TranscriptionBackend): void {
  const options = backend === "whisper" ? WHISPER_MODEL_OPTIONS : SHERPA_MODEL_OPTIONS;
  modelSelect.replaceChildren(...options.map(([value, label]) => new Option(label, value)));
}

function selectedModel(settings: Settings): WhisperModel | SherpaModel {
  return settings.transcriptionBackend === "whisper" ? settings.model : settings.sherpaModel;
}

function modelRequest(backend: TranscriptionBackend, model: WhisperModel | SherpaModel): Record<string, unknown> {
  return {
    backend,
    model: backend === "whisper" ? model : null,
    sherpaModel: backend === "sherpa-streaming" ? model : null,
  };
}

async function loadAudioDevices(): Promise<void> {
  const selected = audioDeviceSelect.value || currentSettings.audioDevice || "";
  const devices = await navigator.mediaDevices?.enumerateDevices().catch(() => []) ?? [];
  const inputs = devices.filter((device) => device.kind === "audioinput");

  audioDeviceSelect.replaceChildren(new Option("Default microphone", ""));
  inputs.forEach((device, index) => {
    audioDeviceSelect.appendChild(new Option(device.label || `Microphone ${index + 1}`, device.deviceId));
  });

  if (Array.from(audioDeviceSelect.options).some((option) => option.value === selected)) {
    audioDeviceSelect.value = selected;
  }
}

async function startMeter(): Promise<void> {
  stopMeter();
  const sessionId = ++meterSessionId;
  const settings = readFromForm();

  try {
    const constraints: MediaTrackConstraints = {
      sampleRate: 16000,
      channelCount: 1,
      echoCancellation: settings.echoCancellation,
      noiseSuppression: settings.noiseSuppression,
    };
    if (settings.audioDevice) {
      constraints.deviceId = { exact: settings.audioDevice };
    }

    const stream = await navigator.mediaDevices.getUserMedia({ audio: constraints });
    if (sessionId !== meterSessionId) {
      stream.getTracks().forEach((track) => track.stop());
      return;
    }

    const context = new AudioContext({ sampleRate: 16000 });
    meterStream = stream;
    meterContext = context;
    const source = context.createMediaStreamSource(stream);
    const analyser = context.createAnalyser();
    analyser.fftSize = 128;
    source.connect(analyser);

    const data = new Uint8Array(analyser.frequencyBinCount);
    const tick = () => {
      if (sessionId !== meterSessionId) return;
      analyser.getByteTimeDomainData(data);
      let sumSq = 0;
      for (let i = 0; i < data.length; i += 1) {
        const value = (data[i] - 128) / 128;
        sumSq += value * value;
      }
      const rms = Math.sqrt(sumSq / data.length);
      inputMeter.style.transform = `scaleX(${Math.min(1, rms * Number(inputGain.value || 2) * 8).toFixed(3)})`;
      meterAnimation = requestAnimationFrame(tick);
    };
    tick();
    await loadAudioDevices();
  } catch {
    if (sessionId === meterSessionId) {
      addEvent("warning", "Microphone meter could not start");
      inputMeter.style.transform = "scaleX(0)";
    }
  }
}

function stopMeter(): void {
  meterSessionId += 1;
  if (meterAnimation !== null) {
    cancelAnimationFrame(meterAnimation);
    meterAnimation = null;
  }
  meterStream?.getTracks().forEach((track) => track.stop());
  meterStream = null;
  if (meterContext && meterContext.state !== "closed") {
    void meterContext.close();
  }
  meterContext = null;
}

async function loadSettings(): Promise<void> {
  currentSettings = normalizeSettings(await invoke<Settings>("get_settings"));
  applyToForm(currentSettings);
  await loadAudioDevices();
  await startMeter();
  await requestModelStatus();
  renderEventLog();
}

function renderEventLog(): void {
  const events = readEvents();
  eventCount.textContent = String(events.length);
  const severity = eventSeverity(events);
  eventCount.dataset.severity = severity ?? "none";
  eventSummary.textContent = eventSummaryText(events);
  eventLog.replaceChildren(...events.slice(0, 50).map(renderEvent));
  if (events.length === 0) {
    const empty = document.createElement("p");
    empty.className = "event-empty";
    empty.textContent = "No events recorded.";
    eventLog.appendChild(empty);
  }
}

function renderEvent(event: AppEvent): HTMLElement {
  const row = document.createElement("article");
  row.className = `event-row ${event.level}`;

  const meta = document.createElement("span");
  meta.className = "event-meta";
  meta.textContent = `${formatEventTime(event.timestamp)} ${event.level}`;

  const message = document.createElement("span");
  message.className = "event-message";
  message.textContent = event.message;

  row.append(meta, message);
  return row;
}

function eventSummaryText(events: AppEvent[]): string {
  const errors = events.filter((event) => event.level === "error").length;
  const warnings = events.filter((event) => event.level === "warning").length;
  if (errors > 0) return `${errors} error${errors === 1 ? "" : "s"} recorded`;
  if (warnings > 0) return `${warnings} warning${warnings === 1 ? "" : "s"} recorded`;
  return events.length === 0 ? "No events recorded" : "No warnings or errors";
}

function selectTab(tabName: string): void {
  tabButtons.forEach((button) => {
    const selected = button.dataset.tab === tabName;
    button.setAttribute("aria-selected", String(selected));
  });
  tabPanels.forEach((panel) => {
    panel.classList.toggle("active", panel.dataset.panel === tabName);
  });
}

function formatEventTime(timestamp: string): string {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

closeBtn.addEventListener("click", () => {
  stopMeter();
  void invoke("close_settings_window");
});

refreshBtn.addEventListener("click", () => {
  void loadAudioDevices();
  void startMeter();
});

engineSelect.addEventListener("change", () => {
  const backend = engineSelect.value as TranscriptionBackend;
  renderModelOptions(backend);
  modelSelect.value = backend === "whisper" ? currentSettings.model : currentSettings.sherpaModel;
  void persistSettings()
    .then((saved) => {
      if (!saved) return;
      addEvent("info", `Transcription engine changed to ${backend === "whisper" ? "Whisper" : "Sherpa streaming"}`);
      return requestModelStatus();
    })
    .catch((error) => addEvent("error", error instanceof Error ? error.message : String(error)));
});

modelSelect.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) return requestModelStatus();
    })
    .catch(reportAsyncError);
});

modelPrepare.addEventListener("click", () => {
  void beginModelPreload();
});

audioDeviceSelect.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) return startMeter();
    })
    .catch(reportAsyncError);
});
noiseSuppression.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) return startMeter();
    })
    .catch(reportAsyncError);
});
echoCancellation.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) return startMeter();
    })
    .catch(reportAsyncError);
});
inputGain.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) return startMeter();
    })
    .catch(reportAsyncError);
});
langSelect.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
postProcess.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
alwaysOnTop.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
maxRecordingSeconds.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
maxRecordingSeconds.addEventListener("input", () => void persistSettings().catch(reportAsyncError));

window.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    stopMeter();
    void invoke("close_settings_window");
  }
});
window.addEventListener("beforeunload", stopMeter);
window.addEventListener("multivoice-events-updated", renderEventLog);
window.addEventListener("storage", (event) => {
  if (event.key === "multivoice-tauri-events") {
    renderEventLog();
  }
});
clearEventsButton.addEventListener("click", () => {
  clearEvents();
  renderEventLog();
});
tabButtons.forEach((button) => {
  button.addEventListener("click", () => selectTab(button.dataset.tab ?? "general"));
});

void loadSettings().catch((error) => {
  const message = error instanceof Error ? error.message : String(error);
  addEvent("error", error instanceof Error ? error.message : String(error));
  modelDownload.dataset.state = "error";
  modelDownloadStatus.textContent = "Settings failed to load";
  modelDownloadStatus.title = message;
  renderEventLog();
});
