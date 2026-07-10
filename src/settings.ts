import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { addEvent } from "./events";

// Parakeet is the default engine; the whisper.cpp ids are only meaningful when
// the backend was built with the `whisper` Cargo feature.
type SttModel =
  | "parakeet-tdt-0.6b-v3"
  | "tiny"
  | "base"
  | "small"
  | "medium"
  | "large-v2"
  | "large-v3"
  | "large-v3-turbo";
type TranscriptionLocation = "local" | "remote-host" | "cloud";
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
  model: SttModel;
  remoteUrl: string;
  remoteAuthToken: string;
  remoteTimeoutSeconds: number;
  cloudAuthToken: string;
  language: string;
  alwaysOnTop: boolean;
  interactionSounds: boolean;
  maxRecordingSeconds: number;
  noteRetentionMinutes: number;
  whisperChunkSeconds: number;
  useGpu: boolean;
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
  insertAtCursor: boolean;
  fnPushToTalk: boolean;
  polishEnabled: boolean;
  polishTones: Record<string, string>;
  contextAwareness: boolean;
}

interface ModelStatus {
  model: SttModel;
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
  model: "parakeet-tdt-0.6b-v3",
  remoteUrl: "",
  remoteAuthToken: "",
  remoteTimeoutSeconds: 60,
  cloudAuthToken: "",
  language: "en",
  alwaysOnTop: true,
  interactionSounds: true,
  maxRecordingSeconds: 120,
  noteRetentionMinutes: 0,
  whisperChunkSeconds: 20,
  useGpu: true,
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
  insertAtCursor: isMacOS(),
  fnPushToTalk: false,
  polishEnabled: false,
  polishTones: {},
  contextAwareness: false,
};

const refreshBtn = required<HTMLButtonElement>("refreshDevices");
const locationSeg = segControl("locationSeg");
const modelSelect = required<HTMLSelectElement>("modelSelect");
const langSelect = required<HTMLSelectElement>("langSelect");
const audioDeviceSelect = required<HTMLSelectElement>("audioDeviceSelect");
const noiseSuppression = required<HTMLInputElement>("noiseSuppression");
const echoCancellation = required<HTMLInputElement>("echoCancellation");
const inputGain = required<HTMLSelectElement>("inputGain");
const postProcess = required<HTMLInputElement>("postProcess");
const alwaysOnTop = required<HTMLInputElement>("alwaysOnTop");
const interactionSounds = required<HTMLInputElement>("interactionSounds");
const maxRecordingSeconds = required<HTMLSelectElement>("maxRecordingSeconds");
const noteRetentionMinutes = required<HTMLSelectElement>("noteRetentionMinutes");
const recordingModeSeg = segControl("recordingModeSeg");
const recordingShortcutChip = required<HTMLButtonElement>("recordingShortcutChip");
const transcriptStackShortcutChip = required<HTMLButtonElement>("transcriptStackShortcutChip");
const shortcutStatus = required<HTMLElement>("shortcutStatus");
const shortcutStatusDefault = shortcutStatus.textContent ?? "";
const insertAtCursor = required<HTMLInputElement>("insertAtCursor");
const insertAtCursorRow = required<HTMLElement>("insertAtCursorRow");
const fnPushToTalk = required<HTMLInputElement>("fnPushToTalk");
const fnPushToTalkRow = required<HTMLElement>("fnPushToTalkRow");
const whisperChunkSeconds = required<HTMLInputElement>("whisperChunkSeconds");
const useGpu = required<HTMLInputElement>("useGpu");
const useGpuRow = required<HTMLElement>("useGpuRow");
const inputMeter = required<HTMLElement>("inputMeter");
const modelDownload = required<HTMLElement>("modelDownload");
const modelField = required<HTMLElement>("modelField");
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
const cloudPanel = required<HTMLElement>("cloudPanel");
const cloudAuthToken = required<HTMLInputElement>("cloudAuthToken");
const cloudStatus = required<HTMLElement>("cloudStatus");
const cloudTest = required<HTMLButtonElement>("cloudTest");
const polishEnabled = required<HTMLInputElement>("polishEnabled");
const polishHelp = required<HTMLElement>("polishHelp");
const polishHelpDefault = polishHelp.textContent ?? "";
const polishTonesPanel = required<HTMLElement>("polishTonesPanel");
const contextAwareness = required<HTMLInputElement>("contextAwareness");
const contextAwarenessRow = required<HTMLElement>("contextAwarenessRow");
// Category ids match the polish endpoint contract and the Rust setting keys.
const POLISH_TONE_CATEGORIES = ["messaging", "email", "docs", "code", "other"] as const;
const polishToneSegs: Record<string, SegControl> = {
  messaging: segControl("polishToneMessaging"),
  email: segControl("polishToneEmail"),
  docs: segControl("polishToneDocs"),
  code: segControl("polishToneCode"),
  other: segControl("polishToneOther"),
};

