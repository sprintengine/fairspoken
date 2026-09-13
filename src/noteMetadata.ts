import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./noteMetadata.css";
type Event = { sequence: number; elapsedMs: number; kind: string; data: Record<string, any> };
type Metadata = { version: number; sessionId: string; startedAt: number; originalTranscript: string | null; polishedTranscript: string | null; savedText: string | null; clipboardText: string | null; omittedEvents: number; events: Event[] };
let available = false;
let viewGeneration = 0;
let menu: HTMLElement | null = null;
let menuOrigin: HTMLElement | null = null;
function control(label: string, action: () => void): HTMLButtonElement {
  const b = document.createElement("button"); b.type = "button"; b.className = "ds-button ds-button--ghost"; b.textContent = label; b.addEventListener("click", action); return b;
}
export async function initializeNoteDebug(refresh: () => void): Promise<void> {
  try {
    const state = await invoke<{available: boolean; enabled: boolean}>("get_note_debug_status");
    available = state.available; if (!available) return;
    const toolbar = document.createElement("div"); toolbar.className = "note-debug-toolbar"; toolbar.dataset.mode = "dark";
    toolbar.title = "Captures future notes locally, including Accessibility text when Context awareness is on. Resets when the app restarts.";
    const label = document.createElement("span"); label.id = "noteDebugLabel"; label.textContent = "Capture metadata (dev)";
    const toggle = document.createElement("button"); toggle.type = "button"; toggle.className = "ds-switch"; toggle.setAttribute("role", "switch"); toggle.setAttribute("aria-labelledby", label.id); toggle.setAttribute("aria-checked", String(state.enabled));
    const thumb = document.createElement("span"); thumb.className = "ds-switch-thumb"; toggle.append(thumb);
    const status = document.createElement("span"); status.setAttribute("role", "status");
    toggle.addEventListener("keydown", e => { if (e.key === "Enter") e.preventDefault(); });
    toggle.addEventListener("click", () => {
      toggle.disabled = true;
      void invoke("set_note_debug_capture", { enabled: toggle.getAttribute("aria-checked") !== "true" })
        .then(() => { status.textContent = ""; })
        .catch(e => { status.textContent = String(e); })
        .finally(() => { toggle.disabled = false; });
    });
    toolbar.append(label, toggle, status); document.querySelector(".topbar")?.insertBefore(toolbar, document.querySelector("#notesSearch"));
    await listen<{enabled: boolean}>("note-debug-status", e => toggle.setAttribute("aria-checked", String(e.payload.enabled)));
    refresh();
  } catch (e) { console.error("Note debug controls unavailable", e); }
}
function closeMenu(restore = false): void {
  menu?.remove(); menu = null;
  menuOrigin?.setAttribute("aria-expanded", "false"); menuOrigin?.removeAttribute("aria-controls");
  if (restore) menuOrigin?.focus(); menuOrigin = null;
}
export function attachMetadataMenu(row: HTMLElement, id: string, open: () => void): void {
  if (!available) return;
  row.setAttribute("aria-haspopup", "menu"); row.setAttribute("aria-expanded", "false");
  const show = (x: number, y: number) => {
    closeMenu(); menuOrigin = row;
    menu = document.createElement("div"); menu.id = "note-metadata-menu"; menu.className = "ds-popover ds-popover--pointer"; menu.dataset.mode = "dark"; menu.setAttribute("role", "menu"); menu.setAttribute("aria-label", "Note actions");
    const list = document.createElement("div"); list.className = "ds-menu";
    const item = control("View metadata", () => { closeMenu(); open(); }); item.className = "ds-menu-item"; item.setAttribute("role", "menuitem"); item.dataset.noteId = id;
    list.append(item); menu.append(list); document.body.append(menu);
    row.setAttribute("aria-expanded", "true"); row.setAttribute("aria-controls", menu.id);
    const inset = parseFloat(getComputedStyle(menu).getPropertyValue("--sem-space-sm")) || 8;
    menu.style.setProperty("--ds-popover-left", `${Math.max(inset, Math.min(x, innerWidth - menu.offsetWidth - inset))}px`);
    menu.style.setProperty("--ds-popover-top", `${Math.max(inset, Math.min(y, innerHeight - menu.offsetHeight - inset))}px`);
    item.focus();
    menu.addEventListener("keydown", e => {
      if (["Escape", "Tab"].includes(e.key)) { if (e.key === "Escape") e.preventDefault(); closeMenu(true); }
      if (["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) { e.preventDefault(); item.focus(); }
    });
  };
  row.addEventListener("contextmenu", e => { e.preventDefault(); show(e.clientX, e.clientY); });
  row.addEventListener("keydown", e => { if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) { e.preventDefault(); const r = row.getBoundingClientRect(); show(r.left, r.bottom); } });
}
document.addEventListener("pointerdown", e => { if (menu && !menu.contains(e.target as Node)) closeMenu(); }, true);
window.addEventListener("resize", () => closeMenu());
document.addEventListener("scroll", e => { if (menu && !menu.contains(e.target as Node)) closeMenu(); }, true);
export function closeMetadataView(): void { viewGeneration++; }
function block(title: string, text: unknown, collapsed = false): HTMLElement {
  const section = document.createElement("section"); section.className = "ds-section note-metadata-event";
  const header = document.createElement("div"); header.className = "ds-section-header";
  const heading = document.createElement("h3"); heading.className = "ds-section-title"; heading.textContent = title; header.append(heading);
  const body = document.createElement("div"); body.className = "ds-section-body";
  const pre = document.createElement("pre");
  const fill = () => { pre.textContent = text == null ? "Not captured / not available" : typeof text === "string" ? text : JSON.stringify(text, null, 2); };
  body.append(pre);
  if (collapsed) {
    body.hidden = true;
    const toggle = control("Show", () => { body.hidden = !body.hidden; if (!body.hidden) fill(); toggle.textContent = body.hidden ? "Show" : "Hide"; toggle.setAttribute("aria-expanded",String(!body.hidden)); });
    toggle.setAttribute("aria-expanded","false"); header.append(toggle);
  } else fill();
  section.append(header, body); return section;
}
export function renderNoteMetadata(host: HTMLElement, id: string, back: () => void): void {
  const generation = ++viewGeneration;
  const view = document.createElement("div"); view.className = "note-metadata"; view.dataset.mode = "dark";
  const head = document.createElement("div"); head.className = "note-metadata-heading";
  const title = document.createElement("h2"); title.textContent = "Note metadata"; title.tabIndex = -1;
  const exit = control("Back to note", back); head.append(title, exit); view.append(head);
  const message = document.createElement("p"); message.setAttribute("role","status"); message.textContent = "Loading recording metadata…"; view.append(message);
  host.replaceChildren(view); title.focus();
  view.addEventListener("keydown", e => { if (e.key === "Escape") { e.preventDefault(); back(); } });
  void invoke<Metadata | null>("get_note_metadata", { id }).then(metadata => {
    if (generation !== viewGeneration || !view.isConnected) return;
    if (!metadata) { message.textContent = "No metadata was captured for this note. Enable Capture metadata (dev), then record a new note. Earlier recordings cannot be reconstructed."; return; }
    message.textContent = `Recording snapshot · ${new Date(metadata.startedAt).toLocaleString()}${metadata.omittedEvents ? ` · ${metadata.omittedEvents} events omitted because the trace limit was reached` : ""}. Editing the note does not change this evidence.`;
    view.append(block("Original full transcript", metadata.originalTranscript), block("After final polish · before dictionary rules", metadata.polishedTranscript), block("Saved note", metadata.savedText), block("Text prepared for clipboard / insertion",metadata.clipboardText,true));
    for (const e of metadata.events.filter(e => e.kind.startsWith("accessibility") || e.kind === "recording-settings")) view.append(block(`${e.kind} · ${e.elapsedMs} ms`,e.data,true));
    const chunks = metadata.events.filter(e => e.kind === "asr-chunk-start");
    if (!chunks.length) view.append(block("Speech chunks", "No local chunk events were captured. Remote transcription supplies its complete text at the end."));
    for (const e of chunks) {
      const result = metadata.events.find(r => r.kind === "asr-chunk-result" && r.data.index === e.data.index);
      view.append(block(`Speech chunk ${e.data.index} · ${e.data.durationSeconds.toFixed(2)} s${e.data.overlapsPrevious ? " · overlaps previous" : ""}`, result?.data.text ?? result?.data.error ?? "No result before the snapshot closed"));
      view.append(block(`Chunk ${e.data.index} input parameters`,e.data,true));
    }
    const explanation = document.createElement("p"); explanation.textContent = "Live polish receives cumulative merged text, not isolated speech chunks. Previews without a polish attempt were superseded or recording ended before they ran. Final polish starts again from the complete raw transcript."; view.append(explanation);
    for (const start of metadata.events.filter(e => e.kind === "polish-start")) {
      const related = metadata.events.filter(e => e.data.attempt === start.sequence);
      const result = related.find(e => e.kind === "polish-result");
      const request = related.find(e => e.kind === "polish-request");
      view.append(block(`${start.data.phase} · input · ${start.elapsedMs} ms`,start.data.input));
      view.append(block(`${start.data.phase} · ${result?.data.value.status ?? "unfinished at snapshot"}`,result?.data.value.acceptedOutput ?? "No accepted result before the snapshot closed."));
      if (result?.data.value.reason) view.append(block("Reason",result.data.value.reason));
      view.append(block(`${start.data.phase} · actual request body`,request?.data.value ?? "No polish request was sent.",true));
      view.append(block(`${start.data.phase} · raw response and processing details`,related,true));
    }
    view.append(block("All events · chronological trace",metadata.events,true));
  }).catch(e => { if (generation === viewGeneration) message.textContent = `Could not load metadata: ${String(e)}`; });
}
