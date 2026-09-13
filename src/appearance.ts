import { emit, listen } from "@tauri-apps/api/event";

type Appearance = "system" | "light" | "dark";
const preferenceKey = "multivoice.appearance";
const system = window.matchMedia("(prefers-color-scheme: dark)");
let appearance: Appearance = "system";
function valid(value: unknown): value is Appearance {
  return value === "system" || value === "light" || value === "dark";
}
try {
  const stored = localStorage.getItem(preferenceKey);
  if (valid(stored)) appearance = stored;
} catch { /* The current window remains usable without local storage. */ }
function apply(): void {
  document.documentElement.dataset.mode = appearance === "system" ? system.matches ? "dark" : "light" : appearance;
  for (const button of document.querySelectorAll<HTMLButtonElement>("button[data-appearance]")) {
    button.setAttribute("aria-pressed", String(button.dataset.appearance === appearance));
  }
  const status = document.getElementById("appearanceStatus");
  if (status) status.textContent = appearance === "system" ? "Follows your system appearance." : `${appearance === "dark" ? "Dark" : "Light"} appearance is selected.`;
}
function choose(next: Appearance, broadcast = false): void {
  appearance = next;
  apply();
  try { localStorage.setItem(preferenceKey, next); } catch {
    const status = document.getElementById("appearanceStatus");
    if (status) status.textContent = "Appearance changed for this session; this window could not save the preference.";
  }
  if (broadcast && "__TAURI_INTERNALS__" in window) void emit("appearance-changed", next).catch(() => {});
}
apply();
system.addEventListener("change", apply);
for (const button of document.querySelectorAll<HTMLButtonElement>("button[data-appearance]")) {
  button.addEventListener("click", () => { if (valid(button.dataset.appearance)) choose(button.dataset.appearance, true); });
}
window.addEventListener("storage", (event) => {
  if (event.key === preferenceKey) choose(valid(event.newValue) ? event.newValue : "system");
});
if ("__TAURI_INTERNALS__" in window) {
  void listen<Appearance>("appearance-changed", ({ payload }) => { if (valid(payload)) choose(payload); }).catch(() => {});
}
