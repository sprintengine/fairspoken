import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { addEvent } from "./events";
import { playRecordingStartSound, setInteractionSoundsEnabled } from "./sounds";

type WhisperModel = "tiny" | "base" | "small" | "medium" | "large-v2" | "large-v3" | "large-v3-turbo";
type TranscriptionBackend = "whisper" | "sherpa-streaming";
type TranscriptionLocation = "local" | "remote-host";
type SherpaModel = "streaming-zipformer-en-2023-06-26-int8";
type RecordingShortcutMode = "toggle" | "push-to-talk";

interface TranscriptCorrection {
  enabled: boolean;
  from: string;
  to: string;
  caseSensitive: boolean;
  wholePhrase: boolean;
}

interface Settings {
  transcriptionLocation: TranscriptionLocation;
  transcriptionBackend: TranscriptionBackend;
  model: WhisperModel;
  sherpaModel: SherpaModel;
  remoteUrl: string;
  remoteAuthToken: string;
  remoteTimeoutSeconds: number;
  language: string;
  alwaysOnTop: boolean;
  interactionSounds: boolean;
  maxRecordingSeconds: number;
  whisperChunkSeconds: number;
  audioDevice?: string;
  noiseSuppression?: boolean;
  echoCancellation?: boolean;
  inputGain?: number;
  postProcess?: boolean;
  vocabularyHints?: string[];
  transcriptCorrections?: TranscriptCorrection[];
  recordingShortcut: string;
  recordingShortcutMode: RecordingShortcutMode;
  transcriptStackShortcut: string;
}

interface ModelStatus {
  backend?: TranscriptionBackend;
  model: WhisperModel | SherpaModel;
  cached: boolean;
  message: string;
  modelPath: string;
}

interface RemoteHealth {
  ok: boolean;
  mode: string;
  backend: string;
  serverVersion?: string;
}

interface ModelPrepareProgressEvent {
  backend: TranscriptionBackend;
  model: string;
  stage: string;
  message: string;
  percentage: number;
  done: boolean;
  error?: string | null;
  status?: ModelStatus | null;
}

const DEFAULTS: Settings = {
  transcriptionLocation: "local",
  transcriptionBackend: "whisper",
  model: "base",
  sherpaModel: "streaming-zipformer-en-2023-06-26-int8",
  remoteUrl: "",
  remoteAuthToken: "",
  remoteTimeoutSeconds: 60,
  language: "en",
  alwaysOnTop: true,
  interactionSounds: true,
  maxRecordingSeconds: 120,
  whisperChunkSeconds: 20,
  audioDevice: "",
  noiseSuppression: true,
  echoCancellation: true,
  inputGain: 2,
  postProcess: true,
  vocabularyHints: [],
  transcriptCorrections: [],
  recordingShortcut: "CommandOrControl+Shift+Digit1",
  recordingShortcutMode: "toggle",
  transcriptStackShortcut: "CommandOrControl+Shift+Digit2",
};

