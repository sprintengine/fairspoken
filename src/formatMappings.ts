import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { addEvent } from "./events";
import { required } from "./dom";
import { errorMessage } from "./errors";

// The polish formats (docs/polish-input.md) and the site/app mappings the
// format decision reads: labels the classifier learned, and the user's own.
// Only a key and a label are stored, never a title or a URL path.
type Format = "email" | "chat" | "document" | "notes" | "code" | "plain";

interface LearnedFormat {
  key: string;
  format: Format;
  source: "llm" | "user";
  updatedAt: number;
}

const FORMAT_LABELS: Record<Format, string> = {
  email: "Email",
  chat: "Chat",
  document: "Document",
  notes: "Notes",
  code: "Code",
  plain: "Plain",
};

const rows = required<HTMLElement>("formatMappingRows");
const setButton = required<HTMLButtonElement>("formatMappingSet");
const keyInput = required<HTMLInputElement>("formatMappingKey");
const formatSelect = required<HTMLSelectElement>("formatMappingFormat");

function report(error: unknown): void {
  addEvent("error", errorMessage(error));
}

function formatOptions(select: HTMLSelectElement, selected: Format): void {
  select.replaceChildren(
    ...(Object.keys(FORMAT_LABELS) as Format[]).map((format) => {
      const option = new Option(FORMAT_LABELS[format], format);
      option.selected = format === selected;
      return option;
    }),
  );
}

// "site:example.com" reads as "example.com", "app:com.vendor.App" as the id.
function keyLabel(key: string): string {
  return key.replace(/^(site|app):/, "");
}

function render(mappings: LearnedFormat[]): void {
  if (mappings.length === 0) {
    rows.innerHTML = '<p class="dict-rows-empty">No sites or apps yet.</p>';
    return;
  }
  rows.replaceChildren(
    ...mappings.map((mapping) => {
      const row = document.createElement("div");
      row.className = "dict-row format-mapping-row";
      const name = document.createElement("span");
      name.className = "format-mapping-key";
      name.textContent = keyLabel(mapping.key);
      name.title = mapping.key.startsWith("app:") ? "App" : "Website";
      const source = document.createElement("span");
      source.className = "format-mapping-source";
      source.textContent = mapping.source === "user" ? "Set by you" : "Detected";
      const select = document.createElement("select");
      select.className = "setting-select w-sm";
      select.setAttribute("aria-label", `Format for ${keyLabel(mapping.key)}`);
      formatOptions(select, mapping.format);
      select.addEventListener("change", () => {
        void invoke<LearnedFormat[]>("set_format_mapping", { key: mapping.key, format: select.value })
          .then(render)
          .catch(report);
      });
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "btn btn-ghost btn-sm";
      remove.textContent = "Remove";
      remove.setAttribute("aria-label", `Remove ${keyLabel(mapping.key)}`);
      remove.addEventListener("click", () => {
        void invoke<LearnedFormat[]>("remove_format_mapping", { key: mapping.key })
          .then(render)
          .catch(report);
      });
      row.append(name, source, select, remove);
      return row;
    }),
  );
}

export function refreshFormatMappings(): void {
  void invoke<LearnedFormat[]>("get_format_mappings").then(render).catch(report);
}

formatOptions(formatSelect, "document");
// The add row lives inside the settings <form>, where a nested <form> would be
// dropped by the HTML parser, so it is a plain group with its own Enter key.
function addMapping(): void {
  const key = keyInput.value.trim();
  if (!key) { keyInput.focus(); return; }
  void invoke<LearnedFormat[]>("set_format_mapping", { key, format: formatSelect.value })
    .then((mappings) => {
      keyInput.value = "";
      render(mappings);
    })
    .catch(report);
}
setButton.addEventListener("click", addMapping);
keyInput.addEventListener("keydown", (event) => {
  if (event.key !== "Enter") return;
  event.preventDefault();
  addMapping();
});

refreshFormatMappings();
// A dictation may have taught a new site or app.
void listen("transcript-history-updated", refreshFormatMappings).catch(report);
document.addEventListener("settings-category-changed", refreshFormatMappings);
