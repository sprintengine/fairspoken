import "./monthlyUsage.css";

export interface MonthlyBucket {
  month: string;
  words: number;
  isCurrentMonth: boolean;
}

export interface MonthlyUsageSummary {
  totalWords: number;
  months: MonthlyBucket[];
  unallocatedWords: number;
}

function monthLabel(month: string): string {
  return new Date(`${month}-01T00:00:00Z`).toLocaleDateString(undefined, {
    month: "short", year: "numeric", timeZone: "UTC",
  });
}

// Pad fixed ranges back from the backend's current UTC month. All time starts
// at the first dated activity and includes every intervening zero month.
export function selectMonths(months: MonthlyBucket[], range: number): MonthlyBucket[] {
  if (!range || !months.length) return months;
  const last = months[months.length - 1].month.split("-").map(Number);
  const end = last[0] * 12 + last[1] - 1;
  const byMonth = new Map(months.map((bucket) => [bucket.month, bucket]));
  return Array.from({ length: range }, (_, i) => {
    const index = end - range + 1 + i;
    const key = `${Math.floor(index / 12).toString().padStart(4, "0")}-${(index % 12 + 1).toString().padStart(2, "0")}`;
    return byMonth.get(key) ?? { month: key, words: 0, isCurrentMonth: false };
  });
}

export function mountMonthlyUsage(root: HTMLElement): (summary: MonthlyUsageSummary | null) => void {
  root.classList.add("monthly-usage");
  const header = document.createElement("div");
  header.className = "monthly-usage-header";
  const title = document.createElement("h2");
  title.textContent = "Words per month";
  const controls = document.createElement("div");
  controls.className = "monthly-usage-ranges";
  controls.setAttribute("role", "group");
  controls.setAttribute("aria-label", "Monthly chart time range");
  const total = document.createElement("p");
  total.className = "monthly-usage-total";
  total.setAttribute("aria-live", "polite");
  const chart = document.createElement("div");
  chart.className = "monthly-usage-scroll";
  chart.tabIndex = 0;
  chart.setAttribute("role", "group");
  chart.setAttribute("aria-label", "Monthly dictated words, scroll horizontally for more months");
  const note = document.createElement("p");
  note.className = "monthly-usage-note";
  let summary: MonthlyUsageSummary | null = null;
  let range = 6;
  const buttons = [6, 12, 0].map((value) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "ds-button ds-button--ghost";
    button.textContent = value ? `${value} months` : "All time";
    button.addEventListener("click", () => { range = value; render(); });
    controls.append(button);
    return { button, value };
  });
  header.append(title, controls);
  root.replaceChildren(header, total, chart, note);

  function render(): void {
    buttons.forEach(({ button, value }) => button.setAttribute("aria-pressed", String(range === value)));
    if (!summary) {
      total.textContent = "Monthly stats are unavailable. Try reopening Home.";
      chart.replaceChildren();
      chart.hidden = true;
      note.textContent = "";
      return;
    }
    const buckets = selectMonths(summary.months, range);
    const words = buckets.reduce((sum, bucket) => sum + bucket.words, 0);
    const period = buckets.length ? `${monthLabel(buckets[0].month)} – ${monthLabel(buckets[buckets.length - 1].month)}` : "All time";
    total.textContent = `${words.toLocaleString()} words · ${period}`;
    chart.hidden = summary.totalWords === 0;
    const list = document.createElement("ul");
    list.className = "monthly-usage-bars";
    list.setAttribute("aria-label", "Words dictated in each UTC calendar month");
    const max = Math.max(1, ...buckets.map((bucket) => bucket.words));
    for (const bucket of buckets) {
      const item = document.createElement("li");
      const label = `${monthLabel(bucket.month)}${bucket.isCurrentMonth ? " (so far)" : ""}`;
      item.setAttribute("aria-label", `${label}: ${bucket.words.toLocaleString()} words`);
      const count = document.createElement("span");
      count.className = "monthly-usage-count";
      count.textContent = bucket.words.toLocaleString();
      const track = document.createElement("div");
      track.className = "monthly-usage-track";
      const bar = document.createElement("div");
      bar.className = "monthly-usage-bar";
      bar.style.height = `${bucket.words / max * 100}%`;
      track.append(bar);
      const month = document.createElement("span");
      month.textContent = label;
      for (const node of [count, track, month]) node.setAttribute("aria-hidden", "true");
      item.append(count, track, month);
      list.append(item);
    }
    chart.replaceChildren(list);
    chart.scrollLeft = chart.scrollWidth;
    note.textContent = summary.totalWords === 0
      ? "No words dictated yet. Your monthly totals will stay here when notes are removed."
      : `${words === 0 ? "No dated words in this range. " : ""}UTC calendar months · Current month is still in progress.${summary.unallocatedWords > 0 ? ` ${summary.unallocatedWords.toLocaleString()} lifetime words have no available month and are not plotted.` : ""}`;
  }
  return (next) => { summary = next; render(); };
}
