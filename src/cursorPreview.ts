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
  polishing: boolean;
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
let lastSize = "";
const order = { recording: 0, finishing: 1, complete: 2, idle: 3 };
const duration = (token: string): number => {
  const value = getComputedStyle(box).getPropertyValue(token).trim();
  return parseFloat(value) * (value.endsWith("ms") ? 1 : 1000);
};
const fadeMs = duration("--sem-motion-duration-deliberate");
const holdMs = duration("--sem-motion-duration-pulse") * 5;

function fitPreview(): void {
  if (!current || box.hidden) return;
  const body = getComputedStyle(document.body);
  const height = Math.ceil(box.getBoundingClientRect().height + parseFloat(body.paddingTop) + parseFloat(body.paddingBottom));
  const dark = document.documentElement.dataset.mode === "dark";
  const key = `${current.sessionId}:${height}:${dark}`;
  if (key === lastSize) return;
  lastSize = key;
  void invoke("set_cursor_preview_size", { sessionId: current.sessionId, height, dark }).catch(() => {
    if (lastSize === key) lastSize = "";
  });
}
new ResizeObserver(fitPreview).observe(box);
new MutationObserver(fitPreview).observe(document.documentElement, { attributes: true, attributeFilter: ["data-mode"] });
void document.fonts.ready.then(fitPreview);

function updateInteraction(active: boolean): void {
  if (!current || current.phase === "idle") return;
  if (active === hovering) return;
  hovering = active;
  void invoke("set_cursor_preview_interacting", { sessionId: current.sessionId, active }).catch(() => {});
}
box.addEventListener("pointerenter", () => updateInteraction(true));
box.addEventListener("pointerleave", () => updateInteraction(false));
box.addEventListener("pointercancel", () => updateInteraction(false));
// Hovering holds the finished box open for reading, so a pointerleave that
// never arrives — the panel resized out from under the pointer, or the webview
// was hidden mid-hover — would strand it on screen for the rest of the session.
// Trust :hover over the event stream, in both directions: it also re-asserts a
// pointer already resting on the box when a new session clears the flag.
setInterval(() => {
  if (!current || current.phase === "idle" || box.hidden) return;
  const over = box.matches(":hover");
  if (over !== hovering) updateInteraction(over);
}, 500);
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
  fitPreview();
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

// Passes land in bursts with short gaps between them. Following that signal
// literally made the brush strobe, so once lit it stays lit through a gap this
// long; only a real stop puts it out.
const POLISH_DWELL_MS = 900;
let polishHold: ReturnType<typeof setTimeout> | undefined;

function setPolishMark(active: boolean, live: boolean): void {
  clearTimeout(polishHold);
  if (active) {
    box.classList.add("is-polishing");
    return;
  }
  // The end of the session is a real stop: settle immediately rather than
  // leaving the brush glowing over finished text.
  if (!live) {
    box.classList.remove("is-polishing");
    return;
  }
  polishHold = setTimeout(() => box.classList.remove("is-polishing"), POLISH_DWELL_MS);
}

function setActivity(snapshot: Snapshot): void {
  // The sweeping rim tracks the session, not individual passes. Tying it to
  // polish activity made it flicker and restart its rotation every few hundred
  // milliseconds, so it never travelled far enough to read as motion. It runs
  // unbroken from the first word to the committed transcript.
  const live = snapshot.phase === "recording" || snapshot.phase === "finishing";
  box.classList.toggle("is-live", live);
  // The brush is the per-pass signal: it lights while text is being rewritten.
  setPolishMark(live && snapshot.polishing, live);
}

// True when a snapshot carries no change other than polish activity, so the
// diff highlights and scroll position can be left alone.
function activityOnly(snapshot: Snapshot, previous: Snapshot): boolean {
  return snapshot.sessionId === previous.sessionId
    && snapshot.revision === previous.revision
    && snapshot.phase === previous.phase
    && snapshot.polished === previous.polished
    && snapshot.text === previous.text;
}

function render(snapshot: Snapshot): void {
  if (current && (snapshot.sessionId < current.sessionId ||
    (snapshot.sessionId === current.sessionId && (order[snapshot.phase] < order[current.phase] ||
      (snapshot.phase === "recording" && (snapshot.revision < current.revision ||
        (snapshot.revision === current.revision && current.polished && !snapshot.polished))))))) return;
  if (current && activityOnly(snapshot, current)) {
    current = snapshot;
    setActivity(snapshot);
    return;
  }
  const newSession = current?.sessionId !== snapshot.sessionId;
  if (newSession) {
    clearTimeout(cleanup);
    edits = [];
    cleanText = "";
    lastRaw = "";
    following = true;
    lastSize = "";
    // show() cleared the backend's hover flag; the watchdog re-asserts it if
    // the pointer is in fact still over the box.
    hovering = false;
  }
  current = snapshot;
  box.hidden = snapshot.phase === "idle";
  setActivity(snapshot);
  if (snapshot.phase === "idle") {
    clearTimeout(cleanup);
    edits = [];
    cleanText = lastRaw = "";
    text.replaceChildren();
    announcement.textContent = "";
    hovering = false;
    return;
  }
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
