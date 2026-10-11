import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { addEvent } from "./events";
import "./formatMappings";
import { isMacOS } from "./shortcuts";
import { speechModelName } from "./speechModels";
import { required } from "./dom";
import { errorMessage } from "./errors";
import {
  DEFAULTS,
  POLISH_TONE_CATEGORIES,
  normalizeSettings,
  sameSettings,
  splitPaths,
  type LocalPolishPrompt,
  type RecordingShortcutMode,
  type Settings,
  type SettingsFormHost,
  type SttModel,
  type SuperMode,
  type TranscriptionLocation,
} from "./settingsSchema";
import { initShortcutCapture, readShortcutChips, renderShortcutChips } from "./shortcutCapture";
import { initHostPairing, showRemoteHost, testRemoteHost } from "./hostPairing";
import { initMicMeter, loadAudioDevices, refreshMeter } from "./micMeter";
import { initModelPreparation, requestModelStatus, showSettingsLoadError, updateModelSize } from "./modelPreparation";

// The Settings screen: one form whose controls auto-save on change. This file
// owns the saved snapshot, reading and applying the form, and the visibility
// rules between controls; the feature modules imported above own the
// shortcut chips, host pairing, the mic meter and model preparation.

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
const trainingCapture = required<HTMLInputElement>("trainingCapture");
const trainingRetentionDays = required<HTMLSelectElement>("trainingRetentionDays");
const recordingModeSeg = segControl("recordingModeSeg");
const insertAtCursor = required<HTMLInputElement>("insertAtCursor");
const insertAtCursorRow = required<HTMLElement>("insertAtCursorRow");
const accessibilityInsert = required<HTMLInputElement>("accessibilityInsert");
const fnPushToTalk = required<HTMLInputElement>("fnPushToTalk");
const fnPushToTalkRow = required<HTMLElement>("fnPushToTalkRow");
const useGpu = required<HTMLInputElement>("useGpu");
const useGpuRow = required<HTMLElement>("useGpuRow");
const superModeSelect = required<HTMLSelectElement>("superModeSelect");
const superModeRow = required<HTMLElement>("superModeRow");
const superModeHelp = required<HTMLElement>("superModeHelp");
const superModeHelpLocal = superModeHelp.textContent ?? "";
const superModeModelSelect = required<HTMLSelectElement>("superModeModelSelect");
const superModeModelRow = required<HTMLElement>("superModeModelRow");
const modelDownload = required<HTMLElement>("modelDownload");
const modelField = required<HTMLElement>("modelField");
const remoteHostPanel = required<HTMLElement>("remoteHostPanel");
const remoteUrl = required<HTMLInputElement>("remoteUrl");
const remoteAuthToken = required<HTMLInputElement>("remoteAuthToken");
const remoteTimeoutSeconds = required<HTMLInputElement>("remoteTimeoutSeconds");
const cloudPanel = required<HTMLElement>("cloudPanel");
const cloudAuthToken = required<HTMLInputElement>("cloudAuthToken");
const cloudStatus = required<HTMLElement>("cloudStatus");
const cloudTest = required<HTMLButtonElement>("cloudTest");
const polishProviderSelect = required<HTMLSelectElement>("polishProviderSelect");
const polishCloudAuthToken = required<HTMLInputElement>("polishCloudAuthToken");
const contextAwarenessHelp = required<HTMLElement>("contextAwarenessHelp");
const polishEnabled = required<HTMLInputElement>("polishEnabled");
const polishHelp = required<HTMLElement>("polishHelp");
const polishTonesPanel = required<HTMLElement>("polishTonesPanel");
const contextAwareness = required<HTMLInputElement>("contextAwareness");
const polishLocalModelPath = required<HTMLInputElement>("polishLocalModelPath");
const polishLocalModelPrompt = required<HTMLSelectElement>("polishLocalModelPrompt");
const polishLocalAdapters = required<HTMLInputElement>("polishLocalAdapters");
const contextAwarenessRow = required<HTMLElement>("contextAwarenessRow");
const polishFormatPanel = required<HTMLElement>("polishFormatPanel");
const formatAiDetection = required<HTMLInputElement>("formatAiDetection");
const formatAiDetectionHelp = required<HTMLElement>("formatAiDetectionHelp");
const polishToneSegs: Record<string, SegControl> = {
  messaging: segControl("polishToneMessaging"),
  email: segControl("polishToneEmail"),
  docs: segControl("polishToneDocs"),
  code: segControl("polishToneCode"),
  other: segControl("polishToneOther"),
};

