import "./settingsNavigation.css";

const categories = {
  general: "General",
  audio: "Audio",
  transcription: "Transcription",
  polish: "Text polish",
  keybindings: "Keybindings",
  appearance: "Appearance",
} as const;
type Category = keyof typeof categories;
let category: Category = "general";
const positions = new Map<Category, number>();
const screen = document.querySelector<HTMLElement>("#screen-settings")!;
const content = screen.querySelector<HTMLElement>(".settings-content")!;
const buttons = [...screen.querySelectorAll<HTMLButtonElement>("[data-settings-category]")];
const pages = [...screen.querySelectorAll<HTMLElement>("[data-settings-page]")];

export function updateSettingsHeading(): void {
  if (!screen.classList.contains("active")) return;
  const heading = document.getElementById("screenTitle");
  if (!heading) return;
  const parent = document.createElement("span");
  parent.className = "settings-breadcrumb-parent";
  parent.textContent = "Settings";
  const separator = document.createElement("span");
  separator.className = "settings-breadcrumb-separator";
  separator.textContent = " / ";
  separator.setAttribute("aria-hidden", "true");
  heading.replaceChildren(parent, separator, document.createTextNode(categories[category]));
}

export function selectSettingsCategory(value: string): void {
  if (!Object.prototype.hasOwnProperty.call(categories, value)) return;
  positions.set(category, content.scrollTop);
  category = value as Category;
  for (const page of pages) page.hidden = page.dataset.settingsPage !== category;
  for (const button of buttons) {
    if (button.dataset.settingsCategory === category) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  }
  content.scrollTop = positions.get(category) ?? 0;
  updateSettingsHeading();
  document.dispatchEvent(new CustomEvent("settings-category-changed", { detail: { category } }));
}

for (const button of buttons) {
  button.addEventListener("click", () => selectSettingsCategory(button.dataset.settingsCategory!));
}
selectSettingsCategory(category);
