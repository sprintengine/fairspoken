import "./appearance"; // the shelf follows the app's light or dark choice
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { addEvent } from "./events";
import { createPreviewOrder, type PreviewPosition } from "./transcriptPreviewOrder";
import { required } from "./dom";
import { errorMessage } from "./errors";

interface TranscriptPreviewEvent extends PreviewPosition {
  text: string;
  finalPreview: boolean;
}

interface TranscriptHistoryItem {
  id: string;
  createdAt: number;
  text: string;
  polished?: boolean;
  rawText?: string | null;
}

interface TranscriptHistoryUpdatedEvent {
  item: TranscriptHistoryItem;
}

const liveTranscriptBubble = required<HTMLElement>("liveTranscriptBubble");
const liveTranscriptText = required<HTMLElement>("liveTranscriptText");
const transcriptShelf = required<HTMLElement>("transcriptShelf");

const appWindow = getCurrentWindow();
// A press that moves drags the whole stack; a press that stays put copies that
// clip. We drive both from JS (rather than a CSS drag region) so the click is
// delivered reliably on macOS instead of being swallowed by the OS drag — the
// same approach the pill uses in main.ts.
const CLIP_DRAG_THRESHOLD_PX = 4;

let transcriptHistory: TranscriptHistoryItem[] = [];
let copiedTranscriptId: string | null = null;
let relativeTimeTimer: ReturnType<typeof setInterval> | null = null;
// Clips collapse to their first sentences; this tracks which the user expanded
// so the choice survives re-renders.
const expandedTranscriptIds = new Set<string>();
let clipPress: { x: number; y: number; dragging: boolean } | null = null;
let suppressClipClick = false;
let contextMenu: HTMLElement | null = null;
// The stack is shown only when summoned from the pill (click or shortcut).
// It never opens on its own, so it cannot sit on top of and block whatever the
// user is working on behind it.
let shelfVisible = false;

// Failures land in the Activity feed; the shelf itself stays as it was.
function reportError(error: unknown): void {
  addEvent("warning", errorMessage(error));
}

async function loadTranscriptHistory(): Promise<void> {
  transcriptHistory = uniqueTranscriptItems(await invoke<TranscriptHistoryItem[]>("get_transcript_history"));
  copiedTranscriptId = copiedTranscriptId ?? transcriptHistory[0]?.id ?? null;
  renderTranscriptShelf();
}

function renderTranscriptShelf(): void {
  transcriptShelf.replaceChildren();

  if (transcriptHistory.length === 0) {
    const empty = document.createElement("p");
    empty.className = "shelf-empty";
    empty.textContent = "No copied messages yet";
    transcriptShelf.append(empty);
    return;
  }

  for (const item of transcriptHistory.slice(0, 8)) {
    transcriptShelf.append(buildTranscriptClip(item));
  }
}

