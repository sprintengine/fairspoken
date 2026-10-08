import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { required } from "./dom";
import { errorMessage } from "./errors";
import { addEvent } from "./events";
import type { ModelPrepareProgressEvent, ModelStatus, SttModel } from "./settingsSchema";

// Settings → Transcription: the local model's download state under the Model
// row — checking the cache, preparing (download, unpack, validate) and the
// one Prepare / Retry action. Remote and cloud transcription skip all of it.

const modelSelect = required<HTMLSelectElement>("modelSelect");
const modelDownload = required<HTMLElement>("modelDownload");
const modelDownloadStatus = required<HTMLElement>("modelDownloadStatus");
const modelDownloadBar = required<HTMLElement>("modelDownloadBar");
const modelSize = required<HTMLElement>("modelSize");
const modelPrepare = required<HTMLButtonElement>("modelPrepare");

let host: { location(): string; reportError(error: unknown): void };

const MODEL_MEMORY_FOOTPRINTS: Record<SttModel, string> = {
  "parakeet-tdt-0.6b-v3": "RAM ~2.5G",
  "parakeet-ultra": "RAM ~2.5G",
  "parakeet-tdt-0.6b-v2": "RAM ~2.5G · English",
  tiny: "RAM ~0.7G",
  base: "RAM ~1.2G",
  small: "RAM ~2.5G",
  medium: "RAM ~8G",
  "large-v2": "RAM ~16G",
  "large-v3": "RAM ~16G",
  "large-v3-turbo": "RAM ~8G",
};

function setModelDownloadStatus(status: string, percentage: number): void {
  const isError = percentage < 0;
  const clamped = isError ? 0 : Math.max(0, Math.min(100, Math.round(percentage)));

  modelDownload.dataset.state = isError ? "error" : clamped >= 100 ? "ready" : clamped > 0 ? "loading" : "idle";
  modelDownloadStatus.textContent = isError ? status : `${status} ${clamped}%`;
  modelDownloadBar.style.transform = `scaleX(${clamped / 100})`;
}

function setModelCacheStatus(status: ModelStatus): void {
  modelDownload.dataset.state = status.cached ? "ready" : "idle";
  modelDownloadStatus.textContent = formatModelStatus(status);
  modelDownloadStatus.title = status.message;
  modelDownloadBar.style.transform = `scaleX(${status.cached ? 1 : 0})`;
  modelPrepare.textContent = status.cached ? "Ready" : "Prepare";
  modelPrepare.disabled = status.cached;
}

function formatModelStatus(status: ModelStatus): string {
  if (status.cached) return "Downloaded";
  if (/checksum|failed/i.test(status.message)) return "Cache needs repair";
  return "Not downloaded";
}

export async function requestModelStatus(): Promise<void> {
  if (host.location() !== "local") {
    updateModelSize(modelSelect.value as SttModel);
    return;
  }
  const model = modelSelect.value as SttModel;
  updateModelSize(model);
  modelPrepare.textContent = "Checking";
  modelPrepare.disabled = true;
  modelDownload.dataset.state = "loading";
  modelDownloadStatus.textContent = "Checking model...";
  modelDownloadBar.style.transform = "scaleX(0.01)";
  delete modelPrepare.dataset.action;
  let status: ModelStatus;
  try {
    status = await invoke<ModelStatus>("get_transcription_model_status", {
      request: { model },
    });
  } catch (error) {
    // Never leave Prepare disabled on "Checking": offer the check again.
    const message = errorMessage(error);
    addEvent("warning", message);
    modelDownload.dataset.state = "error";
    modelDownloadStatus.textContent = "Couldn't check the model";
    modelDownloadStatus.title = message;
    modelDownloadBar.style.transform = "scaleX(0)";
    modelPrepare.textContent = "Retry";
    modelPrepare.dataset.action = "check";
    modelPrepare.disabled = false;
    return;
  }
  addEvent(status.cached ? "info" : "warning", status.message);
  setModelCacheStatus(status);
}

async function beginModelPreload(): Promise<void> {
  if (host.location() !== "local") return;
  const model = modelSelect.value as SttModel;
  updateModelSize(model);
  setModelDownloadStatus("Preparing model...", 1);
  modelPrepare.textContent = "Preparing";
  modelPrepare.disabled = true;
  try {
    await invoke("begin_prepare_transcription_model", {
      request: { model },
    });
  } catch (error) {
    const message = errorMessage(error);
    addEvent("error", errorMessage(error));
    modelDownload.dataset.state = "error";
    modelDownloadStatus.textContent = "Download failed";
    modelDownloadStatus.title = message;
    modelPrepare.textContent = "Retry";
    modelPrepare.disabled = false;
  }
}

function handleModelPrepareProgress(event: ModelPrepareProgressEvent): void {
  if (event.model !== modelSelect.value) return;
  delete modelPrepare.dataset.action;

  if (event.error) {
    addEvent("error", event.error);
    modelDownload.dataset.state = "error";
    modelDownloadStatus.textContent = "Preparation failed";
    modelDownloadStatus.title = event.error;
    modelDownloadBar.style.transform = "scaleX(0)";
    modelPrepare.textContent = "Retry";
    modelPrepare.disabled = false;
    return;
  }

  if (event.status && event.done) {
    addEvent("info", event.status.message);
    setModelCacheStatus(event.status);
    return;
  }

  setModelDownloadStatus(formatPrepareStage(event.stage, event.message), event.percentage);
  modelPrepare.textContent = event.done ? "Ready" : "Preparing";
  modelPrepare.disabled = !event.done;
}

function formatPrepareStage(stage: string, message: string): string {
  if (stage === "downloading") return message || "Downloading model";
  if (stage === "unpacking") return "Unpacking model";
  if (stage === "validating") return "Validating model";
  if (stage === "ready") return "Model ready";
  return message || "Preparing model";
}

export function updateModelSize(model: SttModel): void {
  modelSize.textContent = MODEL_MEMORY_FOOTPRINTS[model] ?? "";
}

// Settings could not load at all: say so where the model state would be.
export function showSettingsLoadError(message: string): void {
  modelDownload.dataset.state = "error";
  modelDownloadStatus.textContent = "Settings failed to load";
  modelDownloadStatus.title = message;
}

export function initModelPreparation(formHost: { location(): string; reportError(error: unknown): void }): void {
  host = formHost;
  modelPrepare.addEventListener("click", () => {
    // After a failed status check, Retry checks again rather than downloading.
    if (modelPrepare.dataset.action === "check") void requestModelStatus().catch(host.reportError);
    else void beginModelPreload();
  });
  void listen<ModelPrepareProgressEvent>("model-prepare-progress", (event) => {
    handleModelPrepareProgress(event.payload);
  }).catch(host.reportError);
}