const MODEL_MEMORY_FOOTPRINTS: Record<SttModel, string> = {
  "parakeet-tdt-0.6b-v3": "RAM ~2.5G",
  tiny: "RAM ~0.7G",
  base: "RAM ~1.2G",
  small: "RAM ~2.5G",
  medium: "RAM ~8G",
  "large-v2": "RAM ~16G",
  "large-v3": "RAM ~16G",
  "large-v3-turbo": "RAM ~8G",
};

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

// A segmented control is a group of `aria-pressed` buttons carrying the active
// value in `data-value`; this adapts it to the same value-in/value-out + change
// contract the form expects from a <select>.
interface SegControl {
  get(): string;
  set(value: string): void;
  onChange(handler: (value: string) => void): void;
}

function segControl(id: string): SegControl {
  const root = required<HTMLElement>(id);
  const buttons = Array.from(root.querySelectorAll<HTMLButtonElement>("button[data-value]"));
  const handlers: Array<(value: string) => void> = [];
  const get = () =>
    buttons.find((button) => button.getAttribute("aria-pressed") === "true")?.dataset.value
    ?? buttons[0]?.dataset.value
    ?? "";
  const set = (value: string) => {
    for (const button of buttons) {
      button.setAttribute("aria-pressed", String(button.dataset.value === value));
    }
  };
  for (const button of buttons) {
    button.addEventListener("click", () => {
      const value = button.dataset.value ?? "";
      if (get() === value) return;
      set(value);
      for (const handler of handlers) handler(value);
    });
  }
  return { get, set, onChange: (handler) => handlers.push(handler) };
}

// Accelerators are stored in the Tauri global-shortcut grammar
// (e.g. "CommandOrControl+Shift+Digit1"); the chip renders them as keycaps.
const onMac = isMacOS();
const KEY_GLYPHS: Record<string, string> = {
  CommandOrControl: onMac ? "⌘" : "Ctrl",
  Command: "⌘", Cmd: "⌘", Meta: "⌘", Super: "⌘",
  Control: "⌃", Ctrl: "⌃",
  Alt: onMac ? "⌥" : "Alt", Option: "⌥",
  Shift: "⇧",
  ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
  Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Backslash: "\\",
  Semicolon: ";", Quote: "'", Comma: ",", Period: ".", Slash: "/", Backquote: "`",
};
const KEY_WORDS: Record<string, string> = {
  CommandOrControl: onMac ? "Command" : "Control",
  Command: "Command", Cmd: "Command", Meta: "Command", Super: "Command",
  Control: "Control", Ctrl: "Control",
  Alt: onMac ? "Option" : "Alt", Option: "Option",
  Shift: "Shift",
};

function accelTokens(accelerator: string): string[] {
  return accelerator.split("+").map((token) => token.trim()).filter(Boolean);
}

function keyGlyph(token: string): string {
  const digit = /^(?:Digit|Numpad)([0-9])$/.exec(token);
  if (digit) return digit[1];
  return KEY_GLYPHS[token] ?? token;
}

function keyWord(token: string): string {
  const digit = /^(?:Digit|Numpad)([0-9])$/.exec(token);
  if (digit) return digit[1];
  return KEY_WORDS[token] ?? token;
}