function buildTranscriptClip(item: TranscriptHistoryItem): HTMLElement {
  const isCopied = item.id === copiedTranscriptId;
  const fullText = item.text.replace(/\s+/g, " ").trim();
  const collapsed = collapsedTranscriptText(fullText);
  const expandable = collapsed !== fullText;
  const expanded = expandable && expandedTranscriptIds.has(item.id);

  const clip = document.createElement("div");
  clip.className = "transcript-clip";
  clip.dataset.id = item.id;
  clip.dataset.copied = String(isCopied);

  const content = document.createElement("div");
  content.className = "transcript-clip-content";

  const button = document.createElement("button");
  button.type = "button";
  button.className = "transcript-clip-copy";
  button.title = item.text;
  button.setAttribute("aria-label", isCopied ? "Copied transcript clip" : "Copy transcript clip");

  const text = document.createElement("span");
  text.className = "transcript-clip-text";
  text.textContent = expanded ? fullText : collapsed;
  button.append(text);
  button.addEventListener("mousedown", (event) => {
    if (event.button !== 0) return;
    suppressClipClick = false;
    clipPress = { x: event.clientX, y: event.clientY, dragging: false };
  });
  button.addEventListener("click", () => {
    if (suppressClipClick) {
      suppressClipClick = false;
      return;
    }
    void copyTranscriptItem(item.id).catch(reportError);
  });

  const foot = document.createElement("div");
  foot.className = "transcript-clip-foot";

  const meta = document.createElement("span");
  meta.className = "transcript-clip-meta";
  meta.dataset.createdAt = String(item.createdAt);
  meta.textContent = relativeTimeLabel(item.createdAt);
  foot.append(meta);

  if (item.polished) {
    const badge = document.createElement("span");
    badge.className = "transcript-clip-badge";
    badge.textContent = "AI polished";
    badge.title = "This transcript was cleaned up by AI polish";
    foot.append(badge);
  }

  if (item.polished && item.rawText) {
    const copyOriginal = document.createElement("button");
    copyOriginal.type = "button";
    copyOriginal.className = "transcript-clip-expand";
    copyOriginal.textContent = "Copy original";
    copyOriginal.title = "Copy the transcript as dictated, before AI polish";
    copyOriginal.addEventListener("click", (event) => {
      event.stopPropagation();
      void invoke("copy_original_transcript", { id: item.id }).catch(reportError);
    });
    foot.append(copyOriginal);
  }

  if (expandable) {
    const expandButton = document.createElement("button");
    expandButton.type = "button";
    expandButton.className = "transcript-clip-expand";
    expandButton.textContent = expanded ? "Show less" : "Show more";
    expandButton.setAttribute("aria-expanded", String(expanded));
    expandButton.addEventListener("click", (event) => {
      event.stopPropagation();
      const nowExpanded = !expandedTranscriptIds.has(item.id);
      if (nowExpanded) {
        expandedTranscriptIds.add(item.id);
      } else {
        expandedTranscriptIds.delete(item.id);
      }
      text.textContent = nowExpanded ? fullText : collapsed;
      expandButton.textContent = nowExpanded ? "Show less" : "Show more";
      expandButton.setAttribute("aria-expanded", String(nowExpanded));
    });
    foot.append(expandButton);
  }

  content.append(button, foot);

  const closeButton = document.createElement("button");
  closeButton.type = "button";
  closeButton.className = "transcript-clip-close";
  closeButton.setAttribute("aria-label", "Delete transcript clip");
  closeButton.title = "Delete";
  closeButton.textContent = "✕";
  closeButton.addEventListener("click", () => {
    deleteTranscriptItem(item.id);
  });

  clip.append(content, closeButton);
  return clip;
}

// Collapse a clip to its first two sentences, falling back to a hard character
// cap when the text has no sentence breaks (or one very long sentence).
function collapsedTranscriptText(fullText: string): string {
  const sentences = fullText.match(/[^.!?]+[.!?]+(?=\s|$)/g);
  let collapsed = sentences ? sentences.slice(0, 2).join(" ").trim() : fullText;
  if (collapsed.length > 200) {
    collapsed = `${collapsed.slice(0, 200).trimEnd()}…`;
  }
  return collapsed;
}

async function copyTranscriptItem(id: string): Promise<void> {
  const item = await invoke<TranscriptHistoryItem>("copy_transcript_history_item", { id });
  copiedTranscriptId = item.id;
  // Grab-and-go: the copied clip flashes a green border as confirmation, then
  // the stack closes so it is out of the way. The pill keeps its copied dot.
  const clip = transcriptShelf.querySelector<HTMLElement>(`.transcript-clip[data-id="${CSS.escape(id)}"]`);
  if (clip) {
    clip.dataset.justCopied = "true";
    await new Promise((resolve) => window.setTimeout(resolve, 440));
  }
  await hideShelf();
}