const refreshBtn = required<HTMLButtonElement>("refreshDevices");
const locationSelect = required<HTMLSelectElement>("locationSelect");
const engineSelect = required<HTMLSelectElement>("engineSelect");
const modelSelect = required<HTMLSelectElement>("modelSelect");
const langSelect = required<HTMLSelectElement>("langSelect");
const audioDeviceSelect = required<HTMLSelectElement>("audioDeviceSelect");
const noiseSuppression = required<HTMLInputElement>("noiseSuppression");
const echoCancellation = required<HTMLInputElement>("echoCancellation");
const inputGain = required<HTMLSelectElement>("inputGain");
const postProcess = required<HTMLInputElement>("postProcess");
const alwaysOnTop = required<HTMLInputElement>("alwaysOnTop");
const interactionSounds = required<HTMLInputElement>("interactionSounds");
const maxRecordingSeconds = required<HTMLInputElement>("maxRecordingSeconds");
const recordingShortcutMode = required<HTMLSelectElement>("recordingShortcutMode");
const recordingShortcut = required<HTMLInputElement>("recordingShortcut");
const recordingShortcutCapture = required<HTMLButtonElement>("recordingShortcutCapture");
const transcriptStackShortcut = required<HTMLInputElement>("transcriptStackShortcut");
const transcriptStackShortcutCapture = required<HTMLButtonElement>("transcriptStackShortcutCapture");
const shortcutStatus = required<HTMLElement>("shortcutStatus");
const whisperChunkField = required<HTMLElement>("whisperChunkField");
const whisperChunkSeconds = required<HTMLInputElement>("whisperChunkSeconds");
const inputMeter = required<HTMLElement>("inputMeter");
const modelDownload = required<HTMLElement>("modelDownload");
const modelDownloadStatus = required<HTMLElement>("modelDownloadStatus");
const modelDownloadBar = required<HTMLElement>("modelDownloadBar");
const modelSize = required<HTMLElement>("modelSize");
const modelPrepare = required<HTMLButtonElement>("modelPrepare");
const remoteHostPanel = required<HTMLElement>("remoteHostPanel");
const remoteUrl = required<HTMLInputElement>("remoteUrl");
const remoteAuthToken = required<HTMLInputElement>("remoteAuthToken");
const remoteTimeoutSeconds = required<HTMLInputElement>("remoteTimeoutSeconds");
const remoteStatus = required<HTMLElement>("remoteStatus");
const remoteTest = required<HTMLButtonElement>("remoteTest");

const MODEL_MEMORY_FOOTPRINTS: Record<WhisperModel, string> = {
  tiny: "RAM ~0.7G",
  base: "RAM ~1.2G",
  small: "RAM ~2.5G",
  medium: "RAM ~8G",
  "large-v2": "RAM ~16G",
  "large-v3": "RAM ~16G",
  "large-v3-turbo": "RAM ~8G",
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
  ["large-v3-turbo", "Large v3 Turbo"],
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
  const chunkSeconds = Number(settings.whisperChunkSeconds ?? DEFAULTS.whisperChunkSeconds);
  return {
    ...DEFAULTS,
    ...settings,
    inputGain: Math.max(1, Math.min(6, Number(settings.inputGain ?? DEFAULTS.inputGain))),
    maxRecordingSeconds: Math.max(10, Math.min(300, Math.round(seconds || DEFAULTS.maxRecordingSeconds))),
    whisperChunkSeconds: Math.max(5, Math.min(60, Math.round(chunkSeconds || DEFAULTS.whisperChunkSeconds))),
    remoteUrl: (settings.remoteUrl ?? "").trim().replace(/\/+$/, ""),
    remoteTimeoutSeconds: Math.max(5, Math.min(300, Math.round(Number(settings.remoteTimeoutSeconds ?? DEFAULTS.remoteTimeoutSeconds)))),
    vocabularyHints: normalizeVocabularyHints(settings.vocabularyHints ?? DEFAULTS.vocabularyHints),
    transcriptCorrections: normalizeTranscriptCorrections(settings.transcriptCorrections ?? DEFAULTS.transcriptCorrections),
    recordingShortcut: normalizeShortcut(settings.recordingShortcut ?? DEFAULTS.recordingShortcut, DEFAULTS.recordingShortcut),
    recordingShortcutMode: settings.recordingShortcutMode === "push-to-talk" ? "push-to-talk" : "toggle",
    transcriptStackShortcut: normalizeShortcut(settings.transcriptStackShortcut ?? DEFAULTS.transcriptStackShortcut, DEFAULTS.transcriptStackShortcut),
  };
}