// Paint the keycaps and keep the chip's accessible name describing the current
// shortcut and the rebind affordance; `data-shortcut` is the form's value source.
function renderShortcutChip(chip: HTMLButtonElement, accelerator: string): void {
  const normalized = normalizeShortcut(accelerator, accelerator);
  chip.dataset.shortcut = normalized;
  const tokens = accelTokens(normalized);
  const keys = chip.querySelector<HTMLElement>(".keys");
  if (keys) {
    keys.replaceChildren(...tokens.map((token) => {
      const span = document.createElement("span");
      span.className = "key";
      span.textContent = keyGlyph(token);
      return span;
    }));
  }
  const name = chip.dataset.label ?? "Shortcut";
  chip.setAttribute("aria-label", `${name}: ${tokens.map(keyWord).join(" ")}. Click to change.`);
}

// Mirror the options of the #maxRecordingSeconds and #noteRetentionMinutes
// selects; settings saved by older builds or edited by hand can hold values
// between the presets, so snap them to the closest one.
const MAX_RECORDING_CHOICES = [30, 60, 120, 180, 300, 600];
const NOTE_RETENTION_CHOICES = [0, 15, 60, 480, 1440, 10080, 43200];

function snapToChoice(value: number, choices: number[], fallback: number): number {
  const target = Number.isFinite(value) ? value : fallback;
  return choices.reduce((closest, choice) =>
    Math.abs(choice - target) < Math.abs(closest - target) ? choice : closest,
  );
}

function normalizeSettings(settings: Partial<Settings>): Settings {
  const seconds = Number(settings.maxRecordingSeconds ?? DEFAULTS.maxRecordingSeconds);
  const retentionMinutes = Number(settings.noteRetentionMinutes ?? DEFAULTS.noteRetentionMinutes);
  const chunkSeconds = Number(settings.whisperChunkSeconds ?? DEFAULTS.whisperChunkSeconds);
  return {
    ...DEFAULTS,
    ...settings,
    inputGain: Math.max(1, Math.min(6, Number(settings.inputGain ?? DEFAULTS.inputGain))),
    maxRecordingSeconds: snapToChoice(seconds, MAX_RECORDING_CHOICES, DEFAULTS.maxRecordingSeconds),
    noteRetentionMinutes: snapToChoice(retentionMinutes, NOTE_RETENTION_CHOICES, DEFAULTS.noteRetentionMinutes),
    whisperChunkSeconds: Math.max(5, Math.min(60, Math.round(chunkSeconds || DEFAULTS.whisperChunkSeconds))),
    remoteUrl: (settings.remoteUrl ?? "").trim().replace(/\/+$/, ""),
    remoteTimeoutSeconds: Math.max(5, Math.min(300, Math.round(Number(settings.remoteTimeoutSeconds ?? DEFAULTS.remoteTimeoutSeconds)))),
    cloudAuthToken: (settings.cloudAuthToken ?? "").trim(),
    polishEnabled: settings.polishEnabled ?? DEFAULTS.polishEnabled,
    polishTones: normalizePolishTones(settings.polishTones ?? DEFAULTS.polishTones),
    contextAwareness: settings.contextAwareness ?? DEFAULTS.contextAwareness,
    vocabularyHints: normalizeVocabularyHints(settings.vocabularyHints ?? DEFAULTS.vocabularyHints),
    transcriptCorrections: normalizeTranscriptCorrections(settings.transcriptCorrections ?? DEFAULTS.transcriptCorrections),
    recordingShortcut: normalizeShortcut(settings.recordingShortcut ?? DEFAULTS.recordingShortcut, DEFAULTS.recordingShortcut),
    recordingShortcutMode: settings.recordingShortcutMode === "push-to-talk" ? "push-to-talk" : "toggle",
    transcriptStackShortcut: normalizeShortcut(settings.transcriptStackShortcut ?? DEFAULTS.transcriptStackShortcut, DEFAULTS.transcriptStackShortcut),
    insertAtCursor: settings.insertAtCursor ?? DEFAULTS.insertAtCursor,
    fnPushToTalk: settings.fnPushToTalk ?? DEFAULTS.fnPushToTalk,
    useGpu: settings.useGpu ?? DEFAULTS.useGpu,
  };
}

