export type EventLevel = "info" | "warning" | "error";

export interface AppEvent {
  id: string;
  timestamp: string;
  level: EventLevel;
  message: string;
}

const EVENT_LOG_KEY = "multivoice-tauri-events";
const MAX_EVENTS = 80;

export function addEvent(level: EventLevel, message: string): void {
  addEventWithId(`${Date.now()}-${Math.random().toString(36).slice(2)}`, level, message);
}

export function addEventWithId(id: string, level: EventLevel, message: string): void {
  const normalized = message.replace(/\s+/g, " ").trim();
  if (!normalized) return;

  // Info-level breadcrumbs are logs, not activity: they go to the console for
  // debugging and are deliberately never persisted to UI state. Only warnings
  // and errors — the things worth a user's attention — reach the Activity feed.
  if (level === "info") {
    console.info(`[multivoice] ${normalized}`);
    return;
  }
  (level === "error" ? console.error : console.warn)(`[multivoice] ${normalized}`);

  const events = readEvents();
  if (events.some((event) => event.id === id)) return;

  events.unshift({
    id,
    timestamp: new Date().toISOString(),
    level,
    message: normalized,
  });

  localStorage.setItem(EVENT_LOG_KEY, JSON.stringify(events.slice(0, MAX_EVENTS)));
  window.dispatchEvent(new CustomEvent("multivoice-events-updated"));
}

export function readEvents(): AppEvent[] {
  try {
    const raw = localStorage.getItem(EVENT_LOG_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter(isAppEvent) : [];
  } catch {
    return [];
  }
}

export function clearEvents(): void {
  localStorage.removeItem(EVENT_LOG_KEY);
  window.dispatchEvent(new CustomEvent("multivoice-events-updated"));
}

export function eventSeverity(events = readEvents()): EventLevel | null {
  if (events.some((event) => event.level === "error")) return "error";
  if (events.some((event) => event.level === "warning")) return "warning";
  return null;
}

function isAppEvent(value: unknown): value is AppEvent {
  if (!value || typeof value !== "object") return false;
  const event = value as Partial<AppEvent>;
  return (
    typeof event.id === "string" &&
    typeof event.timestamp === "string" &&
    typeof event.message === "string" &&
    (event.level === "info" || event.level === "warning" || event.level === "error")
  );
}
