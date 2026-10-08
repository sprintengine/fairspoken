import { clearEvents, readEvents, type AppEvent } from "./events";

// The Activity screen: warnings and errors worth a user's attention. Info-level
// breadcrumbs are not kept here — they go to the console (see events.ts), so this
// surface stays signal, not a log firehose. Reference: Vercel's Activity feed —
// a calm list where severity is carried by the 6px dot, not a text label or pill.

const list = document.getElementById("activityList");
const summary = document.getElementById("activitySummary");
const clearButton = document.getElementById("clearActivityButton") as HTMLButtonElement | null;

function render(): void {
  if (!list || !summary) return;

  const events = readEvents();
  summary.textContent = summaryText(events);
  if (clearButton) clearButton.disabled = events.length === 0;

  if (events.length === 0) {
    list.replaceChildren(emptyState());
    return;
  }
  list.replaceChildren(...events.map(renderRow));
}

function renderRow(event: AppEvent): HTMLElement {
  const row = document.createElement("article");
  row.className = `activity-row ${event.level}`;

  const dot = document.createElement("span");
  dot.className = "activity-dot";
  dot.setAttribute("aria-hidden", "true");

  // Severity is shown by the dot; name it for screen readers so it is not glyph-only.
  const srLevel = document.createElement("span");
  srLevel.className = "sr-only";
  srLevel.textContent = `${event.level}: `;

  const time = document.createElement("time");
  time.className = "activity-time";
  time.dateTime = event.timestamp;
  time.textContent = formatTime(event.timestamp);

  const message = document.createElement("span");
  message.className = "activity-message";
  message.textContent = event.message;

  row.append(dot, srLevel, time, message);
  return row;
}

function emptyState(): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = "empty";
  const title = document.createElement("div");
  title.className = "e-title";
  title.textContent = "No activity";
  const sub = document.createElement("div");
  sub.className = "e-sub";
  sub.textContent =
    "Warnings and errors show up here.";
  wrap.append(title, sub);
  return wrap;
}

function summaryText(events: AppEvent[]): string {
  if (events.length === 0) return "All clear";
  const errors = events.filter((event) => event.level === "error").length;
  const warnings = events.filter((event) => event.level === "warning").length;
  const parts: string[] = [];
  if (errors > 0) parts.push(`${errors} error${errors === 1 ? "" : "s"}`);
  if (warnings > 0) parts.push(`${warnings} warning${warnings === 1 ? "" : "s"}`);
  return parts.join(" · ");
}

function formatTime(timestamp: string): string {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

window.addEventListener("fairspoken-events-updated", render);
window.addEventListener("storage", (event) => {
  if (event.key === "fairspoken-events") render();
});

clearButton?.addEventListener("click", () => {
  clearEvents();
  render();
});

// Backend warnings and errors are recorded by the pill window (main.ts), which
// runs for the app's whole life; this feed picks them up through the storage
// event. Recording them here too raced the pill's read-modify-write of the log.
render();