function applyToForm(settings: Settings): void {
  locationSeg.set(settings.transcriptionLocation);
  modelSelect.value = settings.model;
  updateModelSize(settings.model);
  remoteUrl.value = settings.remoteUrl;
  remoteAuthToken.value = settings.remoteAuthToken;
  remoteTimeoutSeconds.value = String(settings.remoteTimeoutSeconds);
  cloudAuthToken.value = settings.cloudAuthToken;
  langSelect.value = settings.language;
  audioDeviceSelect.value = settings.audioDevice ?? "";
  noiseSuppression.checked = settings.noiseSuppression ?? true;
  echoCancellation.checked = settings.echoCancellation ?? true;
  inputGain.value = String(settings.inputGain ?? 2);
  postProcess.checked = settings.postProcess ?? true;
  alwaysOnTop.checked = settings.alwaysOnTop;
  interactionSounds.checked = settings.interactionSounds ?? true;
  maxRecordingSeconds.value = String(settings.maxRecordingSeconds);
  noteRetentionMinutes.value = String(settings.noteRetentionMinutes);
  recordingModeSeg.set(settings.recordingShortcutMode);
  renderShortcutChip(recordingShortcutChip, settings.recordingShortcut);
  renderShortcutChip(transcriptStackShortcutChip, settings.transcriptStackShortcut);
  insertAtCursor.checked = settings.insertAtCursor;
  fnPushToTalk.checked = settings.fnPushToTalk;
  whisperChunkSeconds.value = String(settings.whisperChunkSeconds);
  useGpu.checked = settings.useGpu;
  polishEnabled.checked = settings.polishEnabled;
  contextAwareness.checked = settings.contextAwareness;
  for (const category of POLISH_TONE_CATEGORIES) {
    polishToneSegs[category].set(settings.polishTones[category] ?? "default");
  }
  updateTranscriptionLocationUi(settings.transcriptionLocation);
  updateUseGpuUi();
  updatePolishUi(settings);
}

// Polish needs the cloud token even in Local mode (transcript text goes to
// the same Worker). Without one, the toggle is disabled and says why. The
// per-category tone rows only matter while polish is on.
function updatePolishUi(settings: Settings): void {
  const hasToken = settings.cloudAuthToken.trim() !== "";
  polishEnabled.disabled = !hasToken;
  polishHelp.textContent = hasToken
    ? polishHelpDefault
    : "Requires a MultiVoice Cloud token (add one under Location → MultiVoice Cloud).";
  polishTonesPanel.hidden = !hasToken || !settings.polishEnabled;
}

// Mirror of the Rust normalization: only known categories and tones survive,
// and redundant "default" entries are dropped.
function normalizePolishTones(tones: Record<string, string>): Record<string, string> {
  const normalized: Record<string, string> = {};
  for (const category of POLISH_TONE_CATEGORIES) {
    const tone = tones[category];
    if (tone === "casual" || tone === "formal" || tone === "off") {
      normalized[category] = tone;
    }
  }
  return normalized;
}