function deleteTranscriptItem(id: string): void {
  const clip = transcriptShelf.querySelector<HTMLElement>(`.transcript-clip[data-id="${CSS.escape(id)}"]`);
  clip?.setAttribute("data-removing", "true");

  window.setTimeout(async () => {
    try {
      await invoke("delete_transcript_history_item", { id });
    } catch (error) {
      // The clip is still there: bring it back rather than leave it faded out.
      clip?.removeAttribute("data-removing");
      reportError(error);
      return;
    }
    transcriptHistory = transcriptHistory.filter((item) => item.id !== id);
    expandedTranscriptIds.delete(id);
    if (copiedTranscriptId === id) {
      copiedTranscriptId = transcriptHistory[0]?.id ?? null;
    }
    renderTranscriptShelf();
  }, 170);
}

function addOrReplaceTranscriptItem(item: TranscriptHistoryItem): void {
  transcriptHistory = uniqueTranscriptItems([item, ...transcriptHistory]).slice(0, 50);
  copiedTranscriptId = item.id;
  renderTranscriptShelf();
}

function showLiveTranscript(text: string): void {
  const preview = recentTranscriptText(text);
  if (!preview) return;
  liveTranscriptText.textContent = preview;
  liveTranscriptBubble.hidden = false;
  liveTranscriptText.scrollTop = liveTranscriptText.scrollHeight;
}

function resetLiveTranscript(): void {
  liveTranscriptText.textContent = "";
  liveTranscriptBubble.hidden = true;
}

function recentTranscriptText(text: string): string {
  const normalized = text.replace(/\s+/g, " ").trim();
  if (normalized.length <= 320) return normalized;

  const tail = normalized.slice(-320);
  const sentenceStart = tail.search(/[.!?]\s+[A-Z0-9]/);
  return sentenceStart >= 0 ? tail.slice(sentenceStart + 2).trim() : tail.trim();
}

function uniqueTranscriptItems(items: TranscriptHistoryItem[]): TranscriptHistoryItem[] {
  const seen = new Set<string>();
  const unique: TranscriptHistoryItem[] = [];
  for (const item of items) {
    const key = item.text.replace(/\s+/g, " ").trim().toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    unique.push(item);
  }
  return unique;
}

function relativeTimeLabel(createdAt: number): string {
  const ageSeconds = Math.max(0, Math.floor((Date.now() - createdAt) / 1000));
  if (ageSeconds < 20) return "10 seconds ago";
  if (ageSeconds < 45) return "30 seconds ago";
  if (ageSeconds < 90) return "1 minute ago";
  if (ageSeconds < 210) return "2 minutes ago";
  if (ageSeconds < 450) return "5 minutes ago";
  if (ageSeconds < 900) return "10 minutes ago";
  if (ageSeconds < 1800) return "15 minutes ago";
  if (ageSeconds < 2700) return "30 minutes ago";
  if (ageSeconds < 5400) return "45 minutes ago";
  if (ageSeconds < 9000) return "1 hour ago";
  const hours = Math.round(ageSeconds / 3600);
  if (hours < 24) return `${hours} hours ago`;
  const days = Math.round(hours / 24);
  return days === 1 ? "1 day ago" : `${days} days ago`;
}

function refreshRelativeTimes(): void {
  transcriptShelf.querySelectorAll<HTMLElement>(".transcript-clip-meta").forEach((meta) => {
    const createdAt = Number(meta.dataset.createdAt);
    if (Number.isFinite(createdAt)) {
      meta.textContent = relativeTimeLabel(createdAt);
    }
  });
}

function startRelativeTimeRefresh(): void {
  if (relativeTimeTimer !== null) return;
  relativeTimeTimer = setInterval(refreshRelativeTimes, 10_000);
}

function stopRelativeTimeRefresh(): void {
  if (relativeTimeTimer !== null) {
    clearInterval(relativeTimeTimer);
    relativeTimeTimer = null;
  }
}

