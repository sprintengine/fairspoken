import "./appearance";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { diffPreview } from "./previewDiff";
import "@fontsource/inter/400.css";
import "@fontsource/inter/500.css";
import "./cursorPreview.css";

type Snapshot = {
  sessionId: number;
  revision: number;
  phase: "idle" | "recording" | "finishing" | "complete";
  text: string;
  polished: boolean;
  remote: boolean;
};
type Edit = { kind: "insert" | "delete"; start: number; end: number; text: string; expires: number };
const box = document.getElementById("cursorPreview")!;
const text = document.getElementById("previewText")!;
const announcement = document.getElementById("previewAnnouncement")!;
let current: Snapshot | undefined;
let receivedEvent = false;
let cleanText = "";
let lastRaw = "";
let edits: Edit[] = [];
let cleanup: ReturnType<typeof setTimeout> | undefined;
let following = true;
let hovering = false;
const order = { recording: 0, finishing: 1, complete: 2, idle: 3 };
const duration = (token: string): number => {
  const value = getComputedStyle(box).getPropertyValue(token).trim();
  return parseFloat(value) * (value.endsWith("ms") ? 1 : 1000);
};
const fadeMs = duration("--sem-motion-duration-deliberate");
const holdMs = duration("--sem-motion-duration-pulse") * 5;

function updateInteraction(active: boolean): void {
  if (!current || current.phase === "idle") return;
  void invoke("set_cursor_preview_interacting", { sessionId: current.sessionId, active }).catch(() => {});
}
box.addEventListener("pointerenter", () => { hovering = true; updateInteraction(true); });
box.addEventListener("pointerleave", () => { hovering = false; updateInteraction(false); });
text.addEventListener("scroll", () => {
  following = text.scrollHeight - text.clientHeight - text.scrollTop <= parseFloat(getComputedStyle(text).lineHeight);
});

function paint(): void {
  const offset = text.scrollTop;
  const tail = following;
  const now = performance.now();
  const fragment = document.createDocumentFragment();
  const boundaries = new Set([0, cleanText.length]);
  for (const edit of edits) { boundaries.add(edit.start); boundaries.add(edit.end); }
  const points = [...boundaries].sort((a, b) => a - b);
  for (let index = 0; index < points.length; index++) {
    const start = points[index];
    for (const edit of edits.filter(e => e.kind === "delete" && e.start === start)) {
      const deleted = document.createElement("del");
      deleted.className = "cursor-preview-deletion";
      deleted.setAttribute("aria-hidden", "true");
      deleted.style.animationDelay = `${edit.expires - now - fadeMs}ms`;
      deleted.textContent = edit.text;
      fragment.append(deleted);
    }
    const end = points[index + 1];
    if (end === undefined || end <= start) continue;
    const inserted = edits.find(e => e.kind === "insert" && e.start <= start && e.end >= end);
    if (inserted) {
      const mark = document.createElement("mark");
      mark.className = "cursor-preview-insertion";
      mark.style.animationDelay = `${inserted.expires - now - fadeMs}ms`;
      mark.textContent = cleanText.slice(start, end);
      fragment.append(mark);
    } else fragment.append(document.createTextNode(cleanText.slice(start, end)));
  }
  if (!cleanText && edits.length === 0) fragment.append(document.createTextNode("…"));
  text.replaceChildren(fragment);
  text.scrollTop = tail ? text.scrollHeight : offset;
  following = tail;
  if (announcement.textContent !== cleanText) announcement.textContent = cleanText;
  clearTimeout(cleanup);
  if (edits.length) {
    cleanup = setTimeout(() => {
      edits = edits.filter(edit => edit.expires > performance.now() + 1);
      paint();
    }, Math.max(0, Math.min(...edits.map(edit => edit.expires)) - performance.now()));
  }
}

function polish(value: string): void {
  const now = performance.now();
  let oldOffset = 0;
  let newOffset = 0;
  const equal: { oldStart: number; oldEnd: number; newStart: number }[] = [];
  const next: Edit[] = [];
  for (const run of diffPreview(cleanText, value)) {
    if (run.kind === "equal") {
      equal.push({ oldStart: oldOffset, oldEnd: oldOffset + run.text.length, newStart: newOffset });
      oldOffset += run.text.length;
      newOffset += run.text.length;
    } else if (run.kind === "delete") {
      next.push({ kind: "delete", start: newOffset, end: newOffset, text: run.text, expires: now + holdMs + fadeMs });
      oldOffset += run.text.length;
    } else {
      next.push({ kind: "insert", start: newOffset, end: newOffset + run.text.length, text: "", expires: now + holdMs + fadeMs });
      newOffset += run.text.length;
    }
  }
  // Preserve still-relevant highlights and their original expiry across later
  // polish passes. A replacement supersedes annotations on the replaced range.
  for (const edit of edits) {
    if (edit.expires <= now) continue;
    const span = equal.find(part => part.oldStart <= edit.start && part.oldEnd >= edit.end);
    if (span) {
      const shift = span.newStart - span.oldStart;
      next.push({ ...edit, start: edit.start + shift, end: edit.end + shift });
    }
  }
  edits = next;
  cleanText = value;
}

function render(snapshot: Snapshot): void {
  if (current && (snapshot.sessionId < current.sessionId ||
    (snapshot.sessionId === current.sessionId && (order[snapshot.phase] < order[current.phase] ||
      (snapshot.phase === "recording" && (snapshot.revision < current.revision ||
        (snapshot.revision === current.revision && current.polished && !snapshot.polished))))))) return;
  const newSession = current?.sessionId !== snapshot.sessionId;
  if (newSession) {
    clearTimeout(cleanup);
    edits = [];
    cleanText = "";
    lastRaw = "";
    following = true;
  }
  current = snapshot;
  box.hidden = snapshot.phase === "idle";
  if (snapshot.phase === "idle") {
    clearTimeout(cleanup);
    edits = [];
    cleanText = lastRaw = "";
    text.replaceChildren();
    announcement.textContent = "";
    hovering = false;
    return;
  }
  if (hovering) updateInteraction(true);
  if (snapshot.polished && cleanText) {
    polish(snapshot.text);
  } else if (!snapshot.polished && snapshot.phase !== "complete" && lastRaw && snapshot.text.startsWith(lastRaw)) {
    // New ASR chunks are cumulative raw text. Keep the already polished prefix
    // visible while appending the new words, until their polish pass arrives.
    cleanText += snapshot.text.slice(lastRaw.length);
    lastRaw = snapshot.text;
  } else {
    edits = [];
    cleanText = snapshot.text;
    if (!snapshot.polished) lastRaw = snapshot.text;
  }
  paint();
}
// Subscribe before fetching; an initial response cannot overwrite live events.
void listen<Snapshot>("cursor-preview-state", event => {
  receivedEvent = true;
  render(event.payload);
}).then(async () => {
  const snapshot = await invoke<Snapshot>("get_cursor_preview_state");
  if (!receivedEvent) render(snapshot);
}).catch(() => { box.hidden = true; });
