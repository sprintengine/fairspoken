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
  const normalized = message.replace(/\s+/g, " ").trim();
  if (!normalized) return;

  const events = readEvents();
  events.unshift({
    id: `${Date.now()}-${Math.random().toString(36).slice(2)}`,
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
