import { isMacOS, normalizeShortcut } from "./shortcuts";

// The Settings shape the backend stores, its defaults, and the normalisers
// that mirror the Rust side. No DOM here: settings.ts and its feature modules
// (shortcutCapture, hostPairing, micMeter, modelPreparation) share these.

// Parakeet is the default engine; the whisper.cpp ids are only meaningful when
// the backend was built with the `whisper` Cargo feature.
export type SttModel =
  | "parakeet-tdt-0.6b-v3"
  | "parakeet-ultra"
  | "parakeet-tdt-0.6b-v2"
  | "tiny"
  | "base"
  | "small"
  | "medium"
  | "large-v2"
  | "large-v3"
  | "large-v3-turbo";
export type TranscriptionLocation = "local" | "remote-host" | "cloud";
export type RecordingShortcutMode = "toggle" | "push-to-talk";

export interface TranscriptCorrection {
  enabled: boolean;
  from: string;
  to: string;
  caseSensitive: boolean;
  wholePhrase: boolean;
}

export interface Settings {
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
  accessibilityInsert: boolean;
  fnPushToTalk: boolean;
  polishEnabled: boolean;
  polishProvider: "local" | "cloud";
  polishModel: string;
  polishTones: Record<string, string>;
  polishLocalModelPath: string;
  polishLocalModelPrompt: LocalPolishPrompt;
  polishLocalAdapters: string[];
  contextAwareness: boolean;
  trainingCapture: boolean;
  /** 30, 90, or 0 for "until deleted". */
  trainingRetentionDays: number;
  formatAiDetection: boolean;
  superMode: SuperMode;
  superModeModel: string;
  /** Download links by model id, set from the Models screen (model_link.rs). */
  modelLinks: Record<string, string>;
}

export type LocalPolishPrompt = "tagged" | "instructed" | "speakoflow";
export type SuperMode = "off" | "auto" | "on";
export const SUPER_MODE_MODELS = ["tiny", "base", "small", "large-v3-turbo"];

export interface ModelStatus {
  model: SttModel;
  cached: boolean;
  message: string;
  modelPath: string;
}

export interface RemoteHealth {
  ok: boolean;
  mode: string;
  backend: string;
  serverVersion?: string;
}

// A transcription host that answered GET /v1/hello (tailnet_discovery.rs).
export interface DiscoveredHost {
  name: string;
  machine: string;
  url: string;
  auth: "none" | "password" | "token";
  serverVersion?: string | null;
  isSelf: boolean;
}

export interface ModelPrepareProgressEvent {
  model: string;
  stage: string;
  message: string;
  percentage: number;
  done: boolean;
  error?: string | null;
  status?: ModelStatus | null;
}

export const DEFAULTS: Settings = {
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
  accessibilityInsert: false,
  fnPushToTalk: false,
  polishEnabled: false,
  polishProvider: "cloud",
  polishModel: "speakoflow-mini",
  polishTones: {},
  polishLocalModelPath: "",
  polishLocalModelPrompt: "tagged",
  polishLocalAdapters: [],
  contextAwareness: false,
  trainingCapture: false,
  trainingRetentionDays: 30,
  formatAiDetection: false,
  superMode: "off",
  superModeModel: "small",
  modelLinks: {},
};

// Category ids match the polish endpoint contract and the Rust setting keys.
export const POLISH_TONE_CATEGORIES = ["messaging", "email", "docs", "code", "other"] as const;

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

