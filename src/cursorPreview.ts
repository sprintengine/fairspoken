import "./appearance";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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
const box = document.getElementById("cursorPreview")!;
const status = document.getElementById("previewStatus")!;
const hint = document.getElementById("previewHint")!;
const text = document.getElementById("previewText")!;
let current: Snapshot | undefined;
let receivedEvent = false;
const order = { recording: 0, finishing: 1, complete: 2, idle: 3 };

function render(snapshot: Snapshot): void {
  if (current && (snapshot.sessionId < current.sessionId ||
    (snapshot.sessionId === current.sessionId && (order[snapshot.phase] < order[current.phase] ||
      (snapshot.phase === "recording" && (snapshot.revision < current.revision ||
        (snapshot.revision === current.revision && current.polished && !snapshot.polished))))))) return;
  const previous = current?.sessionId === snapshot.sessionId ? current.text : "";
  current = snapshot;
  box.hidden = snapshot.phase === "idle";
  status.textContent = snapshot.phase === "finishing" ? "Finishing" : snapshot.phase === "complete" ? "Ready" : "Listening";
  hint.textContent = snapshot.polished ? "Polished" : snapshot.remote ? "Remote" : "";
  const value = snapshot.text || (snapshot.remote ? "Recording · text arrives when you finish" : "Speak naturally…");
  if (snapshot.polished && previous && previous !== value) {
    // Mark the changed span while keeping stable text quiet. Use text nodes so
    // dictated HTML is always literal text, never interpreted markup.
    let start = 0;
    while (start < previous.length && start < value.length && previous[start] === value[start]) start++;
    let end = value.length;
    let priorEnd = previous.length;
    while (end > start && priorEnd > start && value[end - 1] === previous[priorEnd - 1]) { end--; priorEnd--; }
    const change = document.createElement("mark");
    change.className = "cursor-preview-change";
    change.textContent = value.slice(start, end);
    text.replaceChildren(document.createTextNode(value.slice(0, start)), change, document.createTextNode(value.slice(end)));
  } else text.textContent = value;
  text.scrollTop = text.scrollHeight;
}
// Register before fetching: an older snapshot must never overwrite an event.
void listen<Snapshot>("cursor-preview-state", event => {
  receivedEvent = true;
  render(event.payload);
}).then(async () => {
  const snapshot = await invoke<Snapshot>("get_cursor_preview_state");
  if (!receivedEvent) render(snapshot);
}).catch(() => { box.hidden = true; });
