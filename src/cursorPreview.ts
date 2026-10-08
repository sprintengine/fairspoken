import "./appearance";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { diffPreview } from "./previewDiff";
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
const copyButton = document.getElementById("previewCopy") as HTMLButtonElement;
const announcement = document.getElementById("previewAnnouncement")!;
let current: Snapshot | undefined;
let receivedEvent = false;
let cleanText = "";
let lastRaw = "";
let edits: Edit[] = [];
let cleanup: ReturnType<typeof setTimeout> | undefined;
let following = true;
let hovering = false;
let claimed = false;
let edited = false;
let holdReady = false;
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

function resetClaim(): void {
  claimed = false;
  edited = false;
  holdReady = false;
  text.contentEditable = "false";
  text.classList.remove("is-editing");
  text.setAttribute("aria-label", "Dictation text");
}

function setClaimed(next: boolean): void {
  if (!current || current.phase === "idle") return;
  if (claimed === next) {
    if (next) enableEditing();
    return;
  }
  claimed = next;
  if (!next) {
    edited = false;
    text.contentEditable = "false";
    text.classList.remove("is-editing");
    text.setAttribute("aria-label", "Dictation text");
  }
  void invoke("set_cursor_preview_claimed", { sessionId: current.sessionId, claimed: next }).catch(() => {});
  if (next) {
    updateInteraction(true);
    enableEditing();
  }
}

function enableEditing(): void {
  if (!current || edited) return;
  try {
    text.contentEditable = "plaintext-only";
  } catch {
    text.contentEditable = "true";
  }
  text.classList.add("is-editing");
  text.setAttribute("aria-label", "Edit dictation");
  text.focus();
}

function visibleText(): string {
  const copy = text.cloneNode(true) as HTMLElement;
  copy.querySelectorAll("del").forEach(node => node.remove());
  return (copy.textContent ?? "").replace(/\u00a0/g, " ");
}

box.addEventListener("pointerenter", () => {
  if (!current || current.phase === "idle") return;
  if (current.phase === "complete") holdReady = true;
  if (claimed || current.phase === "complete") updateInteraction(true);
});
box.addEventListener("pointerleave", () => {
  if (claimed) return;
  updateInteraction(false);
});
box.addEventListener("pointercancel", () => {
  if (claimed) return;
  updateInteraction(false);
});
box.addEventListener("click", event => {
  if ((event.target as HTMLElement).closest(".cursor-preview-copy")) return;
  setClaimed(true);
});
copyButton.addEventListener("click", event => {
  event.preventDefault();
  event.stopPropagation();
  const value = (edited ? text.innerText : visibleText()).trimEnd();
  void invoke("copy_cursor_preview_text", { text: value }).then(() => {
    announcement.textContent = "Copied";
  }).catch(() => {});
});
text.addEventListener("input", () => {
  if (!claimed) return;
  edited = true;
  edits = [];
  cleanText = visibleText();
});
text.addEventListener("paste", event => {
  if (!claimed) return;
  event.preventDefault();
  const pasted = event.clipboardData?.getData("text/plain") ?? "";
  document.execCommand("insertText", false, pasted);
});
window.addEventListener("keydown", event => {
  if (event.key !== "Escape" || !claimed) return;
  event.preventDefault();
  setClaimed(false);
  updateInteraction(false);
});
window.addEventListener("blur", () => {
  if (!claimed || current?.phase !== "complete") return;
  setClaimed(false);
  updateInteraction(false);
});
// After complete, only a fresh pointerenter counts as a hold: the box appears
// next to the caret, which is usually where the pointer already is. Claim
// pins the box until the user copies, presses Escape, or clicks away. The
// reconcile poll runs only while the box is on screen.
let hoverPoll: ReturnType<typeof setInterval> | undefined;
function syncHoverPoll(): void {
  const showing = !!current && current.phase !== "idle" && !box.hidden;
  if (showing && hoverPoll === undefined) hoverPoll = setInterval(reconcileHover, 500);
  else if (!showing && hoverPoll !== undefined) {
    clearInterval(hoverPoll);
    hoverPoll = undefined;
  }
}
function reconcileHover(): void {
  if (!current || current.phase === "idle" || box.hidden) return;
  if (claimed) {
    if (!hovering) updateInteraction(true);
    return;
  }
  const over = box.matches(":hover");
  if (current.phase === "complete") {
    const hold = holdReady && over;
    if (hold !== hovering) updateInteraction(hold);
    return;
  }
  if (hovering) updateInteraction(false);
}
text.addEventListener("scroll", () => {
  following = text.scrollHeight - text.clientHeight - text.scrollTop <= parseFloat(getComputedStyle(text).lineHeight);
});

function paint(): void {
  if (edited) {
    fitPreview();
    return;
  }
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
  scheduleAnnouncement();
  clearTimeout(cleanup);
  if (edits.length) {
    cleanup = setTimeout(() => {
      edits = edits.filter(edit => edit.expires > performance.now() + 1);
      paint();
    }, Math.max(0, Math.min(...edits.map(edit => edit.expires)) - performance.now()));
  }
}

// The live region is atomic, so every change re-reads the whole text. Announce
// once the text has settled for a moment, and at once when it is complete,
// rather than on every streamed chunk and polish pass.
const ANNOUNCE_SETTLE_MS = 1200;
let announceTimer: ReturnType<typeof setTimeout> | undefined;
function scheduleAnnouncement(): void {
  clearTimeout(announceTimer);
  const announce = () => {
    if (announcement.textContent !== cleanText) announcement.textContent = cleanText;
  };
  if (current?.phase === "complete") announce();
  else announceTimer = setTimeout(announce, ANNOUNCE_SETTLE_MS);
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
    clearTimeout(announceTimer);
    edits = [];
    cleanText = "";
    lastRaw = "";
    following = true;
    lastSize = "";
    hovering = false;
    resetClaim();
  }
  const becameComplete = current?.phase !== "complete" && snapshot.phase === "complete";
  current = snapshot;
  box.hidden = snapshot.phase === "idle";
  syncHoverPoll();
  setActivity(snapshot);
  if (snapshot.phase === "idle") {
    clearTimeout(cleanup);
    clearTimeout(announceTimer);
    edits = [];
    cleanText = lastRaw = "";
    text.replaceChildren();
    announcement.textContent = "";
    hovering = false;
    resetClaim();
    return;
  }
  if (becameComplete) holdReady = false;
  if (edited) {
    if (claimed) enableEditing();
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
  if (claimed) enableEditing();
}
// Subscribe before fetching; an initial response cannot overwrite live events.
void listen<Snapshot>("cursor-preview-state", event => {
  receivedEvent = true;
  render(event.payload);
}).then(async () => {
  const snapshot = await invoke<Snapshot>("get_cursor_preview_state");
  if (!receivedEvent) render(snapshot);
}).catch(() => { box.hidden = true; });
