import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { mountMonthlyUsage, type MonthlyBucket } from "./monthlyUsage";

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
  zeroEditRate7d?: number | null;
  zeroEditRate30d?: number | null;
  polishedZeroEditRate30d?: number | null;
  rawZeroEditRate30d?: number | null;
  totalWords: number;
  totalDictations: number;
  totalRecordingSeconds: number;
  timeSavedSeconds: number;
  typingWpm: number;
  speakingWpm: number;
  moneySavedUsd: number;
  cloudRateUsdPerMinute: number;
  currentStreak: number;
  bestStreak: number;
  thisWeekWords: number;
  lastWeekWords: number;
  week: WeeklyBucket[];
  months: MonthlyBucket[];
  unallocatedWords: number;
}

const TYPING_WPM = 40;

const homeEmpty = byId("homeEmpty");
const homeStats = byId("homeStats");
const statTimeSaved = byId("statTimeSaved");
const statTimeSavedSub = byId("statTimeSavedSub");
const statTotalWords = byId("statTotalWords");
const statSpokenTime = byId("statSpokenTime");
const statThisWeek = byId("statThisWeek");
const statWeekDelta = byId("statWeekDelta");
const statStreak = byId("statStreak");
const statBestStreak = byId("statBestStreak");
const statWpm = byId("statWpm");
const statMoneySaved = byId("statMoneySaved");
const statMoneySavedMeta = byId("statMoneySavedMeta");
const statZeroEditTile = byId("statZeroEditTile");
const statZeroEdit = byId("statZeroEdit");
const statZeroEditMeta = byId("statZeroEditMeta");
const statChart = byId("statChart");
const renderMonthlyUsage = mountMonthlyUsage(byId("monthlyUsage"));

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

function formatMoney(usd: number): string {
  if (usd > 0 && usd < 0.005) return "<$0.01";
  return `$${usd.toFixed(2)}`;
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
  renderMonthlyUsage(summary);
  if (!summary.hasData) {
    homeEmpty.hidden = false;
    homeStats.hidden = true;
    return;
  }
  homeEmpty.hidden = true;
  homeStats.hidden = false;

  statTimeSaved.textContent = formatDuration(summary.timeSavedSeconds);
  const typingWpm = Math.round(summary.typingWpm) || TYPING_WPM;
  const typingSeconds = (summary.totalWords / typingWpm) * 60;
  statTimeSavedSub.textContent =
    `Typing your ${summary.totalWords.toLocaleString()} words at ${typingWpm} wpm would take about ` +
    `${formatDuration(typingSeconds)}; dictating them took ${formatDuration(summary.totalRecordingSeconds)}.`;

  statTotalWords.textContent = summary.totalWords.toLocaleString();
  statSpokenTime.textContent = formatDuration(summary.totalRecordingSeconds);
  statThisWeek.textContent = summary.thisWeekWords.toLocaleString();
  statWeekDelta.textContent = weekDeltaLabel(summary.thisWeekWords, summary.lastWeekWords);
  statStreak.textContent = String(summary.currentStreak);
  statBestStreak.textContent = `best ${summary.bestStreak}`;
  statWpm.textContent = String(summary.speakingWpm);
  statMoneySaved.textContent = formatMoney(summary.moneySavedUsd);
  statMoneySavedMeta.textContent =
    `vs cloud transcription ($${summary.cloudRateUsdPerMinute.toFixed(3)}/min)`;

  renderZeroEdit(summary);
  renderChart(summary.week);
}

// The epic's north-star metric: dictations that needed no follow-up within
// 5 minutes. Hidden until 20 completed dictations exist in the window; the
// polished/raw split appears once each arm has enough data.
function renderZeroEdit(summary: UsageStatsSummary): void {
  const rate = summary.zeroEditRate30d;
  if (rate === null || rate === undefined) {
    statZeroEditTile.hidden = true;
    return;
  }
  statZeroEditTile.hidden = false;
  statZeroEdit.textContent = `${Math.round(rate * 100)}%`;
  const polished = summary.polishedZeroEditRate30d;
  const raw = summary.rawZeroEditRate30d;
  statZeroEditMeta.textContent =
    polished !== null && polished !== undefined && raw !== null && raw !== undefined
      ? `polished ${Math.round(polished * 100)}% · raw ${Math.round(raw * 100)}% (30 days)`
      : "last 30 days";
}

async function refresh(): Promise<void> {
  let summary: UsageStatsSummary;
  try {
    summary = await invoke<UsageStatsSummary>("get_usage_stats");
  } catch {
    // No stats available yet (or running outside the app): show the welcome
    // state rather than inventing numbers.
    renderStats(emptyStats());
    renderMonthlyUsage(null);
    return;
  }

  renderStats(summary);
}

function emptyStats(): UsageStatsSummary {
  return {
    hasData: false,
    totalWords: 0,
    totalDictations: 0,
    totalRecordingSeconds: 0,
    timeSavedSeconds: 0,
    typingWpm: TYPING_WPM,
    speakingWpm: 0,
    moneySavedUsd: 0,
    cloudRateUsdPerMinute: 0.006,
    currentStreak: 0,
    bestStreak: 0,
    thisWeekWords: 0,
    lastWeekWords: 0,
    week: [],
    months: [],
    unallocatedWords: 0,
  };
}

// A finished dictation changes the aggregate; refresh when one lands.
void listen("transcript-history-updated", () => {
  void refresh();
}).catch(() => {
  /* live refresh is best-effort; the initial load still populates */
});

// Setting a measured typing speed in the speed test changes time saved.
void listen("usage-stats-updated", () => {
  void refresh();
}).catch(() => {
  /* best-effort */
});

void refresh();

// Refresh the current calendar month when returning to Home after a long session.
document.querySelector('.nav-item[data-screen="home"]')?.addEventListener("click", () => { void refresh(); });
