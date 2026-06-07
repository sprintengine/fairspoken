import { listen } from "@tauri-apps/api/event";
import "@fontsource/inter/latin-600.css"; // brand wordmark, matching Multicode's title strip
import "./homeStats";
import "./notes";
import "./dictionary";
import "./activity";

// The home window shell: owns sidebar navigation between the four screens.
// The Settings screen's own controls are driven independently by settings.ts,
// the Home screen's stats by homeStats.ts, and the Notes library by notes.ts.

const SCREENS = ["home", "notes", "dictionary", "activity", "settings"] as const;
type Screen = (typeof SCREENS)[number];

const TITLES: Record<Screen, string> = {
  home: "Home",
  notes: "Notes",
  dictionary: "Dictionary",
  activity: "Activity",
  settings: "Settings",
};

const navItems = Array.from(document.querySelectorAll<HTMLButtonElement>(".nav-item"));
const screens = Array.from(document.querySelectorAll<HTMLElement>(".screen"));
const screenTitle = document.getElementById("screenTitle");
const notesSearch = document.getElementById("notesSearch");

function isScreen(value: string): value is Screen {
  return (SCREENS as readonly string[]).includes(value);
}

function go(screen: Screen): void {
  for (const item of navItems) {
    if (item.dataset.screen === screen) {
      item.setAttribute("aria-current", "page");
    } else {
      item.removeAttribute("aria-current");
    }
  }
  for (const section of screens) {
    section.classList.toggle("active", section.id === `screen-${screen}`);
  }
  if (screenTitle) {
    screenTitle.textContent = TITLES[screen];
  }
  if (notesSearch) {
    notesSearch.hidden = screen !== "notes";
  }
}

for (const item of navItems) {
  item.addEventListener("click", () => {
    const target = item.dataset.screen;
    if (target && isScreen(target)) {
      go(target);
    }
  });
}

// The pill's gear opens this window and asks it to deep-link to a screen.
void listen<string>("home-navigate", (event) => {
  if (typeof event.payload === "string" && isScreen(event.payload)) {
    go(event.payload);
  }
}).catch(() => {
  /* navigation deep-link is best-effort; the window still opens on Home */
});
