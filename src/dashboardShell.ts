// Window material, local appearance preference and the shared rail tooltip.
// Navigation stays in home.ts so every route follows the same lifecycle.
import "./dashboardShell.css";

type Appearance = "system" | "light" | "dark";
const preferenceKey = "multivoice.appearance";
const systemAppearance = window.matchMedia("(prefers-color-scheme: dark)");
let appearance: Appearance = "system";
try {
  const stored = localStorage.getItem(preferenceKey);
  if (stored === "light" || stored === "dark") appearance = stored;
} catch { /* Appearance remains usable when storage is unavailable. */ }
function applyAppearance(): void {
  document.documentElement.dataset.mode = appearance === "system"
    ? systemAppearance.matches ? "dark" : "light"
    : appearance;
}
applyAppearance();
systemAppearance.addEventListener("change", applyAppearance);
if ("__TAURI_INTERNALS__" in window && /Mac/.test(navigator.platform)) {
  document.documentElement.dataset.windowMaterial = "glass";
}
const appearanceSelect = document.querySelector<HTMLSelectElement>("#appearanceSelect");
if (appearanceSelect) {
  appearanceSelect.value = appearance;
  appearanceSelect.addEventListener("change", () => {
    const next = appearanceSelect.value;
    if (next !== "system" && next !== "light" && next !== "dark") return;
    appearance = next;
    applyAppearance();
    try { localStorage.setItem(preferenceKey, appearance); } catch { /* Optional persistence. */ }
  });
}

const tooltip = document.createElement("div");
tooltip.className = "ds-tooltip";
tooltip.id = "navigation-tooltip";
tooltip.role = "tooltip";
tooltip.hidden = true;
document.body.append(tooltip);
let trigger: HTMLElement | null = null;
let timer: ReturnType<typeof setTimeout> | undefined;
function dismissTooltip(): void {
  clearTimeout(timer);
  trigger?.removeAttribute("aria-describedby");
  trigger = null;
  tooltip.hidden = true;
}
function showTooltip(button: HTMLElement): void {
  dismissTooltip();
  trigger = button;
  tooltip.textContent = button.dataset.tooltip ?? "";
  tooltip.hidden = false;
  const rect = button.getBoundingClientRect();
  const gutter = parseFloat(getComputedStyle(button).getPropertyValue("--sem-space-sm"));
  const right = rect.right + gutter;
  const left = right + tooltip.offsetWidth <= window.innerWidth - gutter
    ? right : rect.left - tooltip.offsetWidth - gutter;
  tooltip.style.setProperty("--ds-tooltip-left", `${Math.max(gutter, left)}px`);
  tooltip.style.setProperty("--ds-tooltip-top", `${Math.max(gutter, Math.min(
    rect.top + (rect.height - tooltip.offsetHeight) / 2,
    window.innerHeight - tooltip.offsetHeight - gutter,
  ))}px`);
  button.setAttribute("aria-describedby", tooltip.id);
}
for (const button of document.querySelectorAll<HTMLElement>("[data-tooltip]")) {
  button.addEventListener("pointerenter", (event) => {
    if (event.pointerType === "touch") return;
    clearTimeout(timer);
    timer = setTimeout(() => showTooltip(button), 200);
  });
  button.addEventListener("pointerleave", dismissTooltip);
  button.addEventListener("focus", () => {
    if (button.matches(":focus-visible")) showTooltip(button);
  });
  button.addEventListener("blur", dismissTooltip);
  button.addEventListener("click", dismissTooltip);
}
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") dismissTooltip();
});
document.addEventListener("scroll", dismissTooltip, true);
window.addEventListener("resize", dismissTooltip);
