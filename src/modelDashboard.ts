import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./modelDashboard.css";
import { createPublisherIcon } from "./publisherIcons";

type Settings = { model: string; transcriptionLocation: string; polishEnabled: boolean; polishProvider: "local" | "cloud"; polishModel: string; cloudAuthToken: string; [key: string]: unknown };
type Model = { id: string; name: string; publisher: string; description: string; bytes: number; installed: boolean; selected: boolean; loaded: boolean; source: string; downloads: number | null; supported: boolean };
type Download = { model: string; stage: string; downloaded: number; total: number; message: string };
type Catalog = { polish: Model[]; download: Download | null; metadataError: string | null };
type SpeechModel = { model: string; cached: boolean };
const root = document.querySelector<HTMLElement>("#screen-models")!;
const feedback = root.querySelector<HTMLElement>("#modelFeedback")!;
let settings: Settings;
let catalog: Catalog;
let speech: SpeechModel[] = [];
let download: Download | null = null;
let speechDownload: string | null = null;
let loading = false;
const rows = new Map<string, { status: HTMLElement; progress: HTMLProgressElement }>();
function say(message: string, error = false): void { feedback.textContent = message; feedback.dataset.error = String(error); }
function button(label: string, action: () => Promise<unknown>, disabled = false): HTMLButtonElement {
  const node = document.createElement("button"); node.type = "button"; node.className = "ds-button ds-button--outline";
  node.textContent = label; node.disabled = disabled;
  node.addEventListener("click", () => { node.disabled = true; void action().catch(e => say(String(e), true)).finally(() => { if (node.isConnected) node.disabled = disabled; }); });
  return node;
}
async function choose(patch: Partial<Settings>): Promise<void> {
  // Read at action time: don't overwrite newer settings from another screen.
  const latest = await invoke<Settings>("get_settings");
  await invoke("save_settings", { settings: { ...latest, ...patch } });
  await load();
}
const size = (bytes: number): string => bytes >= 1e9 ? `${(bytes / 1e9).toFixed(2)} GB` : `${Math.round(bytes / 1e6)} MB`;
function row(id: string, title: string, publisher: string, detail: string, status: string): { element: HTMLElement; actions: HTMLElement } {
  const element = document.createElement("div"); element.className = "ds-list-row model-row";
  const icon = createPublisherIcon(publisher);
  const content = document.createElement("div"); content.className = "ds-list-row-content";
  const name = document.createElement("div"); name.className = "ds-list-row-title"; name.textContent = title;
  const description = document.createElement("span"); description.className = "ds-list-row-supporting"; description.textContent = detail;
  const state = document.createElement("span"); state.className = "model-status"; state.textContent = status;
  const progress = document.createElement("progress"); progress.hidden = true; progress.max = 100; progress.setAttribute("aria-label", `${title} download`);
  const actions = document.createElement("div"); actions.className = "models-actions";
  content.append(name, description, state, progress); element.append(icon, content, actions); rows.set(id, { status: state, progress });
  return { element, actions };
}
function busyDownload(): boolean { return !!download && ["runtime", "model"].includes(download.stage); }
function progressUpdate(value: Download): void {
  download = value;
  const current = rows.get(value.model);
  if (current) {
    current.status.textContent = value.message + (value.total ? ` · ${size(value.downloaded)} / ${size(value.total)}` : "");
    current.progress.hidden = !busyDownload();
    current.progress.value = value.total ? (value.downloaded / value.total) * 100 : 0;
  }
}
function render(): void {
  rows.clear();
  root.querySelector("#dictationProvider")!.textContent = settings.transcriptionLocation === "local" ? "Available speech models for this device." : `Currently using ${settings.transcriptionLocation === "cloud" ? "MultiVoice Cloud" : "your remote host"}. Choose Use to switch to local dictation.`;
  const speechList = root.querySelector("#dictationModels")!; speechList.replaceChildren();
  for (const m of speech) {
    const parakeet = m.model.startsWith("parakeet");
    const selected = settings.transcriptionLocation === "local" && settings.model === m.model;
    const r = row(m.model, parakeet ? "Parakeet TDT 0.6B v3" : `Whisper ${m.model}`, parakeet ? "NVIDIA" : "OpenAI", parakeet ? "NVIDIA · Multilingual speech recognition · ONNX" : "OpenAI · Speech recognition · whisper.cpp", selected ? "Selected" : m.cached ? "Downloaded" : "Available to download");
    r.actions.append(button(m.cached ? (selected ? "Selected" : "Use") : "Download", async () => {
      if (m.cached) return choose({ model: m.model, transcriptionLocation: "local" });
      speechDownload = m.model; render();
      try { await invoke("begin_prepare_transcription_model", { request: { model: m.model } }); }
      catch (e) { speechDownload = null; render(); throw e; }
    }, selected || speechDownload !== null));
    speechList.append(r.element);
  }
  root.querySelector("#polishProvider")!.textContent = !settings.polishEnabled ? "Polish is off. Dictionary corrections still apply." : settings.polishProvider === "local" ? "Local cleanup stays on this device." : "Cloud cleanup sends text to your configured service.";
  const providerActions = root.querySelector("#polishProviderActions")!;
  providerActions.replaceChildren(button("Off", () => choose({ polishEnabled: false }), !settings.polishEnabled), button("Use cloud", () => choose({ polishProvider: "cloud", polishEnabled: true }), !settings.cloudAuthToken || (settings.polishEnabled && settings.polishProvider === "cloud")));
  const polishList = root.querySelector("#polishModels")!; polishList.replaceChildren();
  for (const m of catalog.polish) {
    const r = row(m.id, m.name, m.publisher, `${m.publisher} · ${m.description} · ${size(m.bytes)} download${m.downloads !== null ? ` · ${m.downloads.toLocaleString()} Hub downloads` : ""}`, !m.supported ? "Unavailable on this platform" : m.selected && m.installed ? (m.loaded ? "Selected · loaded in memory" : "Selected · loads when needed") : m.installed ? "Downloaded" : "Available to download");
    if (busyDownload() && download?.model === m.id) {
      r.actions.append(button("Cancel download", async () => { await invoke("cancel_local_model_download"); say("Cancelling download…"); }));
    } else {
      r.actions.append(button(m.installed ? (m.selected ? "Selected" : "Use") : "Download", async () => {
        if (m.installed) return choose({ polishProvider: "local", polishModel: m.id, polishEnabled: true });
        download = { model: m.id, stage: "runtime", downloaded: 0, total: 0, message: "Starting download…" }; render();
        try { await invoke("download_local_model", { model: m.id }); say(`${m.name} downloaded. Choose Use to enable it.`); }
        finally { await load(); }
      }, !m.supported || (m.selected && m.installed) || busyDownload()));
      if (m.installed) r.actions.append(button("Remove", async () => { await invoke("remove_local_model", { model: m.id }); await load(); }, busyDownload()));
    }
    polishList.append(r.element);
  }
  if (download) progressUpdate(download);
}
async function load(refresh = false): Promise<void> {
  if (loading) return;
  loading = true;
  try {
    const values = await Promise.all([invoke<Settings>("get_settings"), invoke<Catalog>("get_local_model_catalog", { refresh }), invoke<SpeechModel[]>("get_dictation_models")]);
    const previous = catalog;
    [settings, catalog, speech] = values;
    if (!refresh && previous) {
      for (const model of catalog.polish) model.downloads = previous.polish.find(m => m.id === model.id)?.downloads ?? model.downloads;
    }
    download = catalog.download; render();
    if (refresh) say(catalog.metadataError ?? "Compatible model information refreshed from Hugging Face.", !!catalog.metadataError);
  } finally { loading = false; }
}
root.querySelector<HTMLButtonElement>("#refreshModels")!.addEventListener("click", event => {
  const button = event.currentTarget as HTMLButtonElement; button.disabled = true;
  void load(true).catch(e => say(String(e), true)).finally(() => { button.disabled = false; });
});
void listen<Download>("local-model-download", event => {
  const wasBusy = busyDownload(); progressUpdate(event.payload);
  if (catalog && wasBusy !== busyDownload()) render();
  if (["error", "cancelled"].includes(event.payload.stage)) say(event.payload.message, event.payload.stage === "error");
}).catch(e => say(String(e), true));
void listen<{ model: string; percentage: number; message: string; done: boolean; error?: string }>("model-prepare-progress", event => {
  const e = event.payload; const row = rows.get(e.model);
  if (row) { row.status.textContent = e.message; row.progress.hidden = e.done; row.progress.value = e.percentage; }
  if (e.done) { speechDownload = null; if (e.error) say(e.error, true); void load().catch(e => say(String(e), true)); }
}).catch(e => say(String(e), true));
void listen<Settings>("settings-updated", event => { settings = event.payload; void load().catch(e => say(String(e), true)); }).catch(e => say(String(e), true));
// Only refresh memory state while the model screen is visible; don't rebuild focused controls.
window.setInterval(() => {
  if (!catalog || !root.classList.contains("active") || root.contains(document.activeElement) || busyDownload()) return;
  void load().catch(() => {});
}, 5000);
void load().catch(e => say(String(e), true));