const POLISH_MODEL_NAMES: Record<string, string> = {
  "qwen3.5-0.8b": "Qwen3.5 · 0.8B",
  "qwen3.5-2b": "Qwen3.5 · 2B",
  "qwen3.5-4b": "Qwen3.5 · 4B",
  "speakoflow-mini": "SpeakoFlow Mini · 0.8B",
};

let currentSettings: Settings = { ...DEFAULTS };
// Builds without a Fairspoken Cloud endpoint hide every cloud choice.
let cloudAvailable = true;

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

function applyCloudAvailability(): void {
  const cloudLocation = document.querySelector<HTMLButtonElement>('#locationSeg button[data-value="cloud"]');
  const cloudPolish = polishProviderSelect.querySelector<HTMLOptionElement>('option[value="cloud"]');
  for (const choice of [cloudLocation, cloudPolish]) {
    if (!choice) continue;
    choice.hidden = !cloudAvailable;
    choice.disabled = !cloudAvailable;
  }
}

// Typing fields keep what the user is entering when a settings echo (or
// another window's save) re-applies the form; every other control follows.
function setField(field: HTMLInputElement | HTMLSelectElement, value: string): void {
  if (field === document.activeElement) return;
  field.value = value;
}

function applyToForm(settings: Settings): void {
  locationSeg.set(settings.transcriptionLocation);
  // A selection from the model library must remain representable even if its
  // settings event arrives before the supported-model catalog has loaded.
  if (![...modelSelect.options].some(option => option.value === settings.model)) {
    modelSelect.add(new Option(speechModelName(settings.model), settings.model));
  }
  modelSelect.value = settings.model;
  const speechName = document.getElementById("selectedSpeechModel");
  // The select already names the model; this line only adds who made it.
  if (speechName) speechName.textContent = settings.model === "parakeet-ultra" ? "Moondream" : settings.model.startsWith("parakeet") ? "NVIDIA" : "OpenAI";
  const polishName = document.getElementById("selectedPolishModel");
  if (polishName) {
    polishName.textContent = settings.polishLocalModelPath
      ? `Local file · ${settings.polishLocalModelPath.split(/[\\/]/).pop()}`
      : POLISH_MODEL_NAMES[settings.polishModel] ?? settings.polishModel;
  }
  updateModelSize(settings.model);
  setField(remoteUrl, settings.remoteUrl);
  setField(remoteAuthToken, settings.remoteAuthToken);
  setField(remoteTimeoutSeconds, String(settings.remoteTimeoutSeconds));
  setField(cloudAuthToken, settings.cloudAuthToken);
  setField(polishCloudAuthToken, settings.cloudAuthToken);
  polishProviderSelect.value = settings.polishProvider;
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
  trainingCapture.checked = settings.trainingCapture;
  trainingRetentionDays.value = String(settings.trainingRetentionDays);
  recordingModeSeg.set(settings.recordingShortcutMode);
  renderShortcutChips(settings);
  insertAtCursor.checked = settings.insertAtCursor;
  accessibilityInsert.checked = settings.accessibilityInsert;
  fnPushToTalk.checked = settings.fnPushToTalk;
  useGpu.checked = settings.useGpu;
  superModeSelect.value = settings.superMode;
  superModeModelSelect.value = settings.superModeModel;
  polishEnabled.checked = settings.polishEnabled;
  contextAwareness.checked = settings.contextAwareness;
  formatAiDetection.checked = settings.formatAiDetection;
  setField(polishLocalModelPath, settings.polishLocalModelPath);
  polishLocalModelPrompt.value = settings.polishLocalModelPrompt;
  setField(polishLocalAdapters, settings.polishLocalAdapters.join(", "));
  for (const category of POLISH_TONE_CATEGORIES) {
    polishToneSegs[category].set(settings.polishTones[category] ?? "default");
  }
  updateTranscriptionLocationUi(settings.transcriptionLocation);
  updateUseGpuUi();
  updatePolishUi(settings);
}

