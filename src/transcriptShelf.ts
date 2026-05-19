import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface TranscriptPreviewEvent {
  text: string;
  finalPreview: boolean;
}

interface TranscriptHistoryItem {
  id: string;
  createdAt: number;
  text: string;
}

interface TranscriptHistoryUpdatedEvent {
  item: TranscriptHistoryItem;
}

const SHELF_VISIBLE_MS = 18_000;

const liveTranscriptBubble = required<HTMLElement>("liveTranscriptBubble");
const liveTranscriptText = required<HTMLElement>("liveTranscriptText");
const transcriptShelf = required<HTMLElement>("transcriptShelf");

let transcriptHistory: TranscriptHistoryItem[] = [];
let copiedTranscriptId: string | null = null;
let hideTimer: ReturnType<typeof setTimeout> | null = null;
let relativeTimeTimer: ReturnType<typeof setInterval> | null = null;

function required<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`Missing #${id}`);
  return node as T;
}

async function loadTranscriptHistory(): Promise<void> {
  transcriptHistory = uniqueTranscriptItems(await invoke<TranscriptHistoryItem[]>("get_transcript_history"));
  copiedTranscriptId = copiedTranscriptId ?? transcriptHistory[0]?.id ?? null;
  renderTranscriptShelf();
}

function renderTranscriptShelf(): void {
  transcriptShelf.replaceChildren();

  for (const item of transcriptHistory.slice(0, 8)) {
    const clip = document.createElement("div");
    clip.className = "transcript-clip";
    clip.dataset.id = item.id;
    clip.dataset.copied = String(item.id === copiedTranscriptId);

    const button = document.createElement("button");
    button.type = "button";
    button.className = "transcript-clip-copy";
    button.title = item.text;
    button.setAttribute("aria-label", item.id === copiedTranscriptId ? "Copied transcript clip" : "Copy transcript clip");

    const text = document.createElement("span");
    text.className = "transcript-clip-text";
    text.textContent = item.text;
    const meta = document.createElement("span");
    meta.className = "transcript-clip-meta";
    meta.dataset.createdAt = String(item.createdAt);
    meta.textContent = relativeTimeLabel(item.createdAt);
    button.append(text, meta);
    button.addEventListener("click", () => {
      void copyTranscriptItem(item.id);
    });

    const closeButton = document.createElement("button");
    closeButton.type = "button";
    closeButton.className = "transcript-clip-close";
    closeButton.setAttribute("aria-label", "Delete transcript clip");
    closeButton.title = "Delete";
    closeButton.textContent = "✕";
    closeButton.addEventListener("click", () => {
      void deleteTranscriptItem(item.id);
    });

    clip.append(button, closeButton);
    transcriptShelf.append(clip);
  }
}

async function copyTranscriptItem(id: string): Promise<void> {
  const item = await invoke<TranscriptHistoryItem>("copy_transcript_history_item", { id });
  copiedTranscriptId = item.id;
  renderTranscriptShelf();
  await showShelf();
}

async function deleteTranscriptItem(id: string): Promise<void> {
  const clip = transcriptShelf.querySelector<HTMLElement>(`.transcript-clip[data-id="${CSS.escape(id)}"]`);
  clip?.setAttribute("data-removing", "true");

  window.setTimeout(async () => {
    await invoke("delete_transcript_history_item", { id });
    transcriptHistory = transcriptHistory.filter((item) => item.id !== id);
    if (copiedTranscriptId === id) {
      copiedTranscriptId = transcriptHistory[0]?.id ?? null;
    }
    renderTranscriptShelf();
    if (transcriptHistory.length === 0 && liveTranscriptBubble.hidden) {
      await hideShelf();
    }
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
  if (hideTimer !== null) {
    clearTimeout(hideTimer);
  }
  await invoke("show_transcript_shelf_window");
  refreshRelativeTimes();
  startRelativeTimeRefresh();
  hideTimer = setTimeout(() => void hideShelf(), SHELF_VISIBLE_MS);
}

async function hideShelf(): Promise<void> {
  hideTimer = null;
  resetLiveTranscript();
  stopRelativeTimeRefresh();
  await invoke("hide_transcript_shelf_window");
}

void listen<TranscriptPreviewEvent>("transcript-preview", async (event) => {
  if (event.payload.finalPreview) {
    resetLiveTranscript();
    return;
  }
  showLiveTranscript(event.payload.text);
  await showShelf();
});

void listen<TranscriptHistoryUpdatedEvent>("transcript-history-updated", async (event) => {
  resetLiveTranscript();
  addOrReplaceTranscriptItem(event.payload.item);
  await showShelf();
});

void loadTranscriptHistory();
