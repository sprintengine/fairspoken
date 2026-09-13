import { listen } from "@tauri-apps/api/event";
import "@fontsource/inter/latin-600.css"; // brand wordmark, matching Multicode's title strip
import "./homeStats";
import "./notes";
import "./dictionary";
import "./activity";
import "./modelDashboard";
import { createSpeedTest } from "./speedTest";
import { createVoiceAccuracyTest } from "./voiceAccuracyTest";

// The home window shell: owns sidebar navigation between the four screens.
// The Settings screen's own controls are driven independently by settings.ts,
// the Home screen's stats by homeStats.ts, and the Notes library by notes.ts.

const SCREENS = ["home", "notes", "dictionary", "activity", "models", "settings"] as const;
type Screen = (typeof SCREENS)[number];

const TITLES: Record<Screen, string> = {
  home: "Home",
  notes: "Notes",
  dictionary: "Dictionary",
  activity: "Activity",
  settings: "Settings",
  models: "Models",
};

const navItems = Array.from(document.querySelectorAll<HTMLButtonElement>(".nav-item"));
const screens = Array.from(document.querySelectorAll<HTMLElement>(".screen"));
const screenTitle = document.getElementById("screenTitle");
const notesSearch = document.getElementById("notesSearch");

// The speed test is a sub-route of Home, not a nav screen: it renders into its
// own #screen-speedtest section and is opened from the Home screen.
const speedTestContainer = document.getElementById("speedTest");
const speedTest = speedTestContainer
  ? createSpeedTest({ container: speedTestContainer, onExit: () => go("home") })
  : null;

// The voice accuracy test is a second Home sub-route, mounted the same way.
const voiceTestContainer = document.getElementById("voiceTest");
const voiceTest = voiceTestContainer
  ? createVoiceAccuracyTest({ container: voiceTestContainer, onExit: () => go("home") })
  : null;

function isScreen(value: string): value is Screen {
  return (SCREENS as readonly string[]).includes(value);
}

function go(screen: Screen): void {
  speedTest?.stop();
  voiceTest?.stop();
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

// Both tests are Home sub-routes: keep the Home nav item highlighted, swap the
// visible section, and make sure the other test's controller is stopped first.
function openSubRoute(sectionId: string, title: string): void {
  speedTest?.stop();
  voiceTest?.stop();
  for (const item of navItems) {
    if (item.dataset.screen === "home") {
      item.setAttribute("aria-current", "page");
    } else {
      item.removeAttribute("aria-current");
    }
  }
  for (const section of screens) {
    section.classList.toggle("active", section.id === sectionId);
  }
  if (screenTitle) screenTitle.textContent = title;
  if (notesSearch) notesSearch.hidden = true;
}

function openSpeedTest(): void {
  if (!speedTest) return;
  openSubRoute("screen-speedtest", "Speed test");
  speedTest.start();
}

function openVoiceTest(): void {
  if (!voiceTest) return;
  openSubRoute("screen-voicetest", "Voice accuracy test");
  voiceTest.start();
}

for (const item of navItems) {
  item.addEventListener("click", () => {
    const target = item.dataset.screen;
    if (target && isScreen(target)) {
      go(target);
    }
  });
}

for (const opener of document.querySelectorAll<HTMLElement>('[data-open="speedtest"]')) {
  opener.addEventListener("click", openSpeedTest);
}

for (const opener of document.querySelectorAll<HTMLElement>('[data-open="voicetest"]')) {
  opener.addEventListener("click", openVoiceTest);
}

// The pill's gear opens this window and asks it to deep-link to a screen.
void listen<string>("home-navigate", (event) => {
  if (typeof event.payload === "string" && isScreen(event.payload)) {
    go(event.payload);
  }
}).catch(() => {
  /* navigation deep-link is best-effort; the window still opens on Home */
});
