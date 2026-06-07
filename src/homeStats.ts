import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Renders the Home screen from the persistent usage_stats aggregate. Numbers
// here are real: time saved compares typing the dictated words at 40 wpm
// against the actual recording time, and the speaking pace is measured.

interface WeeklyBucket {
  dayLabel: string;
  words: number;
  isToday: boolean;
}

interface UsageStatsSummary {
  hasData: boolean;
  totalWords: number;
  totalDictations: number;
  totalRecordingSeconds: number;
  timeSavedSeconds: number;
  speakingWpm: number;
  currentStreak: number;
  bestStreak: number;
  thisWeekWords: number;
  lastWeekWords: number;
  week: WeeklyBucket[];
}

interface TranscriptHistoryItem {
  id: string;
  createdAt: number;
  text: string;
  durationSeconds: number;
}

const TYPING_WPM = 40;
const RECENT_LIMIT = 5;

const homeEmpty = byId("homeEmpty");
const homeStats = byId("homeStats");
const statTimeSaved = byId("statTimeSaved");
const statTimeSavedSub = byId("statTimeSavedSub");
const statTotalWords = byId("statTotalWords");
const statThisWeek = byId("statThisWeek");
const statWeekDelta = byId("statWeekDelta");
const statStreak = byId("statStreak");
const statBestStreak = byId("statBestStreak");
const statWpm = byId("statWpm");
const statChart = byId("statChart");
const statRecent = byId("statRecent");
const statViewAll = document.getElementById("statViewAll");

function byId(id: string): HTMLElement {
  const node = document.getElementById(id);
  if (!node) throw new Error(`Missing #${id}`);
  return node;
}

function formatDuration(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  if (total < 60) return "<1 m";
  const hours = Math.floor(total / 3600);
  const minutes = Math.round((total % 3600) / 60);
  if (hours > 0) return `${hours} h ${minutes} m`;
  return `${minutes} m`;
}

function weekDeltaLabel(thisWeek: number, lastWeek: number): string {
  if (lastWeek === 0) return thisWeek > 0 ? "first active week" : "no words yet";
  const pct = Math.round(((thisWeek - lastWeek) / lastWeek) * 100);
  const sign = pct > 0 ? "+" : "";
  return `${sign}${pct}% vs last week`;
}

function renderChart(week: WeeklyBucket[]): void {
  const max = Math.max(1, ...week.map((bucket) => bucket.words));
  statChart.replaceChildren(
    ...week.map((bucket) => {
      const col = document.createElement("div");
      col.className = bucket.isToday ? "bar-col is-today" : "bar-col";

      const bar = document.createElement("div");
      bar.className = bucket.isToday ? "bar today" : "bar";
      bar.style.height = `${(bucket.words / max) * 100}%`;
      bar.title = `${bucket.words.toLocaleString()} words`;

      const day = document.createElement("div");
      day.className = "bar-day";
      day.textContent = bucket.dayLabel;

      col.append(bar, day);
      return col;
    }),
  );
}

function renderStats(summary: UsageStatsSummary): void {
  if (!summary.hasData) {
    homeEmpty.hidden = false;
    homeStats.hidden = true;
    return;
  }
  homeEmpty.hidden = true;
  homeStats.hidden = false;

  statTimeSaved.textContent = formatDuration(summary.timeSavedSeconds);
  const typingSeconds = (summary.totalWords / TYPING_WPM) * 60;
  statTimeSavedSub.textContent =
    `Typing your ${summary.totalWords.toLocaleString()} words at ${TYPING_WPM} wpm would take about ` +
    `${formatDuration(typingSeconds)}; dictating them took ${formatDuration(summary.totalRecordingSeconds)}.`;

  statTotalWords.textContent = summary.totalWords.toLocaleString();
  statThisWeek.textContent = summary.thisWeekWords.toLocaleString();
  statWeekDelta.textContent = weekDeltaLabel(summary.thisWeekWords, summary.lastWeekWords);
  statStreak.textContent = String(summary.currentStreak);
  statBestStreak.textContent = `best ${summary.bestStreak}`;
  statWpm.textContent = String(summary.speakingWpm);

  renderChart(summary.week);
}

function relativeTime(createdAtMs: number): string {
  const date = new Date(createdAtMs);
  if (Number.isNaN(date.getTime())) return "";
  const now = new Date();
  const sameDay = date.toDateString() === now.toDateString();
  if (sameDay) return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) return "Yesterday";
  return date.toLocaleDateString([], { month: "short", day: "numeric" });
}

function wordCount(text: string): number {
  const trimmed = text.trim();
  return trimmed ? trimmed.split(/\s+/).length : 0;
}

function renderRecent(items: TranscriptHistoryItem[]): void {
  if (items.length === 0) {
    const empty = document.createElement("p");
    empty.className = "act-empty";
    empty.textContent = "No dictations yet.";
    statRecent.replaceChildren(empty);
    return;
  }

  statRecent.replaceChildren(
    ...items.slice(0, RECENT_LIMIT).map((item) => {
      const row = document.createElement("div");
      row.className = "act-row";

      const text = document.createElement("span");
      text.className = "act-text";
      text.textContent = item.text;
      text.title = item.text;

      const time = document.createElement("span");
      time.className = "act-time";
      time.textContent = relativeTime(item.createdAt);

      const words = document.createElement("span");
      words.className = "act-words num";
      const count = wordCount(item.text);
      words.textContent = `${count} ${count === 1 ? "word" : "words"}`;

      row.append(text, time, words);
      return row;
    }),
  );
}

async function refresh(): Promise<void> {
  let summary: UsageStatsSummary;
  try {
    summary = await invoke<UsageStatsSummary>("get_usage_stats");
  } catch {
    // No stats available yet (or running outside the app): show the welcome
    // state rather than inventing numbers.
    renderStats(emptyStats());
    return;
  }

  renderStats(summary);
  if (!summary.hasData) return;

  // The recent list is secondary: a failure here must not blank the stats
  // that already loaded.
  try {
    const history = await invoke<TranscriptHistoryItem[]>("get_transcript_history");
    renderRecent(history);
  } catch {
    renderRecent([]);
  }
}

function emptyStats(): UsageStatsSummary {
  return {
    hasData: false,
    totalWords: 0,
    totalDictations: 0,
    totalRecordingSeconds: 0,
    timeSavedSeconds: 0,
    speakingWpm: 0,
    currentStreak: 0,
    bestStreak: 0,
    thisWeekWords: 0,
    lastWeekWords: 0,
    week: [],
  };
}

statViewAll?.addEventListener("click", () => {
  document.querySelector<HTMLButtonElement>('.nav-item[data-screen="notes"]')?.click();
});

// A finished dictation changes the aggregate; refresh when one lands.
void listen("transcript-history-updated", () => {
  void refresh();
}).catch(() => {
  /* live refresh is best-effort; the initial load still populates */
});

void refresh();
