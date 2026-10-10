import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { accelTokens, keyGlyph, keyWord } from "./shortcuts";
import { speechModelShortName } from "./speechModels";

// The sidebar's status card: what transcribes, whether it is ready, and the
// dictation shortcut. It mirrors state the Settings form already owns: the
// model's download state (#modelDownload, from modelPreparation.ts) and the
// host or cloud check (#remoteStatus, #cloudStatus), so it never asks the
// backend anything the form has not. A click opens where that state is fixed.

type Location = "local" | "remote-host" | "cloud";
type Tone = "ok" | "busy" | "error" | "idle";
interface StatusSettings { transcriptionLocation: Location; model: string; recordingShortcut: string }

const card = document.getElementById("sidebarStatus") as HTMLButtonElement | null;
const nameLabel = document.getElementById("sidebarStatusName");
const stateLabel = document.getElementById("sidebarStatusState");
const shortcut = document.getElementById("sidebarShortcut");
const modelDownload = document.getElementById("modelDownload");
const remoteStatus = document.getElementById("remoteStatus");
const cloudStatus = document.getElementById("cloudStatus");
let settings: StatusSettings | null = null;

// Saved settings from older builds may lack a field; read what is there.
function read(value: Partial<StatusSettings> | null | undefined): StatusSettings {
  return {
    transcriptionLocation: value?.transcriptionLocation ?? "local",
    model: value?.model ?? "parakeet-tdt-0.6b-v3",
    recordingShortcut: value?.recordingShortcut ?? "",
  };
}

function localState(): [string, Tone] {
  switch (modelDownload?.dataset.state) {
    case "ready": return ["Ready", "ok"];
    case "loading": return [modelDownload.querySelector(".status")?.textContent?.startsWith("Checking") ? "Checking…" : "Preparing…", "busy"];
    case "error": return ["Needs attention", "error"];
    default: return ["Not downloaded", "idle"];
  }
}

// "studio-mac · Connected · parakeet" reads as "Connected".
function remoteState(text: string): [string, Tone] {
  if (/Connected/.test(text)) return ["Connected", "ok"];
  if (/Checking/.test(text)) return ["Checking…", "busy"];
  if (/Unavailable/.test(text)) return ["Not reachable", "error"];
  if (/Not connected/.test(text)) return ["Not set", "idle"];
  return ["Not checked", "idle"];
}

function render(): void {
  if (!card || !nameLabel || !stateLabel || !settings) return;
  const location = settings.transcriptionLocation;
  const [name, [state, tone]] = location === "remote-host"
    ? ["My host", remoteState(remoteStatus?.textContent ?? "")]
    : location === "cloud"
    ? ["Fairspoken Cloud", remoteState(cloudStatus?.textContent ?? "")]
    : [speechModelShortName(settings.model), localState()];
  nameLabel.textContent = name;
  stateLabel.textContent = state;
  card.dataset.state = tone;
  // Local problems are fixed on Models; host and cloud ones in Settings.
  card.dataset.route = location === "local" ? "models" : "settings";
  if (location === "local") delete card.dataset.settingsTarget;
  else card.dataset.settingsTarget = "transcription";

  const tokens = accelTokens(settings.recordingShortcut);
  shortcut?.replaceChildren(...tokens.map((token) => {
    const key = document.createElement("kbd");
    key.className = "ds-kbd-chord-key";
    key.textContent = keyGlyph(token);
    return key;
  }));
  card.setAttribute("aria-label", `${name}, ${state}. Dictate with ${tokens.map(keyWord).join(" ")}. Opens ${location === "local" ? "Models" : "transcription settings"}.`);
}

for (const target of [modelDownload, remoteStatus, cloudStatus]) {
  if (target) new MutationObserver(render).observe(target, { attributes: true, childList: true, characterData: true, subtree: true });
}

// Subscribe before loading so a newer save is never replaced by the first read.
let updated = false;
void listen<Partial<StatusSettings>>("settings-updated", (event) => {
  updated = true;
  settings = read(event.payload);
  render();
}).then(() => invoke<Partial<StatusSettings>>("get_settings")).then((loaded) => {
  if (updated) return;
  settings = read(loaded);
  render();
}).catch(() => { /* The settings form owns load errors; the card keeps its placeholder. */ });
