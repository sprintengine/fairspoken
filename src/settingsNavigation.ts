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
const select = screen.querySelector<HTMLSelectElement>("#settingsCategorySelect")!;

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
  select.value = category;
  content.scrollTop = positions.get(category) ?? 0;
  updateSettingsHeading();
  document.dispatchEvent(new CustomEvent("settings-category-changed", { detail: { category } }));
}

for (const button of buttons) {
  button.addEventListener("click", () => selectSettingsCategory(button.dataset.settingsCategory!));
}
select.addEventListener("change", () => selectSettingsCategory(select.value));

// A resize can hide the navigation currently holding keyboard focus. Move that
// focus to the equivalent visible category control rather than leaving it lost.
const compact = window.matchMedia("(max-width: 999px)");
compact.addEventListener("change", () => {
  if (!screen.classList.contains("active")) return;
  if (compact.matches && buttons.includes(document.activeElement as HTMLButtonElement)) select.focus();
  else if (!compact.matches && document.activeElement === select) {
    buttons.find(button => button.dataset.settingsCategory === category)?.focus();
  }
});

selectSettingsCategory(category);
