import "./settingsNavigation.css";

// The tab strip under the Settings title: one page of the form at a time.
const categories = ["general", "keybindings", "audio", "transcription", "polish", "appearance", "updates"] as const;
type Category = (typeof categories)[number];
let category: Category = "general";
const positions = new Map<Category, number>();
const screen = document.querySelector<HTMLElement>("#screen-settings")!;
const content = screen.querySelector<HTMLElement>(".settings-content")!;
const buttons = [...screen.querySelectorAll<HTMLButtonElement>("[data-settings-category]")];
const pages = [...screen.querySelectorAll<HTMLElement>("[data-settings-page]")];

export function selectSettingsCategory(value: string): void {
  if (!(categories as readonly string[]).includes(value)) return;
  positions.set(category, content.scrollTop);
  category = value as Category;
  for (const page of pages) page.hidden = page.dataset.settingsPage !== category;
  for (const button of buttons) {
    if (button.dataset.settingsCategory === category) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  }
  content.scrollTop = positions.get(category) ?? 0;
  document.dispatchEvent(new CustomEvent("settings-category-changed", { detail: { category } }));
}

for (const button of buttons) {
  button.addEventListener("click", () => {
    selectSettingsCategory(button.dataset.settingsCategory!);
    // Keep the chosen tab in view when the strip is scrolled at narrow widths.
    button.scrollIntoView({ block: "nearest", inline: "nearest" });
  });
}
selectSettingsCategory(category);