function applyToForm(settings: Settings): void {
  locationSelect.value = settings.transcriptionLocation;
  engineSelect.value = settings.transcriptionBackend;
  renderModelOptions(settings.transcriptionBackend);
  modelSelect.value = selectedModel(settings);
  updateModelSize(settings.transcriptionBackend, selectedModel(settings));
  remoteUrl.value = settings.remoteUrl;
  remoteAuthToken.value = settings.remoteAuthToken;
  remoteTimeoutSeconds.value = String(settings.remoteTimeoutSeconds);
  langSelect.value = settings.language;
  audioDeviceSelect.value = settings.audioDevice ?? "";
  noiseSuppression.checked = settings.noiseSuppression ?? true;
  echoCancellation.checked = settings.echoCancellation ?? true;
  inputGain.value = String(settings.inputGain ?? 2);
  postProcess.checked = settings.postProcess ?? true;
  alwaysOnTop.checked = settings.alwaysOnTop;
  interactionSounds.checked = settings.interactionSounds ?? true;
  maxRecordingSeconds.value = String(settings.maxRecordingSeconds);
  recordingShortcutMode.value = settings.recordingShortcutMode;
  recordingShortcut.value = settings.recordingShortcut;
  transcriptStackShortcut.value = settings.transcriptStackShortcut;
  whisperChunkSeconds.value = String(settings.whisperChunkSeconds);
  updateWhisperChunkUi(settings.transcriptionBackend);
  updateTranscriptionLocationUi(settings.transcriptionLocation);
}

function readFromForm(): Settings {
  const backend = engineSelect.value as TranscriptionBackend;
  return normalizeSettings({
    transcriptionLocation: locationSelect.value as TranscriptionLocation,
    transcriptionBackend: backend,
    model: backend === "whisper" ? (modelSelect.value as WhisperModel) : currentSettings.model,
    sherpaModel: backend === "sherpa-streaming" ? (modelSelect.value as SherpaModel) : currentSettings.sherpaModel,
    remoteUrl: remoteUrl.value,
    remoteAuthToken: remoteAuthToken.value,
    remoteTimeoutSeconds: Number(remoteTimeoutSeconds.value),
    language: langSelect.value,
    audioDevice: audioDeviceSelect.value,
    noiseSuppression: noiseSuppression.checked,
    echoCancellation: echoCancellation.checked,
    inputGain: Number(inputGain.value),
    postProcess: postProcess.checked,
    alwaysOnTop: alwaysOnTop.checked,
    interactionSounds: interactionSounds.checked,
    maxRecordingSeconds: Number(maxRecordingSeconds.value),
    recordingShortcutMode: recordingShortcutMode.value as RecordingShortcutMode,
    recordingShortcut: recordingShortcut.value,
    transcriptStackShortcut: transcriptStackShortcut.value,
    whisperChunkSeconds: Number(whisperChunkSeconds.value),
  });
}

function normalizeVocabularyHints(hints: string[] = []): string[] {
  const normalized: string[] = [];
  for (const hint of hints) {
    const value = cleanSettingText(hint, 100);
    if (!value || normalized.includes(value)) continue;
    normalized.push(value);
    if (normalized.length >= 50) break;
  }
  return normalized;
}

function normalizeTranscriptCorrections(corrections: TranscriptCorrection[] = []): TranscriptCorrection[] {
  const normalized: TranscriptCorrection[] = [];
  for (const correction of corrections) {
    const from = cleanSettingText(correction.from, 120);
    const to = cleanSettingText(correction.to, 120);
    if (!from || !to) continue;
    normalized.push({
      enabled: correction.enabled !== false,
      from,
      to,
      caseSensitive: correction.caseSensitive === true,
      wholePhrase: correction.wholePhrase !== false,
    });
    if (normalized.length >= 100) break;
  }
  return normalized;
}

function cleanSettingText(value: string, maxLength: number): string {
  return value.replace(/\0/g, "").replace(/\s+/g, " ").trim().slice(0, maxLength);
}

function normalizeShortcut(value: string, fallback: string): string {
  const normalized = value
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean)
    .join("+")
    .slice(0, 80);
  return normalized || fallback;
}

