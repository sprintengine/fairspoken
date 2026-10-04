import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Settings > General > Updates. The backend (src-tauri/src/updates.rs) checks
// on its own schedule and downloads in the background; this row reports where
// that stands and lets the user check now or restart into a downloaded update.

type UpdateState =
  | { state: "disabled" }
  | { state: "idle" }
  | { state: "checking" }
  | { state: "upToDate" }
  | { state: "downloading"; version: string; downloaded: number; total: number | null }
  | { state: "ready"; version: string }
  | { state: "failed"; message: string };

type UpdateStatus = UpdateState & { currentVersion: string; channel: "stable" | "nightly" };

const versionLabel = document.querySelector<HTMLElement>("#appVersion")!;
const statusLabel = document.querySelector<HTMLElement>("#updateStatus")!;
const action = document.querySelector<HTMLButtonElement>("#updateAction")!;
let current: UpdateStatus | null = null;

function describe(status: UpdateStatus): { text: string; button: string | null; busy: boolean } {
  switch (status.state) {
    case "disabled":
      return { text: "Development builds don't update themselves.", button: null, busy: false };
    case "idle":
      return { text: "Checks for updates automatically.", button: "Check for updates", busy: false };
    case "checking":
      return { text: "Checking for updates…", button: "Check for updates", busy: true };
    case "upToDate":
      return { text: "You're on the latest version.", button: "Check for updates", busy: false };
    case "downloading": {
      const percent = status.total ? ` ${Math.floor((status.downloaded / status.total) * 100)}%` : "";
      return { text: `Downloading ${status.version}…${percent}`, button: "Check for updates", busy: true };
    }
    case "ready":
      return { text: `Version ${status.version} is ready to install.`, button: "Restart to update", busy: false };
    case "failed":
      return { text: status.message, button: "Check for updates", busy: false };
  }
}

function render(status: UpdateStatus): void {
  current = status;
  versionLabel.textContent = status.currentVersion;
  const { text, button, busy } = describe(status);
  statusLabel.textContent = text;
  action.hidden = button === null;
  action.textContent = button ?? "";
  action.disabled = busy;
}

action.addEventListener("click", () => {
  action.disabled = true;
  const request =
    current?.state === "ready"
      ? invoke("install_update")
      : invoke<UpdateStatus>("check_for_updates").then(render);
  request.catch((error: unknown) => {
    statusLabel.textContent = String(error);
    action.disabled = false;
  });
});

void listen<UpdateStatus>("update-status", (event) => render(event.payload));
void invoke<UpdateStatus>("get_update_status").then(render);