export function normalizeSettings(settings: Partial<Settings>): Settings {
  const seconds = Number(settings.maxRecordingSeconds ?? DEFAULTS.maxRecordingSeconds);
  const retentionMinutes = Number(settings.noteRetentionMinutes ?? DEFAULTS.noteRetentionMinutes);
  return {
    ...DEFAULTS,
    ...settings,
    inputGain: Math.max(1, Math.min(6, Number(settings.inputGain ?? DEFAULTS.inputGain))),
    maxRecordingSeconds: snapToChoice(seconds, MAX_RECORDING_CHOICES, DEFAULTS.maxRecordingSeconds),
    noteRetentionMinutes: snapToChoice(retentionMinutes, NOTE_RETENTION_CHOICES, DEFAULTS.noteRetentionMinutes),
    remoteUrl: (settings.remoteUrl ?? "").trim().replace(/\/+$/, ""),
    remoteTimeoutSeconds: Math.max(5, Math.min(300, Math.round(Number(settings.remoteTimeoutSeconds ?? DEFAULTS.remoteTimeoutSeconds)))),
    cloudAuthToken: (settings.cloudAuthToken ?? "").trim(),
    polishEnabled: settings.polishEnabled ?? DEFAULTS.polishEnabled,
    polishTones: normalizePolishTones(settings.polishTones ?? DEFAULTS.polishTones),
    contextAwareness: settings.contextAwareness ?? DEFAULTS.contextAwareness,
    trainingCapture: settings.trainingCapture ?? DEFAULTS.trainingCapture,
    trainingRetentionDays: [0, 30, 90].includes(Number(settings.trainingRetentionDays))
      ? Number(settings.trainingRetentionDays)
      : DEFAULTS.trainingRetentionDays,
    formatAiDetection: settings.formatAiDetection ?? DEFAULTS.formatAiDetection,
    polishLocalModelPath: (settings.polishLocalModelPath ?? "").trim(),
    polishLocalModelPrompt: settings.polishLocalModelPrompt === "instructed" || settings.polishLocalModelPrompt === "speakoflow"
      ? settings.polishLocalModelPrompt
      : "tagged",
    polishLocalAdapters: splitPaths((settings.polishLocalAdapters ?? []).join(",")),
    vocabularyHints: normalizeVocabularyHints(settings.vocabularyHints ?? DEFAULTS.vocabularyHints),
    transcriptCorrections: normalizeTranscriptCorrections(settings.transcriptCorrections ?? DEFAULTS.transcriptCorrections),
    recordingShortcut: normalizeShortcut(settings.recordingShortcut ?? DEFAULTS.recordingShortcut, DEFAULTS.recordingShortcut),
    recordingShortcutMode: settings.recordingShortcutMode === "push-to-talk" ? "push-to-talk" : "toggle",
    transcriptStackShortcut: normalizeShortcut(settings.transcriptStackShortcut ?? DEFAULTS.transcriptStackShortcut, DEFAULTS.transcriptStackShortcut),
    insertAtCursor: settings.insertAtCursor ?? DEFAULTS.insertAtCursor,
    accessibilityInsert: settings.accessibilityInsert ?? DEFAULTS.accessibilityInsert,
    fnPushToTalk: settings.fnPushToTalk ?? DEFAULTS.fnPushToTalk,
    useGpu: settings.useGpu ?? DEFAULTS.useGpu,
    superMode: settings.superMode === "auto" || settings.superMode === "on" ? settings.superMode : "off",
    superModeModel: settings.superModeModel && SUPER_MODE_MODELS.includes(settings.superModeModel)
      ? settings.superModeModel
      : DEFAULTS.superModeModel,
    modelLinks: normalizeModelLinks(settings.modelLinks ?? DEFAULTS.modelLinks),
  };
}

// Mirror of the Rust normalization: trimmed ids and links, empty ones dropped.
export function normalizeModelLinks(links: Record<string, unknown>): Record<string, string> {
  const normalized: Record<string, string> = {};
  if (!links || typeof links !== "object" || Array.isArray(links)) return normalized;
  for (const [model, link] of Object.entries(links)) {
    if (typeof link !== "string") continue;
    const id = model.trim(), value = link.replace(/\0/g, "").trim();
    if (id && value) normalized[id] = value;
  }
  return normalized;
}

// Mirror of the Rust normalization: only known categories and tones survive,
// and redundant "default" entries are dropped.
export function normalizePolishTones(tones: Record<string, string>): Record<string, string> {
  const normalized: Record<string, string> = {};
  for (const category of POLISH_TONE_CATEGORIES) {
    const tone = tones[category];
    if (tone === "casual" || tone === "formal" || tone === "off") {
      normalized[category] = tone;
    }
  }
  return normalized;
}

export function splitPaths(value: string): string[] {
  const paths = value.split(",").map((path) => path.trim()).filter(Boolean);
  return [...new Set(paths)];
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

// Equality that ignores key order, for telling an echo of a save apart from
// a real change.
export function sameSettings(a: Settings, b: Settings): boolean {
  return stableJson(a) === stableJson(b);
}

function stableJson(value: unknown): string {
  return JSON.stringify(value, (_key, item) =>
    item && typeof item === "object" && !Array.isArray(item)
      ? Object.fromEntries(Object.keys(item).sort().map((key) => [key, item[key]]))
      : item);
}

// What the feature modules need from the Settings form that owns the saved
// snapshot and the controls.
export interface SettingsFormHost {
  current(): Settings;
  setCurrent(next: Settings): void;
  applyToForm(settings: Settings): void;
  readFromForm(): Settings;
  persist(): Promise<boolean>;
  reportError(error: unknown): void;
}
