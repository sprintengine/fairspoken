import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { addEvent } from "./events";
import { required } from "./dom";
import { errorMessage } from "./errors";

// Settings → Training data: how many dictations are kept and how much disk
// they use, plus Export… and Delete all. The on/off switch and retention are
// ordinary settings saved by settings.ts.

interface TrainingDataSummary {
  count: number;
  bytes: number;
}

const summary = required("trainingDataSummary");
const exportButton = required<HTMLButtonElement>("trainingDataExport");
const deleteButton = required<HTMLButtonElement>("trainingDataDelete");

// Delete all asks for a second click instead of a dialog.
const DELETE_CONFIRM_MS = 4000;
let deleteArmedUntil = 0;
let deleteDisarmTimer: ReturnType<typeof setTimeout> | undefined;

function formatBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

async function refresh(): Promise<void> {
  try {
    const { count, bytes } = await invoke<TrainingDataSummary>("get_training_data_summary");
    summary.textContent =
      count === 0 ? "None yet." : `${count} dictation${count === 1 ? "" : "s"} · ${formatBytes(bytes)}`;
    exportButton.disabled = count === 0;
    deleteButton.disabled = count === 0;
  } catch (error) {
    summary.textContent = errorMessage(error);
  }
}

function resetDeleteButton(): void {
  clearTimeout(deleteDisarmTimer);
  deleteDisarmTimer = undefined;
  deleteArmedUntil = 0;
  deleteButton.textContent = "Delete all";
}

exportButton.addEventListener("click", async () => {
  exportButton.disabled = true;
  summary.textContent = "Exporting…";
  try {
    const path = await invoke<string>("export_training_data");
    addEvent("info", `Training data exported to ${path}`);
  } catch (error) {
    addEvent("error", errorMessage(error));
  }
  await refresh();
});

deleteButton.addEventListener("click", async () => {
  if (Date.now() > deleteArmedUntil) {
    deleteArmedUntil = Date.now() + DELETE_CONFIRM_MS;
    deleteButton.textContent = "Click again to delete";
    // One pending disarm at a time, so an old timer cannot reset a newer arm.
    clearTimeout(deleteDisarmTimer);
    deleteDisarmTimer = setTimeout(resetDeleteButton, DELETE_CONFIRM_MS);
    return;
  }
  resetDeleteButton();
  try {
    await invoke("delete_training_data");
    addEvent("info", "Kept dictations deleted");
  } catch (error) {
    addEvent("error", errorMessage(error));
  }
  await refresh();
});

// A finished dictation may have been kept; settings changes may prune.
void listen("transcript-history-updated", () => void refresh());
void listen("settings-updated", () => void refresh());
window.addEventListener("focus", () => void refresh());

void refresh();