function shortcutFromKeyboardEvent(event: KeyboardEvent): string | null {
  if (event.key === "Escape") return null;
  if (["Shift", "Control", "Alt", "Meta"].includes(event.key)) return "";

  const key = shortcutKeyName(event);
  if (!key) return "";

  const modifiers: string[] = [];
  if (event.metaKey || event.ctrlKey) modifiers.push("CommandOrControl");
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

function beginShortcutCapture(target: HTMLInputElement, button: HTMLButtonElement, label: string): void {
  shortcutStatus.textContent = "Press a key combination, or Esc to cancel.";
  button.textContent = "Listening";
  button.disabled = true;

  const stopCapture = (message?: string) => {
    window.removeEventListener("keydown", onKeyDown, true);
    button.textContent = "Record";
    button.disabled = false;
    if (message) shortcutStatus.textContent = message;
  };

  const onKeyDown = (event: KeyboardEvent) => {
    event.preventDefault();
    event.stopPropagation();
    const shortcut = shortcutFromKeyboardEvent(event);
    if (shortcut === null) {
      stopCapture("Shortcut capture canceled.");
      return;
    }
    if (!shortcut) {
      shortcutStatus.textContent = "Use a modifier key or a function key.";
      return;
    }

    target.value = shortcut;
    stopCapture(`${label} shortcut set to ${shortcut}.`);
    void persistSettings().catch(reportAsyncError);
  };

  window.addEventListener("keydown", onKeyDown, true);
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
  if (locationSelect.value === "remote-host") {
    updateModelSize(engineSelect.value as TranscriptionBackend, modelSelect.value as WhisperModel | SherpaModel);
    return;
  }
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
  if (locationSelect.value === "remote-host") return;
  const backend = engineSelect.value as TranscriptionBackend;
  const model = modelSelect.value as WhisperModel | SherpaModel;
  updateModelSize(backend, model);
  setModelDownloadStatus("Preparing model...", 1);
  modelPrepare.textContent = "Preparing";
  modelPrepare.disabled = true;
  try {
    await invoke("begin_prepare_transcription_model", {
      request: modelRequest(backend, model),
    });
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

function handleModelPrepareProgress(event: ModelPrepareProgressEvent): void {
  const backend = engineSelect.value as TranscriptionBackend;
  const model = modelSelect.value as WhisperModel | SherpaModel;
  if (event.backend !== backend || event.model !== model) return;

  if (event.error) {
    addEvent("error", event.error);
    modelDownload.dataset.state = "error";
    modelDownloadStatus.textContent = "Preparation failed";
    modelDownloadStatus.title = event.error;
    modelDownloadBar.style.transform = "scaleX(0)";
    modelPrepare.textContent = "Retry";
    modelPrepare.disabled = false;
    return;
  }

  if (event.status && event.done) {
    addEvent("info", event.status.message);
    setModelCacheStatus(event.status);
    return;
  }

  setModelDownloadStatus(formatPrepareStage(event.stage, event.message), event.percentage);
  modelPrepare.textContent = event.done ? "Ready" : "Preparing";
  modelPrepare.disabled = !event.done;
}

function formatPrepareStage(stage: string, message: string): string {
  if (stage === "downloading") return message || "Downloading model";
  if (stage === "unpacking") return "Unpacking model";
  if (stage === "validating") return "Validating model";
  if (stage === "ready") return "Model ready";
  return message || "Preparing model";
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

function updateWhisperChunkUi(backend: TranscriptionBackend): void {
  const whisper = backend === "whisper";
  whisperChunkField.hidden = !whisper;
  whisperChunkSeconds.disabled = !whisper;
}

function updateTranscriptionLocationUi(location: TranscriptionLocation): void {
  const remote = location === "remote-host";
  modelDownload.hidden = remote;
  remoteHostPanel.hidden = !remote;
  if (remote) {
    remoteStatus.textContent = currentSettings.remoteUrl ? "Remote host not checked" : "Remote host not configured";
  }
}

async function testRemoteHost(): Promise<void> {
  const saved = await persistSettings();
  if (!saved) return;
  remoteTest.disabled = true;
  remoteStatus.textContent = "Checking remote host...";
  try {
    const health = await invoke<RemoteHealth>("test_remote_transcription_host");
    remoteStatus.textContent = health.ok ? `Reachable (${health.mode})` : "Unavailable";
    addEvent(health.ok ? "info" : "warning", `Remote host ${health.ok ? "reachable" : "unavailable"}: ${health.backend}`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    remoteStatus.textContent = "Remote host unavailable";
    addEvent("error", message);
  } finally {
    remoteTest.disabled = false;
  }
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

let meterScreenVisible = false;

// The mic meter is the one always-on cost on this window: getUserMedia holds the
// microphone open and a rAF loop runs every frame. Scope it to when the Settings
// screen is actually on-screen and the window is visible, so navigating to
// another screen — or hiding the window — releases the mic and stops the loop.
function refreshMeter(): void {
  if (meterScreenVisible && document.visibilityState === "visible") {
    void startMeter();
  } else {
    stopMeter();
  }
}

async function loadSettings(): Promise<void> {
  currentSettings = normalizeSettings(await invoke<Settings>("get_settings"));
  applyToForm(currentSettings);
  await loadAudioDevices();
  if (currentSettings.transcriptionLocation === "local") {
    await requestModelStatus();
  }
}

refreshBtn.addEventListener("click", () => {
  void loadAudioDevices();
  refreshMeter();
});

locationSelect.addEventListener("change", () => {
  updateTranscriptionLocationUi(locationSelect.value as TranscriptionLocation);
  void persistSettings()
    .then((saved) => {
      if (!saved) return;
      addEvent("info", `Transcription location changed to ${locationSelect.value === "local" ? "local" : "remote host"}`);
      return requestModelStatus();
    })
    .catch(reportAsyncError);
});

engineSelect.addEventListener("change", () => {
  const backend = engineSelect.value as TranscriptionBackend;
  renderModelOptions(backend);
  updateWhisperChunkUi(backend);
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
remoteTest.addEventListener("click", () => {
  void testRemoteHost();
});
remoteUrl.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
remoteAuthToken.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
remoteTimeoutSeconds.addEventListener("change", () => void persistSettings().catch(reportAsyncError));

audioDeviceSelect.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) refreshMeter();
    })
    .catch(reportAsyncError);
});
noiseSuppression.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) refreshMeter();
    })
    .catch(reportAsyncError);
});
echoCancellation.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) refreshMeter();
    })
    .catch(reportAsyncError);
});
inputGain.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved) refreshMeter();
    })
    .catch(reportAsyncError);
});
langSelect.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
postProcess.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
alwaysOnTop.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
interactionSounds.addEventListener("change", () => {
  void persistSettings()
    .then((saved) => {
      if (saved && interactionSounds.checked) {
        setInteractionSoundsEnabled(true);
        playRecordingStartSound();
      }
    })
    .catch(reportAsyncError);
});
maxRecordingSeconds.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
maxRecordingSeconds.addEventListener("input", () => void persistSettings().catch(reportAsyncError));
recordingShortcutMode.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
recordingShortcutCapture.addEventListener("click", () => {
  beginShortcutCapture(recordingShortcut, recordingShortcutCapture, "Recording");
});
transcriptStackShortcutCapture.addEventListener("click", () => {
  beginShortcutCapture(transcriptStackShortcut, transcriptStackShortcutCapture, "Copied messages");
});
whisperChunkSeconds.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
whisperChunkSeconds.addEventListener("input", () => void persistSettings().catch(reportAsyncError));

// Settings auto-persist on change; the form must never submit/navigate, which
// in the home window would reload the whole webview.
document.getElementById("settingsForm")?.addEventListener("submit", (event) => event.preventDefault());

// Run the meter only while the Settings screen is on-screen. IntersectionObserver
// reports the screen as not-intersecting whenever home.ts toggles it to
// display:none, which releases the mic; navigating back restarts it.
const settingsScreen = required<HTMLElement>("screen-settings");
new IntersectionObserver((entries) => {
  meterScreenVisible = entries.some((entry) => entry.isIntersecting);
  refreshMeter();
}).observe(settingsScreen);

document.addEventListener("visibilitychange", refreshMeter);
window.addEventListener("beforeunload", stopMeter);

void listen<ModelPrepareProgressEvent>("model-prepare-progress", (event) => {
  handleModelPrepareProgress(event.payload);
}).catch(reportAsyncError);

void loadSettings().catch((error) => {
  const message = error instanceof Error ? error.message : String(error);
  addEvent("error", error instanceof Error ? error.message : String(error));
  modelDownload.dataset.state = "error";
  modelDownloadStatus.textContent = "Settings failed to load";
  modelDownloadStatus.title = message;
});
