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
// Parakeet variants share one row; each has its own picker label, publisher and summary.
const PARAKEET_VARIANTS: Record<string, { label: string; publisher: string; detail: string }> = {
  "parakeet-tdt-0.6b-v3": { label: "0.6B v3 · Recommended", publisher: "NVIDIA", detail: "NVIDIA · Recommended · 25 languages" },
  "parakeet-ultra": { label: "Ultra 0.6B · Moondream", publisher: "Moondream", detail: "Moondream · Post-trained v3, lower error rates · 25 languages · 2.60 GB download" },
  "parakeet-tdt-0.6b-v2": { label: "0.6B v2 · English", publisher: "NVIDIA", detail: "NVIDIA · English · 2.51 GB download" },
};
const parakeetVariant = (model: string) => PARAKEET_VARIANTS[model] ?? PARAKEET_VARIANTS["parakeet-tdt-0.6b-v3"];
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
function chevron(): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "ds-select-chevron");
  svg.setAttribute("viewBox", "0 0 10 10");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(svg.namespaceURI, "path");
  path.setAttribute("d", "M2 4l3 3 3-3");
  path.setAttribute("stroke", "currentColor");
  path.setAttribute("stroke-width", "1.4");
  path.setAttribute("fill", "none");
  path.setAttribute("stroke-linecap", "round");
  path.setAttribute("stroke-linejoin", "round");
  svg.append(path);
  return svg;
}
function checkMark(): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "ds-select-check");
  svg.setAttribute("viewBox", "0 0 10 10");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(svg.namespaceURI, "path");
  path.setAttribute("d", "M2 5.2l2 2 4-4");
  path.setAttribute("stroke", "currentColor");
  path.setAttribute("stroke-width", "1.6");
  path.setAttribute("fill", "none");
  path.setAttribute("stroke-linecap", "round");
  path.setAttribute("stroke-linejoin", "round");
  svg.append(path);
  return svg;
}
function variantPicker(family: string, name: string, choices: { value: string; label: string }[], value: string): HTMLElement {
  const field = document.createElement("div"); field.className = "model-variant";
  const caption = document.createElement("span"); caption.textContent = "Variant";
  const listId = `variant-list-${family}`;
  const trigger = document.createElement("button");
  trigger.type = "button"; trigger.className = "ds-select"; trigger.id = `variant-${family}`;
  trigger.dataset.value = value;
  trigger.setAttribute("role", "combobox");
  trigger.setAttribute("aria-haspopup", "listbox");
  trigger.setAttribute("aria-expanded", "false");
  trigger.setAttribute("aria-controls", listId);
  trigger.setAttribute("aria-label", `${name} variant`);
  const shown = document.createElement("span"); shown.className = "ds-select-value";
  shown.textContent = choices.find(choice => choice.value === value)?.label ?? value;
  trigger.append(shown, chevron());
  const list = document.createElement("ul");
  list.className = "ds-select-listbox"; list.id = listId; list.hidden = true;
  list.setAttribute("role", "listbox"); list.setAttribute("aria-label", `${name} variant`);
  let active = Math.max(0, choices.findIndex(choice => choice.value === value));
  const paint = () => {
    [...list.children].forEach((node, index) => {
      const option = node as HTMLElement;
      option.classList.toggle("ds-select-option--active", index === active);
      option.setAttribute("aria-selected", String(choices[index].value === trigger.dataset.value));
      option.querySelector<HTMLElement>(".ds-select-check")!.hidden = choices[index].value !== trigger.dataset.value;
    });
    const current = list.children[active] as HTMLElement | undefined;
    if (current) { trigger.setAttribute("aria-activedescendant", current.id); current.scrollIntoView({ block: "nearest" }); }
  };
  const place = () => {
    const box = trigger.getBoundingClientRect();
    list.style.setProperty("--ds-select-top", `${box.bottom + 4}px`);
    list.style.setProperty("--ds-select-left", `${box.left}px`);
    list.style.setProperty("--ds-select-trigger-width", `${box.width}px`);
  };
  const close = () => { list.hidden = true; trigger.setAttribute("aria-expanded", "false"); };
  const open = () => {
    active = Math.max(0, choices.findIndex(choice => choice.value === trigger.dataset.value));
    list.hidden = false; trigger.setAttribute("aria-expanded", "true"); place(); paint();
  };
  const choose = (next: string) => {
    close();
    if (next === trigger.dataset.value) return;
    variants.set(family, next);
    render();
    root.querySelector<HTMLButtonElement>(`#variant-${family}`)?.focus();
  };
  choices.forEach((choice, index) => {
    const option = document.createElement("li");
    option.className = "ds-select-option"; option.id = `${listId}-${index}`;
    option.setAttribute("role", "option");
    const label = document.createElement("span"); label.className = "ds-select-option-label"; label.textContent = choice.label;
    option.append(label, checkMark());
    option.addEventListener("pointerenter", () => { active = index; paint(); });
    option.addEventListener("mousedown", event => event.preventDefault());
    option.addEventListener("click", () => choose(choice.value));
    list.append(option);
  });
  trigger.addEventListener("click", () => { if (list.hidden) open(); else close(); });
  trigger.addEventListener("keydown", event => {
    if (["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key) && list.hidden) { event.preventDefault(); open(); return; }
    if (list.hidden) return;
    if (event.key === "Escape") { event.preventDefault(); close(); return; }
    if (event.key === "ArrowDown") { event.preventDefault(); active = (active + 1) % choices.length; paint(); }
    if (event.key === "ArrowUp") { event.preventDefault(); active = (active + choices.length - 1) % choices.length; paint(); }
    if (event.key === "Home") { event.preventDefault(); active = 0; paint(); }
    if (event.key === "End") { event.preventDefault(); active = choices.length - 1; paint(); }
    if (event.key === "Enter" || event.key === " ") { event.preventDefault(); choose(choices[active].value); }
  });
  field.append(caption, trigger, list);
  paint();
  return field;
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
  root.querySelector("#dictationProvider")!.textContent = settings.transcriptionLocation === "local" ? "Available speech models for this device." : `Currently using ${settings.transcriptionLocation === "cloud" ? "Fairspoken Cloud" : "your remote host"}. Choose Use to switch to local dictation.`;
  const speechList = root.querySelector("#dictationModels")!; speechList.replaceChildren();
  for (const family of ["parakeet", "whisper"]) {
    const options = speech.filter(m => m.model.startsWith("parakeet") === (family === "parakeet"));
    if (!options.length) continue;
    const preferred = variants.get(family) ?? options.find(m => m.model === settings.model)?.model ?? options[0].model;
    const m = options.find(m => m.model === preferred) ?? options[0];
    variants.set(family, m.model);
    const parakeet = family === "parakeet";
    const selected = settings.transcriptionLocation === "local" && settings.model === m.model;
    const r = row(m.model, parakeet ? "Parakeet" : "Whisper", parakeet ? parakeetVariant(m.model).publisher : "OpenAI", parakeet ? parakeetVariant(m.model).detail : "OpenAI · Multilingual speech recognition", selected && m.cached ? "Selected" : m.cached ? "Downloaded" : "Available to download");
    r.element.id = `family-${family}`;
    const picker = variantPicker(family, parakeet ? "Parakeet" : "Whisper", options.map(option => ({
      value: option.model,
      label: `${parakeet ? parakeetVariant(option.model).label : option.model}${option.cached ? " · Downloaded" : ""}${settings.model === option.model && settings.transcriptionLocation === "local" ? " · Selected" : ""}`,
    })), m.model);
    r.element.querySelector(".ds-list-row-content")!.append(picker);
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

type HubModel = { id: string; author?: string; name?: string; downloads: number; likes: number; pipelineTag: string | null; libraryName: string | null; languages: string[]; license: string | null; description?: string | null; gated: boolean; source: string };
type SearchHit = { key: string; title: string; publisher: string; detail: string; family?: string; model?: string; polishId?: string; repo?: string };
const searchInput = root.querySelector<HTMLInputElement>("#modelSearch")!;
const searchBox = root.querySelector<HTMLElement>("#modelSearchBox")!;
const searchClear = root.querySelector<HTMLButtonElement>("#modelSearchClear")!;
const searchStatus = root.querySelector<HTMLElement>("#modelSearchStatus")!;
const searchListbox = root.querySelector<HTMLElement>("#modelSearchListbox")!;
const searchResults = root.querySelector<HTMLElement>("#modelSearchResults")!;
let searchRevision = 0;
let searchTimer: ReturnType<typeof setTimeout> | undefined;
let searchHits: SearchHit[] = [];
let activeHit = 0;
let popoverOpen = false;
function supportedVariant(id: string): { family: string; model?: string } | null {
  const parakeet = /^(?:nvidia\/parakeet-tdt-0\.6b-(v[23])|istupakov\/parakeet-tdt-0\.6b-(v[23])-onnx)$/.exec(id);
  if (parakeet) {
    const model = `parakeet-tdt-0.6b-${parakeet[1] ?? parakeet[2]}`;
    if (speech.some(m => m.model === model)) return { family: "parakeet", model };
  }
  if (id === "moondream/parakeet-ultra" && speech.some(m => m.model === "parakeet-ultra")) return { family: "parakeet", model: "parakeet-ultra" };
  const whisper = /^openai\/whisper-(tiny|base|small|medium|large-v2|large-v3|large-v3-turbo)$/.exec(id);
  if (whisper && speech.some(m => m.model === whisper[1])) return { family: "whisper", model: whisper[1] };
  if (id === "ggerganov/whisper.cpp" && speech.some(m => !m.model.startsWith("parakeet"))) return { family: "whisper" };
  return null;
}
function polishFor(id: string): Model | undefined {
  return catalog?.polish.find(m => m.supported && m.source === `https://huggingface.co/${id}`);
}
function compatibleHit(model: HubModel): SearchHit | null {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(model.id)) return null;
  const match = supportedVariant(model.id);
  const polish = polishFor(model.id);
  if (!match && !polish) return null;
  const publisher = model.author || model.id.split("/")[0];
  const title = model.name || model.id.split("/")[1];
  const detail = [model.libraryName, model.languages.slice(0, 3).join(", "), `${(model.downloads ?? 0).toLocaleString()} downloads`, model.license, model.gated ? "Access approval required" : null].filter(Boolean).join(" · ");
  return { key: `hub-${model.id}`, title: `${publisher}/${title}`, publisher, detail, family: match?.family, model: match?.model, polishId: polish?.id, repo: model.id };
}
function localHits(query: string): SearchHit[] {
  const needle = query.toLowerCase();
  const hits: SearchHit[] = [];
  for (const family of ["parakeet", "whisper"] as const) {
    const options = speech.filter(m => m.model.startsWith("parakeet") === (family === "parakeet"));
    if (!options.length) continue;
    const title = family === "parakeet" ? "Parakeet" : "Whisper";
    const publisher = family === "parakeet" ? "NVIDIA" : "OpenAI";
    if (![title, publisher, family, ...options.map(m => m.model), ...options.map(m => PARAKEET_VARIANTS[m.model]?.publisher ?? "")].join(" ").toLowerCase().includes(needle)) continue;
    hits.push({ key: `local-${family}`, title, publisher, detail: family === "parakeet" ? "On-device dictation" : "whisper.cpp dictation", family, model: variants.get(family) ?? options.find(m => m.model === settings?.model)?.model ?? options[0].model });
  }
  for (const model of catalog?.polish ?? []) {
    if (![model.id, model.name, model.publisher, model.description].join(" ").toLowerCase().includes(needle)) continue;
    hits.push({ key: `local-${model.id}`, title: model.name, publisher: model.publisher, detail: model.description, polishId: model.id });
  }
  return hits;
}
function jumpTo(hit: SearchHit): void {
  closePopover();
  if (hit.family && hit.model) { variants.set(hit.family, hit.model); render(); }
  const target = hit.family ? document.getElementById(`variant-${hit.family}`) : document.getElementById(`model-${hit.polishId}`)?.querySelector("button");
  target?.scrollIntoView({ block: "center", behavior: "smooth" });
  target?.focus({ preventScroll: true });
}
function placePopover(): void {
  const box = searchBox.getBoundingClientRect();
  searchListbox.style.setProperty("--ds-popover-top", `${box.bottom + 6}px`);
  searchListbox.style.setProperty("--ds-popover-left", `${box.left}px`);
  searchListbox.style.setProperty("--ds-popover-trigger-width", `${box.width}px`);
}
function closePopover(): void {
  popoverOpen = false;
  searchListbox.hidden = true;
  searchInput.setAttribute("aria-expanded", "false");
  searchInput.removeAttribute("aria-activedescendant");
}
function optionButton(hit: SearchHit, index: number): HTMLButtonElement {
  const option = document.createElement("button");
  option.type = "button";
  option.className = "ds-menu-option ds-menu-option--stacked model-search-option";
  option.id = `model-search-option-${index}`;
  option.setAttribute("role", "option");
  option.tabIndex = -1;
  option.setAttribute("aria-selected", String(index === activeHit));
  const text = document.createElement("span"); text.className = "ds-menu-option-text";
  const name = document.createElement("span"); name.className = "ds-menu-option-name"; name.textContent = hit.title;
  const supporting = document.createElement("span"); supporting.className = "ds-menu-option-supporting"; supporting.textContent = hit.detail;
  text.append(name, supporting);
  option.append(createPublisherIcon(hit.publisher, { size: "sm" }), text);
  option.addEventListener("pointerenter", () => { if (activeHit !== index) { activeHit = index; paintActive(); } });
  option.addEventListener("mousedown", event => event.preventDefault());
  option.addEventListener("click", () => jumpTo(hit));
  return option;
}
function paintActive(): void {
  const options = [...searchListbox.querySelectorAll<HTMLElement>("[role='option']")];
  options.forEach((option, index) => option.setAttribute("aria-selected", String(index === activeHit)));
  const current = options[activeHit];
  if (current) {
    searchInput.setAttribute("aria-activedescendant", current.id);
    current.scrollIntoView({ block: "nearest" });
  } else searchInput.removeAttribute("aria-activedescendant");
}
function renderPopover(local: SearchHit[], hub: SearchHit[], note: string | null): void {
  searchHits = [...local, ...hub];
  activeHit = Math.min(activeHit, Math.max(0, searchHits.length - 1));
  searchListbox.replaceChildren();
  const addGroup = (label: string, rows: SearchHit[], start: number) => {
    if (!rows.length) return;
    const group = document.createElement("div"); group.className = "model-search-group"; group.setAttribute("role", "group"); group.setAttribute("aria-label", label);
    const heading = document.createElement("div"); heading.className = "model-search-group-heading"; heading.textContent = label; heading.setAttribute("aria-hidden", "true");
    group.append(heading, ...rows.map((hit, i) => optionButton(hit, start + i)));
    searchListbox.append(group);
  };
  addGroup("On this device", local, 0);
  addGroup("Hugging Face", hub, local.length);
  if (note && !searchHits.length) {
    const empty = document.createElement("div"); empty.className = "model-search-empty"; empty.textContent = note;
    searchListbox.append(empty);
  }
  if (searchHits.length) {
    const footer = document.createElement("div"); footer.className = "model-search-footer";
    footer.innerHTML = `<span><span class="ds-kbd-chord"><kbd class="ds-kbd-chord-key">↑</kbd><kbd class="ds-kbd-chord-key">↓</kbd></span> to select</span><span><span class="ds-kbd-chord"><kbd class="ds-kbd-chord-key">↵</kbd></span> to choose</span>`;
    searchListbox.append(footer);
  }
  popoverOpen = true;
  searchListbox.hidden = false;
  searchInput.setAttribute("aria-expanded", "true");
  placePopover();
  paintActive();
}
function renderHubList(hits: SearchHit[]): void {
  searchResults.replaceChildren();
  for (const hit of hits) {
    const item = document.createElement("div"); item.className = "ds-list-row model-row model-search-hit";
    const content = document.createElement("div"); content.className = "ds-list-row-content";
    const title = document.createElement("div"); title.className = "ds-list-row-title"; title.textContent = hit.title;
    const detail = document.createElement("span"); detail.className = "ds-list-row-supporting"; detail.textContent = hit.detail;
    content.append(title, detail);
    const actions = document.createElement("div"); actions.className = "models-actions";
    actions.append(button(hit.family ? "Choose variant" : "Show model", async () => jumpTo(hit)));
    if (hit.repo) actions.append(button("Model page ↗", () => openUrl(`https://huggingface.co/${hit.repo}`)));
    item.append(createPublisherIcon(hit.publisher), content, actions);
    searchResults.append(item);
  }
}
async function searchHub(query: string, revision: number, local: SearchHit[]): Promise<void> {
  searchStatus.textContent = "Searching Hugging Face…"; searchResults.setAttribute("aria-busy", "true");
  if (document.activeElement === searchInput) renderPopover(local, [], "Searching Hugging Face…");
  try {
    const result = await invoke<{ models: HubModel[]; cached: boolean }>("search_hugging_face_models", { query });
    if (revision !== searchRevision) return;
    const hub = result.models.map(compatibleHit).filter((hit): hit is SearchHit => !!hit);
    const count = hub.length;
    searchStatus.textContent = count ? `${count} compatible model${count === 1 ? "" : "s"}${result.cached ? " · Cached" : ""}` : "No compatible models for that search.";
    renderHubList(hub);
    if (document.activeElement === searchInput) renderPopover(localHits(query), hub, count ? null : "No compatible models for that search.");
  } catch (error) {
    if (revision !== searchRevision) return;
    searchStatus.textContent = `Search unavailable: ${String(error)}. Edit your search to try again.`;
    searchResults.replaceChildren();
    if (document.activeElement === searchInput) renderPopover(localHits(query), [], searchStatus.textContent);
  } finally {
    if (revision === searchRevision) searchResults.setAttribute("aria-busy", "false");
  }
}
function scheduleSearch(): void {
  const revision = ++searchRevision; clearTimeout(searchTimer);
  const query = searchInput.value.trim();
  searchClear.hidden = !query;
  searchResults.replaceChildren(); searchResults.setAttribute("aria-busy", "false");
  if (!query) {
    closePopover();
    searchStatus.textContent = "Search speech models on this device and Hugging Face.";
    return;
  }
  const local = localHits(query);
  searchStatus.textContent = "Waiting to search…";
  if (document.activeElement === searchInput) renderPopover(local, [], local.length ? null : "Waiting to search…");
  searchTimer = setTimeout(() => { void searchHub(query, revision, localHits(query)); }, 350);
}
searchInput.addEventListener("input", scheduleSearch);
searchInput.addEventListener("focus", () => {
  const query = searchInput.value.trim();
  if (query && !popoverOpen) renderPopover(localHits(query), [], null);
});
searchClear.addEventListener("mousedown", event => event.preventDefault());
searchClear.addEventListener("click", () => { searchInput.value = ""; searchInput.focus(); scheduleSearch(); });
searchInput.addEventListener("keydown", event => {
  if (event.key === "Escape") {
    event.preventDefault();
    if (popoverOpen) closePopover();
    else { searchInput.value = ""; scheduleSearch(); }
    return;
  }
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    if (!popoverOpen || !searchHits.length) return;
    event.preventDefault();
    activeHit = (activeHit + (event.key === "ArrowDown" ? 1 : searchHits.length - 1)) % searchHits.length;
    paintActive();
    return;
  }
  if (event.key === "Enter") {
    event.preventDefault();
    const hit = searchHits[activeHit];
    if (popoverOpen && hit) jumpTo(hit);
    else { clearTimeout(searchTimer); void searchHub(searchInput.value.trim(), ++searchRevision, localHits(searchInput.value.trim())); }
  }
});
document.addEventListener("pointerdown", event => {
  if (popoverOpen && !searchBox.contains(event.target as Node) && !searchListbox.contains(event.target as Node)) closePopover();
  for (const list of root.querySelectorAll<HTMLElement>(".ds-select-listbox")) {
    if (list.hidden) continue;
    const field = list.closest(".model-variant");
    if (field && !field.contains(event.target as Node)) {
      list.hidden = true;
      field.querySelector("[role='combobox']")?.setAttribute("aria-expanded", "false");
    }
  }
});
window.addEventListener("resize", () => { if (popoverOpen) placePopover(); });