// Speech and polish use independent providers and share only the cloud token.
function updatePolishUi(settings: Settings): void {
  const local = settings.polishProvider === "local";
  const hasToken = local || (cloudAvailable && settings.cloudAuthToken.trim() !== "");
  polishEnabled.disabled = !hasToken;
  polishHelp.textContent = local
    ? "Removes fillers and self-corrections, on this device."
    : !cloudAvailable ? "Fairspoken Cloud is not in this build. Choose This device."
    : hasToken ? "Sends transcript text, never audio, to Fairspoken Cloud."
    : "Add a Fairspoken Cloud token below to turn this on.";
  polishCloudAuthToken.closest<HTMLElement>("[data-polish-cloud]")?.toggleAttribute("hidden", local || !cloudAvailable);
  document.getElementById("polishLocalModelRow")?.toggleAttribute("hidden", !local);
  document.getElementById("polishDeveloperPanel")?.toggleAttribute("hidden", !local);
  polishTonesPanel.hidden = !hasToken || !settings.polishEnabled;
  // The format decision reads the focused app through macOS Accessibility.
  polishFormatPanel.hidden = polishTonesPanel.hidden || !isMacOS();
  formatAiDetectionHelp.textContent = local
    ? "Asks the polish model once per site or app, on this device."
    : "Asks Fairspoken Cloud once per site or app, sending its name and title.";
  contextAwarenessHelp.textContent = local
    ? "Reads text near your cursor to spell names right. Stays on this device."
    : "Reads text near your cursor to spell names right. Sent to Fairspoken Cloud with polish.";
}

function readFromForm(): Settings {
  return normalizeSettings({
    ...currentSettings,
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
    trainingCapture: trainingCapture.checked,
    trainingRetentionDays: Number(trainingRetentionDays.value),
    recordingShortcutMode: recordingModeSeg.get() as RecordingShortcutMode,
    ...readShortcutChips(DEFAULTS),
    useGpu: useGpu.checked,
    superMode: superModeSelect.value as SuperMode,
    superModeModel: superModeModelSelect.value,
    insertAtCursor: insertAtCursor.checked,
    accessibilityInsert: accessibilityInsert.checked,
    fnPushToTalk: fnPushToTalk.checked,
    polishEnabled: polishEnabled.checked,
    polishProvider: polishProviderSelect.value === "local" ? "local" : "cloud",
    contextAwareness: contextAwareness.checked,
    formatAiDetection: formatAiDetection.checked,
    polishLocalModelPath: polishLocalModelPath.value.trim(),
    polishLocalModelPrompt: polishLocalModelPrompt.value as LocalPolishPrompt,
    polishLocalAdapters: splitPaths(polishLocalAdapters.value),
    polishTones: Object.fromEntries(
      POLISH_TONE_CATEGORIES.map((category) => [category, polishToneSegs[category].get()]),
    ),
  });
}

function showSaveStatus(message: string, error = false): void {
  const status = document.getElementById("settingsSaveStatus");
  if (status) { status.textContent = message; status.dataset.error = String(error); }
}
async function persistSettings(): Promise<boolean> {
  const nextSettings = readFromForm();
  showSaveStatus("Saving…");
  try {
    await invoke("save_settings", { settings: nextSettings });
    currentSettings = { ...nextSettings,
      recordingShortcut: currentSettings.recordingShortcut,
      transcriptStackShortcut: currentSettings.transcriptStackShortcut,
    };
    addEvent("info", "Settings saved");
    showSaveStatus("Saved.");
    return true;
  } catch (error) {
    const message = errorMessage(error);
    addEvent("error", message);
    applyToForm(currentSettings);
    showSaveStatus(`Could not save: ${message}`, true);
    return false;
  }
}