async function showShelf(): Promise<void> {
  if (!shelfVisible) {
    await invoke("show_transcript_shelf_window");
    shelfVisible = true;
  }
  refreshRelativeTimes();
  startRelativeTimeRefresh();
}

async function hideShelf(): Promise<void> {
  shelfVisible = false;
  resetLiveTranscript();
  stopRelativeTimeRefresh();
  closeContextMenu();
  await invoke("hide_transcript_shelf_window");
}

function openContextMenu(x: number, y: number): void {
  closeContextMenu();

  const menu = document.createElement("div");
  menu.className = "shelf-context-menu";
  menu.setAttribute("role", "menu");

  const hideItem = document.createElement("button");
  hideItem.type = "button";
  hideItem.className = "shelf-context-item";
  hideItem.setAttribute("role", "menuitem");
  hideItem.textContent = "Hide";
  hideItem.addEventListener("click", () => {
    void hideShelf().catch(reportError);
  });

  menu.append(hideItem);
  document.body.append(menu);
  contextMenu = menu;

  // Keep the menu inside the small frameless window.
  const { width, height } = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(4, Math.min(x, window.innerWidth - width - 4))}px`;
  menu.style.top = `${Math.max(4, Math.min(y, window.innerHeight - height - 4))}px`;
  hideItem.focus();
}

function closeContextMenu(): void {
  contextMenu?.remove();
  contextMenu = null;
}

// A press that moves drags the whole stack; a stationary press copies the clip
// (the click handler does that). Drag detection lives on window so it keeps
// tracking once the pointer leaves the originating clip.
window.addEventListener("mousemove", (event) => {
  if (clipPress === null || clipPress.dragging) return;
  if (
    Math.abs(event.clientX - clipPress.x) > CLIP_DRAG_THRESHOLD_PX ||
    Math.abs(event.clientY - clipPress.y) > CLIP_DRAG_THRESHOLD_PX
  ) {
    clipPress.dragging = true;
    suppressClipClick = true;
    void appWindow.startDragging();
  }
});

window.addEventListener("mouseup", () => {
  clipPress = null;
});

// Right-click anywhere on the stack offers a single action: hide it.
window.addEventListener("contextmenu", (event) => {
  event.preventDefault();
  openContextMenu(event.clientX, event.clientY);
});

window.addEventListener("mousedown", (event) => {
  if (contextMenu && !contextMenu.contains(event.target as Node)) {
    closeContextMenu();
  }
});

window.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeContextMenu();
});

window.addEventListener("blur", closeContextMenu);

// Recording previews update the live bubble only while the stack is already
// open; they never summon it. Stale previews (older session or revision, or raw
// text after its polish) are dropped so the bubble never steps back.
const previewOrder = createPreviewOrder();
void listen<number>("transcript-session-started", (event) => {
  previewOrder.startSession(event.payload);
}).catch(reportError);
void listen<TranscriptPreviewEvent>("transcript-preview", (event) => {
  if (event.payload.finalPreview) {
    resetLiveTranscript();
    return;
  }
  if (!previewOrder.accept(event.payload)) return;
  if (shelfVisible) {
    showLiveTranscript(event.payload.text);
  }
}).catch(reportError);

// A finished transcript is recorded into the stack in the background; it does
// not pop the stack open.
void listen<TranscriptHistoryUpdatedEvent>("transcript-history-updated", (event) => {
  resetLiveTranscript();
  addOrReplaceTranscriptItem(event.payload.item);
}).catch(reportError);

// The only thing that opens the stack: a summon from the pill (click or
// shortcut). Toggling again dismisses it.
void listen("transcript-shelf-toggle", async () => {
  try {
    if (shelfVisible) {
      await hideShelf();
      return;
    }
    // A stale list is better than no shelf: show it even if the reload fails.
    await loadTranscriptHistory().catch(reportError);
    await showShelf();
  } catch (error) {
    reportError(error);
  }
}).catch(reportError);

void loadTranscriptHistory().catch(reportError);
