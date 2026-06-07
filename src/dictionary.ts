import { invoke } from "@tauri-apps/api/core";

// The Dictionary screen owns the vocabulary, corrections, and snippets. These
// still live in Settings on disk, but only this screen edits them (the backend
// save_dictionary command owns those fields), so there is no clobbering with
// the Settings screen.

interface Correction {
  enabled: boolean;
  from: string;
  to: string;
  caseSensitive: boolean;
  wholePhrase: boolean;
}

interface Snippet {
  enabled: boolean;
  trigger: string;
  expansion: string;
}

interface DictionarySettings {
  vocabularyHints?: string[];
  transcriptCorrections?: Correction[];
  snippets?: Snippet[];
}

const vocabAdd = required<HTMLFormElement>("vocabAdd");
const vocabInput = required<HTMLInputElement>("vocabInput");
const vocabChips = required("vocabChips");
const addCorrectionBtn = required("addCorrection");
const correctionRows = required("correctionRows");
const addSnippetBtn = required("addSnippet");
const snippetRows = required("snippetRows");

let vocabulary: string[] = [];
let corrections: Correction[] = [];
let snippets: Snippet[] = [];

function required<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`Missing #${id}`);
  return node as T;
}

async function persist(): Promise<void> {
  try {
    await invoke("save_dictionary", {
      update: {
        vocabularyHints: vocabulary,
        transcriptCorrections: corrections,
        snippets,
      },
    });
  } catch (error) {
    console.error("dictionary:", error instanceof Error ? error.message : String(error));
  }
}

// ── Vocabulary ──────────────────────────────────────────────
function renderChips(): void {
  vocabChips.replaceChildren(
    ...vocabulary.map((word, index) => {
      const chip = document.createElement("span");
      chip.className = "chip";
      chip.append(document.createTextNode(word));

      const remove = document.createElement("button");
      remove.type = "button";
      remove.textContent = "×";
      remove.setAttribute("aria-label", `Remove ${word}`);
      remove.addEventListener("click", () => {
        vocabulary.splice(index, 1);
        renderChips();
        void persist();
      });

      chip.append(remove);
      return chip;
    }),
  );
}

vocabAdd.addEventListener("submit", (event) => {
  event.preventDefault();
  const word = vocabInput.value.trim();
  vocabInput.value = "";
  if (!word || vocabulary.some((existing) => existing.toLowerCase() === word.toLowerCase())) {
    return;
  }
  vocabulary.push(word);
  renderChips();
  void persist();
});

// ── Shared row builders ─────────────────────────────────────
function enabledToggle(checked: boolean, label: string, onChange: (value: boolean) => void): HTMLInputElement {
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = checked;
  input.setAttribute("aria-label", label);
  input.addEventListener("change", () => onChange(input.checked));
  return input;
}

function fieldInput(
  value: string,
  placeholder: string,
  onInput: (value: string) => void,
): HTMLInputElement {
  const input = document.createElement("input");
  input.type = "text";
  input.className = "setting-input";
  input.value = value;
  input.placeholder = placeholder;
  input.autocomplete = "off";
  input.addEventListener("input", () => onInput(input.value));
  input.addEventListener("change", () => void persist());
  return input;
}

function deleteButton(label: string, onClick: () => void): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "icon-btn";
  button.textContent = "×";
  button.title = label;
  button.setAttribute("aria-label", label);
  button.addEventListener("click", onClick);
  return button;
}

function arrow(): HTMLElement {
  const span = document.createElement("span");
  span.className = "dict-arrow";
  span.textContent = "→";
  span.setAttribute("aria-hidden", "true");
  return span;
}

function focusLastRowInput(container: HTMLElement): void {
  const inputs = container.querySelectorAll<HTMLInputElement>(".dict-row .setting-input");
  inputs[inputs.length - 1]?.focus();
}

// ── Corrections ─────────────────────────────────────────────
function renderCorrections(): void {
  if (corrections.length === 0) {
    correctionRows.innerHTML = '<p class="dict-rows-empty">No corrections yet.</p>';
    return;
  }
  correctionRows.replaceChildren(
    ...corrections.map((correction, index) => {
      const row = document.createElement("div");
      row.className = "dict-row";
      row.append(
        enabledToggle(correction.enabled, "Enable correction", (value) => {
          corrections[index].enabled = value;
          void persist();
        }),
        fieldInput(correction.from, "Mis-heard phrase", (value) => (corrections[index].from = value)),
        arrow(),
        fieldInput(correction.to, "Replacement", (value) => (corrections[index].to = value)),
        deleteButton("Delete correction", () => {
          corrections.splice(index, 1);
          renderCorrections();
          void persist();
        }),
      );
      return row;
    }),
  );
}

addCorrectionBtn.addEventListener("click", () => {
  corrections.push({ enabled: true, from: "", to: "", caseSensitive: false, wholePhrase: true });
  renderCorrections();
  focusLastRowInput(correctionRows);
});

// ── Snippets ────────────────────────────────────────────────
function renderSnippets(): void {
  if (snippets.length === 0) {
    snippetRows.innerHTML = '<p class="dict-rows-empty">No snippets yet.</p>';
    return;
  }
  snippetRows.replaceChildren(
    ...snippets.map((snippet, index) => {
      const row = document.createElement("div");
      row.className = "dict-row";
      row.append(
        enabledToggle(snippet.enabled, "Enable snippet", (value) => {
          snippets[index].enabled = value;
          void persist();
        }),
        fieldInput(snippet.trigger, "Trigger phrase", (value) => (snippets[index].trigger = value)),
        arrow(),
        fieldInput(snippet.expansion, "Expands to", (value) => (snippets[index].expansion = value)),
        deleteButton("Delete snippet", () => {
          snippets.splice(index, 1);
          renderSnippets();
          void persist();
        }),
      );
      return row;
    }),
  );
}

addSnippetBtn.addEventListener("click", () => {
  snippets.push({ enabled: true, trigger: "", expansion: "" });
  renderSnippets();
  focusLastRowInput(snippetRows);
});

async function load(): Promise<void> {
  try {
    const settings = await invoke<DictionarySettings>("get_settings");
    vocabulary = settings.vocabularyHints ?? [];
    corrections = settings.transcriptCorrections ?? [];
    snippets = settings.snippets ?? [];
  } catch (error) {
    console.error("dictionary:", error instanceof Error ? error.message : String(error));
  }
  renderChips();
  renderCorrections();
  renderSnippets();
}

void load();