function reportAsyncError(error: unknown): void {
  addEvent("error", errorMessage(error));
}

// The GPU toggle only affects local Whisper inference: a remote host's GPU
// use is the host operator's configuration, and the Parakeet engine is
// CPU-only — showing a toggle it ignores would be a lie.
function updateUseGpuUi(): void {
  const local = locationSeg.get() === "local";
  const gpuCapableEngine = !modelSelect.value.startsWith("parakeet");
  useGpuRow.hidden = !local || !gpuCapableEngine;
  useGpu.disabled = !local || !gpuCapableEngine;
  updateSuperModeUi();
}

// Super mode pairs Parakeet with Whisper. Locally the user picks the Whisper
// model; a remote host pairs whatever its operator serves; Fairspoken Cloud
// has no super mode.
function updateSuperModeUi(): void {
  const location = locationSeg.get();
  const parakeet = modelSelect.value.startsWith("parakeet");
  const local = location === "local";
  superModeRow.hidden = location === "cloud" || (local && !parakeet);
  superModeModelRow.hidden = !local || !parakeet || superModeSelect.value === "off";
  superModeHelp.textContent = location === "remote-host"
    ? "Asks your host to add Whisper when it serves both engines and has room."
    : superModeHelpLocal;
}

function updateTranscriptionLocationUi(location: TranscriptionLocation): void {
  const local = location === "local";
  // The model choice and its download state are local-transcription concerns;
  // a remote host (or Fairspoken Cloud) serves whatever its operator runs.
  modelField.hidden = !local;
  modelDownload.hidden = !local;
  remoteHostPanel.hidden = location !== "remote-host";
  cloudPanel.hidden = location !== "cloud";
  if (location === "remote-host") showRemoteHost(currentSettings);
  if (location === "cloud") {
    cloudStatus.textContent = !cloudAvailable
      ? "Fairspoken Cloud is not in this build."
      : currentSettings.cloudAuthToken
      ? "Allowance appears after your first dictation."
      : "Paste a token to start.";
  }
}

async function loadSettings(): Promise<void> {
  const [saved, models, cloud] = await Promise.all([
    invoke<Settings>("get_settings"),
    invoke<{ model: SttModel }[]>("get_dictation_models").catch(() => []),
    invoke<boolean>("get_cloud_available").catch(() => false),
  ]);
  cloudAvailable = cloud;
  applyCloudAvailability();
  if (Array.isArray(models) && models.length) {
    modelSelect.replaceChildren(...models.map(model => new Option(speechModelName(model.model), model.model)));
  }
  currentSettings = normalizeSettings(saved);
  // Cloud is the polish default; without a cloud, offer this device instead.
  if (!cloudAvailable && !currentSettings.polishEnabled && currentSettings.polishProvider === "cloud") {
    currentSettings.polishProvider = "local";
  }
  applyToForm(currentSettings);
  await loadAudioDevices();
  if (currentSettings.transcriptionLocation === "local") {
    await requestModelStatus();
  }
}

const host: SettingsFormHost = {
  current: () => currentSettings,
  setCurrent: (next) => { currentSettings = next; },
  applyToForm,
  readFromForm,
  persist: persistSettings,
  reportError: reportAsyncError,
};
initShortcutCapture(host);
initHostPairing(host);
initMicMeter(host);
initModelPreparation({ location: () => locationSeg.get(), reportError: reportAsyncError });