function readFromForm(): Settings {
  return normalizeSettings({
    transcriptionLocation: locationSeg.get() as TranscriptionLocation,
    model: modelSelect.value as SttModel,
    remoteUrl: remoteUrl.value,
    remoteAuthToken: remoteAuthToken.value,
    remoteTimeoutSeconds: Number(remoteTimeoutSeconds.value),
    cloudAuthToken: cloudAuthToken.value,
    language: langSelect.value,
    audioDevice: audioDeviceSelect.value,
    noiseSuppression: noiseSuppression.checked,
    echoCancellation: echoCancellation.checked,
    inputGain: Number(inputGain.value),
    postProcess: postProcess.checked,
    alwaysOnTop: alwaysOnTop.checked,
    interactionSounds: interactionSounds.checked,
    maxRecordingSeconds: Number(maxRecordingSeconds.value),
    noteRetentionMinutes: Number(noteRetentionMinutes.value),
    recordingShortcutMode: recordingModeSeg.get() as RecordingShortcutMode,
    recordingShortcut: recordingShortcutChip.dataset.shortcut ?? DEFAULTS.recordingShortcut,
    transcriptStackShortcut: transcriptStackShortcutChip.dataset.shortcut ?? DEFAULTS.transcriptStackShortcut,
    whisperChunkSeconds: Number(whisperChunkSeconds.value),
    useGpu: useGpu.checked,
    insertAtCursor: insertAtCursor.checked,
    fnPushToTalk: fnPushToTalk.checked,
    polishEnabled: polishEnabled.checked,
    contextAwareness: contextAwareness.checked,
    polishTones: Object.fromEntries(
      POLISH_TONE_CATEGORIES.map((category) => [category, polishToneSegs[category].get()]),
    ),
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

function beginShortcutCapture(chip: HTMLButtonElement, label: string): void {
  if (chip.classList.contains("capturing")) return;
  chip.classList.add("capturing");
  shortcutStatus.textContent = "Press a key combination, or Esc to cancel.";

  const stopCapture = (message?: string) => {
    window.removeEventListener("keydown", onKeyDown, true);
    chip.classList.remove("capturing");
    shortcutStatus.textContent = message ?? shortcutStatusDefault;
  };

  const onKeyDown = (event: KeyboardEvent) => {
    event.preventDefault();
    event.stopPropagation();
    const shortcut = shortcutFromKeyboardEvent(event);
    if (shortcut === null) {
      stopCapture();
      return;
    }
    if (!shortcut) {
      shortcutStatus.textContent = "Use a modifier key or a function key.";
      return;
    }

    renderShortcutChip(chip, shortcut);
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
  if (locationSeg.get() !== "local") {
    updateModelSize(modelSelect.value as SttModel);
    return;
  }
  const model = modelSelect.value as SttModel;
  updateModelSize(model);
  modelPrepare.textContent = "Checking";
  modelPrepare.disabled = true;
  modelDownload.dataset.state = "loading";
  modelDownloadStatus.textContent = "Checking model...";
  modelDownloadBar.style.transform = "scaleX(0.01)";
  const status = await invoke<ModelStatus>("get_transcription_model_status", {
    request: { model },
  });
  addEvent(status.cached ? "info" : "warning", status.message);
  setModelCacheStatus(status);
}

async function beginModelPreload(): Promise<void> {
  if (locationSeg.get() !== "local") return;
  const model = modelSelect.value as SttModel;
  updateModelSize(model);
  setModelDownloadStatus("Preparing model...", 1);
  modelPrepare.textContent = "Preparing";
  modelPrepare.disabled = true;
  try {
    await invoke("begin_prepare_transcription_model", {
      request: { model },
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
  if (event.model !== modelSelect.value) return;

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

function updateModelSize(model: SttModel): void {
  modelSize.textContent = MODEL_MEMORY_FOOTPRINTS[model] ?? "";
}

// The GPU toggle only affects local Whisper inference: a remote host's GPU
// use is the host operator's configuration, and the Parakeet engine is
// CPU-only — showing a toggle it ignores would be a lie.
function updateUseGpuUi(): void {
  const local = locationSeg.get() === "local";
  const gpuCapableEngine = !modelSelect.value.startsWith("parakeet");
  useGpuRow.hidden = !local || !gpuCapableEngine;
  useGpu.disabled = !local || !gpuCapableEngine;
}

function updateTranscriptionLocationUi(location: TranscriptionLocation): void {
  const local = location === "local";
  // The model choice and its download state are local-transcription concerns;
  // a remote host (or MultiVoice Cloud) serves whatever its operator runs.
  modelField.hidden = !local;
  modelDownload.hidden = !local;
  remoteHostPanel.hidden = location !== "remote-host";
  cloudPanel.hidden = location !== "cloud";
  if (location === "remote-host") {
    remoteStatus.textContent = currentSettings.remoteUrl ? "Remote host not checked" : "Remote host not configured";
  }
  if (location === "cloud") {
    cloudStatus.textContent = currentSettings.cloudAuthToken
      ? "Allowance: shown after first dictation"
      : "Paste a token to enable MultiVoice Cloud";
  }
}

// Both remote targets speak the same health protocol, so one test flow serves
// both panels; the backend resolves URL + token from the saved location.
async function testRemoteHost(statusEl: HTMLElement, buttonEl: HTMLButtonElement, label: string): Promise<void> {
  const saved = await persistSettings();
  if (!saved) return;
  buttonEl.disabled = true;
  statusEl.textContent = `Checking ${label}...`;
  try {
    const health = await invoke<RemoteHealth>("test_remote_transcription_host");
    statusEl.textContent = health.ok ? `Reachable (${health.mode})` : "Unavailable";
    addEvent(health.ok ? "info" : "warning", `${label} ${health.ok ? "reachable" : "unavailable"}: ${health.backend}`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    statusEl.textContent = `${label} unavailable`;
    addEvent("error", message);
  } finally {
    buttonEl.disabled = false;
  }
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

locationSeg.onChange((value) => {
  updateTranscriptionLocationUi(value as TranscriptionLocation);
  updateUseGpuUi();
  void persistSettings()
    .then((saved) => {
      if (!saved) return;
      const label = value === "local" ? "local" : value === "cloud" ? "MultiVoice Cloud" : "remote host";
      addEvent("info", `Transcription location changed to ${label}`);
      return requestModelStatus();
    })
    .catch(reportAsyncError);
});

modelSelect.addEventListener("change", () => {
  updateUseGpuUi();
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
  void testRemoteHost(remoteStatus, remoteTest, "Remote host");
});
cloudTest.addEventListener("click", () => {
  void testRemoteHost(cloudStatus, cloudTest, "MultiVoice Cloud");
});
remoteUrl.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
remoteAuthToken.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
cloudAuthToken.addEventListener("change", () => {
  void persistSettings()
    .then(() => updatePolishUi(currentSettings))
    .catch(reportAsyncError);
});
polishEnabled.addEventListener("change", () => {
  void persistSettings()
    .then(() => updatePolishUi(currentSettings))
    .catch(reportAsyncError);
});
for (const category of POLISH_TONE_CATEGORIES) {
  polishToneSegs[category].onChange(() => void persistSettings().catch(reportAsyncError));
}
contextAwareness.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
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
        // Preview through the same Rust playback path the real clicks use.
        return invoke("preview_interaction_sound", { sound: "recording-start" });
      }
    })
    .catch(reportAsyncError);
});
maxRecordingSeconds.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
noteRetentionMinutes.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
recordingModeSeg.onChange(() => void persistSettings().catch(reportAsyncError));
recordingShortcutChip.addEventListener("click", () => {
  beginShortcutCapture(recordingShortcutChip, "Recording");
});
transcriptStackShortcutChip.addEventListener("click", () => {
  beginShortcutCapture(transcriptStackShortcutChip, "Copied messages");
});
whisperChunkSeconds.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
whisperChunkSeconds.addEventListener("input", () => void persistSettings().catch(reportAsyncError));
useGpu.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
insertAtCursor.addEventListener("change", () => void persistSettings().catch(reportAsyncError));
fnPushToTalk.addEventListener("change", () => void persistSettings().catch(reportAsyncError));

// Both delivery integrations are macOS-only (CGEvent paste and the Fn event
// tap); hide rather than disable them elsewhere so the form stays honest.
function isMacOS(): boolean {
  return navigator.platform.toLowerCase().includes("mac");
}
if (!isMacOS()) {
  insertAtCursorRow.hidden = true;
  fnPushToTalkRow.hidden = true;
  // Context awareness reads the macOS Accessibility tree; there is no
  // Windows/Linux implementation yet.
  contextAwarenessRow.hidden = true;
}

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
