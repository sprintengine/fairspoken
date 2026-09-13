import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createPublisherIcon } from "./publisherIcons";

type ModelSettings = { model: string };
function render(settings: ModelSettings): void {
  for (const [id, publisher] of [
    ["speechModelIdentity", settings.model.startsWith("parakeet") ? "NVIDIA" : "OpenAI"],
    ["polishModelIdentity", "Qwen"],
  ]) {
    const host = document.getElementById(id);
    if (!host) continue;
    host.querySelector(".publisher-icon")?.remove();
    host.prepend(createPublisherIcon(publisher));
  }
}

// Subscribe before loading so a model change can't be replaced by an older
// initial response. Names are maintained by the settings form controller.
let updated = false;
void listen<ModelSettings>("settings-updated", event => {
  updated = true;
  render(event.payload);
}).then(() => invoke<ModelSettings>("get_settings")).then(settings => {
  if (!updated) render(settings);
}).catch(() => { /* The settings form owns load/save error feedback. */ });