locationSeg.onChange((value) => {
  updateTranscriptionLocationUi(value as TranscriptionLocation);
  updateUseGpuUi();
  void persistSettings()
    .then((saved) => {
      if (!saved) return;
      const label = value === "local" ? "local" : value === "cloud" ? "Fairspoken Cloud" : "remote host";
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

cloudTest.addEventListener("click", () => {
  void testRemoteHost(cloudStatus, cloudTest, "Fairspoken Cloud");
});

// Every other control saves the whole form on change. `before` runs first
// (mirroring the shared cloud token, refreshing dependent rows); `after` runs
// once the save settles, with whether it succeeded.
interface PersistOnChange {
  before?: () => void;
  after?: (saved: boolean) => unknown;
}
const refreshPolish = () => updatePolishUi(currentSettings);
const restartMeterIfSaved = (saved: boolean) => { if (saved) refreshMeter(); };
const PERSIST_ON_CHANGE: Array<[HTMLInputElement | HTMLSelectElement, PersistOnChange?]> = [
  [remoteUrl],
  [remoteAuthToken],
  [remoteTimeoutSeconds],
  [polishProviderSelect, { after: refreshPolish }],
  [polishCloudAuthToken, { before: () => { cloudAuthToken.value = polishCloudAuthToken.value; }, after: refreshPolish }],
  [cloudAuthToken, { before: () => { polishCloudAuthToken.value = cloudAuthToken.value; }, after: refreshPolish }],
  [polishEnabled, { after: refreshPolish }],
  [contextAwareness],
  [formatAiDetection],
  [polishLocalModelPath],
  [polishLocalModelPrompt],
  [polishLocalAdapters],
  [audioDeviceSelect, { after: restartMeterIfSaved }],
  [noiseSuppression, { after: restartMeterIfSaved }],
  [echoCancellation, { after: restartMeterIfSaved }],
  [inputGain, { after: restartMeterIfSaved }],
  [langSelect],
  [postProcess],
  [alwaysOnTop],
  // Preview through the same Rust playback path the real clicks use.
  [interactionSounds, { after: (saved) => saved && interactionSounds.checked ? invoke("preview_interaction_sound", { sound: "recording-start" }) : undefined }],
  [maxRecordingSeconds],
  [noteRetentionMinutes],
  [trainingCapture],
  [trainingRetentionDays],
  [useGpu],
  [superModeSelect, { before: updateSuperModeUi }],
  [superModeModelSelect],
  [insertAtCursor],
  [accessibilityInsert],
  [fnPushToTalk],
];
for (const [field, { before, after } = {}] of PERSIST_ON_CHANGE) {
  field.addEventListener("change", () => {
    before?.();
    void persistSettings().then((saved) => after?.(saved)).catch(reportAsyncError);
  });
}
for (const seg of [...POLISH_TONE_CATEGORIES.map((category) => polishToneSegs[category]), recordingModeSeg]) {
  seg.onChange(() => void persistSettings().catch(reportAsyncError));
}

// Both delivery integrations are macOS-only (CGEvent paste and the Fn event
// tap); hide rather than disable them elsewhere so the form stays honest.
if (!isMacOS()) {
  insertAtCursorRow.hidden = true;
  insertAtCursorRow.closest<HTMLElement>(".settings-section")?.setAttribute("hidden", "");
  fnPushToTalkRow.hidden = true;
  // Context awareness reads the macOS Accessibility tree; there is no
  // Windows/Linux implementation yet.
  contextAwarenessRow.hidden = true;
  contextAwarenessRow.closest<HTMLElement>(".settings-section")?.setAttribute("hidden", "");
}

// Settings auto-persist on change; the form must never submit/navigate, which
// in the home window would reload the whole webview.
document.getElementById("settingsForm")?.addEventListener("submit", (event) => event.preventDefault());

void loadSettings().catch((error) => {
  const message = errorMessage(error);
  addEvent("error", message);
  showSettingsLoadError(message);
});

// Model selection is also owned by the Models dashboard in this window.
// An echo of this window's own save changes nothing, so it is ignored; any
// other update re-applies the form without touching the field being typed in.
void listen<Settings>("settings-updated", (event) => {
  const next = normalizeSettings(event.payload);
  if (sameSettings(next, currentSettings)) return;
  currentSettings = next;
  applyToForm(currentSettings);
});
