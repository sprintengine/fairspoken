// Minimal Tauri v2 IPC stub so the REAL frontend (home.html, index.html,
// cursor-preview.html served by Vite) renders in a plain browser for the
// README screenshots. render.mjs injects it into every frame before any app
// script runs (Playwright addInitScript). Unknown commands reject, which the
// app already treats as "no data yet".
//
// window.__fsEmit(event, payload) fires a backend event into the page.
(() => {
  const settings = {
    transcriptionLocation: "local", model: "parakeet-tdt-0.6b-v3",
    remoteUrl: "https://mac-mini.tail1234.ts.net", remoteAuthToken: "practice-token", remoteTimeoutSeconds: 60,
    cloudAuthToken: "", language: "en", alwaysOnTop: true, interactionSounds: true, maxRecordingSeconds: 120,
    noteRetentionMinutes: 0, useGpu: true, audioDevice: "", noiseSuppression: true, echoCancellation: true, inputGain: 2,
    postProcess: true, vocabularyHints: [], transcriptCorrections: [], recordingShortcut: "CommandOrControl+Shift+Digit1",
    recordingShortcutMode: "push-to-talk", transcriptStackShortcut: "CommandOrControl+Shift+Digit2", insertAtCursor: true,
    accessibilityInsert: false, fnPushToTalk: true, polishEnabled: true, polishProvider: "local", polishModel: "qwen3.5-2b",
    polishTones: {}, contextAwareness: true,
  };
  const speech = ["parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v2", "small", "medium", "large-v3", "large-v3-turbo"]
    .map((model) => ({ model, cached: model.startsWith("parakeet") || model === "large-v3-turbo" }));
  const polish = [
    { id: "qwen3.5-0.8b", name: "Qwen3.5 0.8B", publisher: "Qwen", description: "Fastest cleanup", bytes: 560e6, installed: true, selected: false, loaded: false, source: "https://huggingface.co/Qwen/Qwen3.5-0.8B", downloads: null, supported: true },
    { id: "qwen3.5-2b", name: "Qwen3.5 2B", publisher: "Qwen", description: "Balanced cleanup", bytes: 1.28e9, installed: true, selected: true, loaded: true, source: "https://huggingface.co/Qwen/Qwen3.5-2B", downloads: null, supported: true },
  ];
  const handlers = {
    get_settings: () => settings,
    get_cloud_available: () => true,
    save_settings: () => null,
    get_dictation_models: () => speech,
    get_transcription_model_status: () => ({ model: settings.model, cached: true, message: "Model is ready", modelPath: "~/Library/Application Support/models/parakeet-tdt-0.6b-v3" }),
    get_local_model_catalog: () => ({ polish, download: null, metadataError: null }),
    get_notes: () => [],
    get_transcript_history: () => [],
    get_cursor_preview_state: () => ({ sessionId: 0, revision: 0, phase: "idle", text: "", polished: false, remote: false, polishing: false }),
  };

  const listeners = new Map();
  let next = 1;
  window.__fsEmit = (event, payload) => {
    for (const id of listeners.get(event) || []) window[`_${id}`]?.({ event, id: 0, payload });
  };
  window.__TAURI_INTERNALS__ = {
    invoke: async (cmd, args = {}) => {
      if (cmd === "plugin:event|listen") {
        listeners.set(args.event, [...(listeners.get(args.event) || []), args.handler]);
        return next++;
      }
      if (cmd.startsWith("plugin:")) return null;
      if (cmd in handlers) return structuredClone(handlers[cmd](args));
      throw new Error(`screenshot stub: ${cmd} not available`);
    },
    transformCallback: (fn, once) => {
      const id = next++;
      window[`_${id}`] = (payload) => { if (once) delete window[`_${id}`]; return fn?.(payload); };
      return id;
    },
    unregisterCallback: (id) => { delete window[`_${id}`]; },
    convertFileSrc: (path) => path,
    metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
})();
