import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
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
let reloadRequested = false;
const variants = new Map<string, string>();
const speechProgress = new Map<string, { percentage: number; message: string; done: boolean }>();
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
  const element = document.createElement("div"); element.className = "ds-list-row model-row"; element.id = `model-${id}`;
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
  const focusedId = root.contains(document.activeElement) ? (document.activeElement as HTMLElement).id : "";
  rows.clear();
  root.querySelector("#dictationProvider")!.textContent = settings.transcriptionLocation === "local" ? "Available speech models for this device." : `Currently using ${settings.transcriptionLocation === "cloud" ? "MultiVoice Cloud" : "your remote host"}. Choose Use to switch to local dictation.`;
  const speechList = root.querySelector("#dictationModels")!; speechList.replaceChildren();
  for (const family of ["parakeet", "whisper"]) {
    const options = speech.filter(m => m.model.startsWith("parakeet") === (family === "parakeet"));
    if (!options.length) continue;
    const preferred = variants.get(family) ?? options.find(m => m.model === settings.model)?.model ?? options[0].model;
    const m = options.find(m => m.model === preferred) ?? options[0];
    variants.set(family, m.model);
    const parakeet = family === "parakeet";
    const selected = settings.transcriptionLocation === "local" && settings.model === m.model;
    const r = row(m.model, parakeet ? "Parakeet" : "Whisper", parakeet ? "NVIDIA" : "OpenAI", parakeet ? "NVIDIA · Recommended · 25 languages" : "OpenAI · Multilingual speech recognition", selected && m.cached ? "Selected" : m.cached ? "Downloaded" : "Available to download");
    r.element.classList.add("model-family"); r.element.id = `family-${family}`;
    const label = document.createElement("label"); label.className = "model-variant"; label.textContent = "Variant";
    const select = document.createElement("select"); select.id = `variant-${family}`; select.setAttribute("aria-label", `${parakeet ? "Parakeet" : "Whisper"} variant`);
    for (const option of options) {
      const node = document.createElement("option"); node.value = option.model;
      node.textContent = `${parakeet ? (option.model.endsWith("-v2") ? "0.6B v2 · English" : "0.6B v3 · Recommended") : option.model}${option.cached ? " · Downloaded" : ""}${settings.model === option.model && settings.transcriptionLocation === "local" ? " · Selected" : ""}`;
      select.append(node);
    }
    select.value = m.model;
    select.addEventListener("change", () => { variants.set(family, select.value); render(); root.querySelector<HTMLSelectElement>(`#variant-${family}`)?.focus(); });
    label.append(select); r.element.querySelector(".ds-list-row-content")!.append(label);
    r.actions.append(button(m.cached ? (selected ? "Selected" : "Use") : speechDownload === m.model ? "Downloading…" : "Download", async () => {
      if (m.cached) return choose({ model: m.model, transcriptionLocation: "local" });
      speechDownload = m.model; render();
      try { await invoke("begin_prepare_transcription_model", { request: { model: m.model } }); }
      catch (e) { speechDownload = null; render(); throw e; }
    }, (selected && m.cached) || speechDownload !== null));
    const progress = speechProgress.get(m.model); const current = rows.get(m.model)!;
    if (progress && !progress.done) { current.status.textContent = progress.message; current.progress.hidden = false; current.progress.value = progress.percentage; }
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
  if (focusedId) document.getElementById(focusedId)?.focus({ preventScroll: true });
}
async function load(refresh = false): Promise<void> {
  if (loading) { reloadRequested = true; return; }
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
  } finally {
    loading = false;
    if (reloadRequested) { reloadRequested = false; void load().catch(e => say(String(e), true)); }
  }
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
  const e = event.payload; speechProgress.set(e.model, e); if (!e.done) speechDownload = e.model; const row = rows.get(e.model);
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

// Hub discovery is separate from our deliberately small, tested runtime catalog.
type HubModel = { id: string; downloads: number; likes: number; pipelineTag: string | null; libraryName: string | null; languages: string[]; license: string | null; gated: boolean; source: string };
const searchInput = root.querySelector<HTMLInputElement>("#modelSearch")!;
const searchStatus = root.querySelector<HTMLElement>("#modelSearchStatus")!;
const searchResults = root.querySelector<HTMLElement>("#modelSearchResults")!;
let searchRevision = 0;
let searchTimer: ReturnType<typeof setTimeout> | undefined;
function supportedFamily(id: string): string | null {
  if (id === "istupakov/parakeet-tdt-0.6b-v3-onnx") return "parakeet";
  if (id === "ggerganov/whisper.cpp" && speech.some(m => !m.model.startsWith("parakeet"))) return "whisper";
  return null;
}
async function searchHub(query: string, revision: number): Promise<void> {
  searchStatus.textContent = "Searching Hugging Face…"; searchResults.replaceChildren(); searchResults.setAttribute("aria-busy", "true");
  try {
    const result = await invoke<{ models: HubModel[]; cached: boolean }>("search_hugging_face_models", { query });
    if (revision !== searchRevision) return;
    searchStatus.textContent = result.models.length ? `${result.models.length} results${result.cached ? " · Cached" : ""}. Only verified runtime formats can be downloaded in MultiVoice.` : "No models found. Try a different name or publisher.";
    for (const model of result.models) {
      if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(model.id)) continue;
      const item = document.createElement("div"); item.className = "model-search-result";
      const content = document.createElement("div");
      const title = document.createElement("strong"); title.textContent = model.id;
      const detail = document.createElement("p"); detail.className = "model-status";
      detail.textContent = [model.pipelineTag, model.libraryName, `${(model.downloads ?? 0).toLocaleString()} downloads`, model.license, model.gated ? "Access approval required" : null].filter(Boolean).join(" · ");
      const compatibility = document.createElement("p"); compatibility.className = "model-status";
      const family = supportedFamily(model.id);
      const polish = catalog?.polish.find(m => m.supported && m.source === `https://huggingface.co/${model.id}`);
      compatibility.textContent = family || polish ? "Compatible variants available above" : "Not supported yet";
      content.append(title, detail, compatibility);
      const actions = document.createElement("div"); actions.className = "models-actions";
      if (family || polish) actions.append(button(family ? "Choose variant" : "Show model", async () => { const target = family ? document.getElementById(`variant-${family}`) : document.getElementById(`model-${polish!.id}`)?.querySelector("button"); target?.scrollIntoView({ block: "center", behavior: "smooth" }); target?.focus({ preventScroll: true }); }));
      actions.append(button("Model page ↗", () => openUrl(`https://huggingface.co/${model.id}`)));
      item.append(content, actions); searchResults.append(item);
    }
  } catch (error) { if (revision === searchRevision) searchStatus.textContent = `Search unavailable: ${String(error)}. Edit your search to try again.`; }
  finally { if (revision === searchRevision) searchResults.setAttribute("aria-busy", "false"); }
}
searchInput.addEventListener("input", () => {
  const revision = ++searchRevision; clearTimeout(searchTimer); const query = searchInput.value.trim();
  searchResults.replaceChildren(); searchResults.setAttribute("aria-busy", "false");
  searchStatus.textContent = query ? "Waiting to search…" : "Search model names or publishers on Hugging Face.";
  if (query) searchTimer = setTimeout(() => { void searchHub(query, revision); }, 350);
});
