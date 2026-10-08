import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { required } from "./dom";
import { errorMessage } from "./errors";
import { addEvent } from "./events";

// The Dictionary screen owns the vocabulary, corrections, snippets and enabled
// vocabulary packs. These still live in Settings on disk, but only this screen
// edits them (the backend save_dictionary command owns those fields), so there
// is no clobbering with the Settings screen. Entries the edit watcher learned
// carry a "Learned" chip, and fixes it will not apply on its own wait under
// Suggestions.

interface Correction {
  enabled: boolean;
  from: string;
  to: string;
  caseSensitive: boolean;
  wholePhrase: boolean;
  origin?: "manual" | "learned";
  /** Speech models a learned correction is scoped to; absent means all. */
  models?: string[];
}

interface LearnedSuggestion {
  id: string;
  heard: string;
  intended: string;
  count: number;
  reason?: string;
}

interface Snippet {
  enabled: boolean;
  trigger: string;
  expansion: string;
}

interface DictionarySettings {
  vocabularyHints?: string[];
  learnedVocabularyHints?: string[];
  transcriptCorrections?: Correction[];
  snippets?: Snippet[];
  learnFromEdits?: boolean;
  enabledPacks?: string[];
}

interface PackSource {
  name: string;
  url: string;
  licence: string;
  used_for: string;
}

interface PackSummary {
  id: string;
  name: string;
  description: string;
  version: string;
  licence: string;
  attribution: string;
  sources: PackSource[];
  termCount: number;
}

interface PackTermPreview {
  term: string;
  category: string;
  spokenForms: string[];
}

const vocabAdd = required<HTMLFormElement>("vocabAdd");
const vocabInput = required<HTMLInputElement>("vocabInput");
const vocabChips = required("vocabChips");
const addCorrectionBtn = required("addCorrection");
const correctionRows = required("correctionRows");
const addSnippetBtn = required("addSnippet");
const snippetRows = required("snippetRows");
const suggestionsSection = required("suggestionsSection");
const suggestionRows = required("suggestionRows");
const learnFromEdits = required<HTMLInputElement>("learnFromEdits");
const packRows = required("packRows");

let vocabulary: string[] = [];
let learnedVocabulary: string[] = [];
let corrections: Correction[] = [];
let snippets: Snippet[] = [];
let suggestions: LearnedSuggestion[] = [];
let packs: PackSummary[] = [];
let enabledPacks: string[] = [];

// Learned entries the screen has already shown, keyed by learnedKey(). Anything
// learned outside this set arrived while the user was typing.
let knownLearned = new Set<string>();
// The backend learned something while the user was typing in a row; reload
// when focus leaves the rows, and fold it into any save before then.
let reloadPending = false;
// Saves run one at a time, in order; a reload waits for the queue.
let saveChain: Promise<void> = Promise.resolve();
let retryTimer: ReturnType<typeof setTimeout> | undefined;
const SAVE_RETRY_MS = 3000;

function learnedKey(entry: string | Correction): string {
  return typeof entry === "string" ? `word:${entry}` : `fix:${entry.from}=>${entry.to}`;
}

function typingInRows(): boolean {
  return !!document.activeElement?.closest(".dict-row, #vocabAdd");
}

function persist(): Promise<void> {
  clearTimeout(retryTimer);
  saveChain = saveChain.then(() => save(false));
  return saveChain;
}

// The backend refuses saves while another settings operation (a dictation)
// holds the lock, so one failure is retried shortly; a second is reported and
// the screen goes back to what is actually saved.
async function save(retry: boolean): Promise<void> {
  try {
    if (reloadPending) await mergeLearned();
    await invoke("save_dictionary", {
      update: {
        vocabularyHints: vocabulary,
        learnedVocabularyHints: learnedVocabulary.filter((word) => vocabulary.includes(word)),
        transcriptCorrections: corrections,
        snippets,
        learnFromEdits: learnFromEdits.checked,
        enabledPacks,
      },
    });
  } catch (error) {
    const message = errorMessage(error);
    if (!retry) {
      addEvent("warning", `Dictionary changes not saved yet (${message}); retrying.`);
      retryTimer = setTimeout(() => { saveChain = saveChain.then(() => save(true)); }, SAVE_RETRY_MS);
    } else {
      addEvent("error", `Dictionary changes were not saved: ${message}`);
      reloadWhenIdle();
    }
  }
}

// Keep what the backend learned since the last load so a save from this screen
// does not erase it. Entries the user removed were known, so stay removed.
async function mergeLearned(): Promise<void> {
  const saved = await invoke<DictionarySettings>("get_settings");
  for (const word of saved.learnedVocabularyHints ?? []) {
    if (knownLearned.has(learnedKey(word)) || !(saved.vocabularyHints ?? []).includes(word)) continue;
    knownLearned.add(learnedKey(word));
    if (!vocabulary.includes(word)) vocabulary.push(word);
    if (!learnedVocabulary.includes(word)) learnedVocabulary.push(word);
  }
  for (const correction of saved.transcriptCorrections ?? []) {
    if (correction.origin !== "learned" || knownLearned.has(learnedKey(correction))) continue;
    knownLearned.add(learnedKey(correction));
    if (!corrections.some((existing) => learnedKey(existing) === learnedKey(correction))) corrections.push(correction);
  }
}

function reloadWhenIdle(): void {
  if (typingInRows()) reloadPending = true;
  else void saveChain.then(load);
}

// ── Vocabulary ──────────────────────────────────────────────
function renderChips(): void {
  vocabChips.replaceChildren(
    ...vocabulary.map((word, index) => {
      const chip = document.createElement("span");
      chip.className = "chip";
      chip.append(document.createTextNode(word));
      if (learnedVocabulary.includes(word)) chip.append(learnedChip("Learned from your corrections"));

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
function learnedChip(title: string): HTMLElement {
  const chip = document.createElement("span");
  chip.className = "learned-chip";
  chip.textContent = "Learned";
  chip.title = title;
  return chip;
}

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
      );
      const end = document.createElement("span");
      end.className = "dict-row-end";
      if (correction.origin === "learned") {
        const scope = correction.models?.length ? ` Applies to ${correction.models.join(", ")}.` : "";
        end.append(learnedChip(`Learned from your corrections.${scope}`));
      }
      end.append(
        deleteButton("Delete correction", () => {
          corrections.splice(index, 1);
          renderCorrections();
          void persist();
        }),
      );
      row.append(end);
      return row;
    }),
  );
}

addCorrectionBtn.addEventListener("click", () => {
  corrections.push({ enabled: true, from: "", to: "", caseSensitive: false, wholePhrase: true, origin: "manual" });
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

// ── Learning ────────────────────────────────────────────────
function renderSuggestions(): void {
  suggestionsSection.hidden = suggestions.length === 0;
  suggestionRows.replaceChildren(
    ...suggestions.map((suggestion) => {
      const row = document.createElement("div");
      row.className = "dict-suggestion";
      const text = document.createElement("div");
      text.className = "dict-suggestion-text";
      const pair = document.createElement("div");
      pair.append(document.createTextNode(suggestion.heard), arrow(), document.createTextNode(suggestion.intended));
      text.append(pair);
      if (suggestion.reason) {
        const reason = document.createElement("div");
        reason.className = "dict-suggestion-reason";
        reason.textContent = suggestion.reason;
        text.append(reason);
      }
      const accept = document.createElement("button");
      accept.type = "button";
      accept.className = "btn";
      accept.textContent = "Accept";
      accept.addEventListener("click", () => void decide("accept_learned_suggestion", suggestion.id));
      const dismiss = document.createElement("button");
      dismiss.type = "button";
      dismiss.className = "btn btn-ghost";
      dismiss.textContent = "Dismiss";
      dismiss.addEventListener("click", () => void decide("dismiss_learned_suggestion", suggestion.id));
      row.append(text, accept, dismiss);
      return row;
    }),
  );
}

// ── Packs ───────────────────────────────────────────────────
const PREVIEW_LIMIT = 80;

function packDetail(pack: PackSummary): HTMLElement {
  const detail = document.createElement("div");
  detail.className = "pack-detail";
  detail.id = `pack-detail-${pack.id}`;
  detail.hidden = true;

  const source = document.createElement("p");
  source.className = "pack-source";
  source.textContent = `Version ${pack.version}. ${pack.attribution}`;

  const search = document.createElement("input");
  search.type = "search";
  search.className = "setting-input w-lg";
  search.placeholder = "Search terms";
  search.autocomplete = "off";
  search.setAttribute("aria-label", `Search ${pack.name} terms`);

  const terms = document.createElement("div");
  terms.className = "pack-terms";
  terms.setAttribute("aria-live", "polite");

  let request = 0;
  const refresh = async (): Promise<void> => {
    const current = ++request;
    try {
      const found = await invoke<PackTermPreview[]>("search_vocabulary_pack", {
        id: pack.id,
        query: search.value.trim(),
        limit: PREVIEW_LIMIT,
      });
      if (current !== request) return;
      terms.replaceChildren(
        ...found.map((term) => {
          const chip = document.createElement("span");
          chip.className = "chip";
          chip.textContent = term.term;
          chip.title = term.spokenForms.length
            ? `${term.category}; heard as ${term.spokenForms.join(", ")}`
            : term.category;
          return chip;
        }),
      );
    } catch (error) {
      console.error("dictionary:", errorMessage(error));
    }
  };
  search.addEventListener("input", () => void refresh());
  detail.addEventListener("pack-open", () => void refresh(), { once: true });

  detail.append(source, search, terms);
  return detail;
}

function renderPacks(): void {
  if (packs.length === 0) {
    packRows.innerHTML = '<p class="dict-rows-empty">No packs in this build.</p>';
    return;
  }
  packRows.replaceChildren(
    ...packs.flatMap((pack) => {
      const row = document.createElement("div");
      row.className = "row";

      const text = document.createElement("div");
      text.className = "row-text";
      const label = document.createElement("div");
      label.className = "row-label";
      label.id = `pack-label-${pack.id}`;
      label.textContent = pack.name;
      const help = document.createElement("div");
      help.className = "row-help";
      const licences = [...new Set([pack.licence, ...pack.sources.map((s) => s.licence)])].join(", ");
      help.textContent = `${pack.description} ${pack.termCount.toLocaleString()} terms · ${licences}`;
      text.append(label, help);

      const detail = packDetail(pack);
      const browse = document.createElement("button");
      browse.type = "button";
      browse.className = "btn btn-ghost";
      browse.textContent = "Browse terms";
      browse.setAttribute("aria-expanded", "false");
      browse.setAttribute("aria-controls", detail.id);
      browse.addEventListener("click", () => {
        detail.hidden = !detail.hidden;
        browse.setAttribute("aria-expanded", String(!detail.hidden));
        if (!detail.hidden) detail.dispatchEvent(new Event("pack-open"));
      });

      const toggle = document.createElement("input");
      toggle.type = "checkbox";
      toggle.className = "switch";
      toggle.setAttribute("role", "switch");
      toggle.setAttribute("aria-labelledby", label.id);
      toggle.checked = enabledPacks.includes(pack.id);
      toggle.addEventListener("change", () => {
        enabledPacks = toggle.checked
          ? [...enabledPacks.filter((id) => id !== pack.id), pack.id]
          : enabledPacks.filter((id) => id !== pack.id);
        void persist();
      });

      const control = document.createElement("div");
      control.className = "row-control";
      control.append(browse, toggle);
      row.append(text, control);
      return [row, detail];
    }),
  );
}

async function decide(command: string, id: string): Promise<void> {
  try {
    await invoke(command, { id });
  } catch (error) {
    console.error("dictionary:", errorMessage(error));
  }
  await load();
}

learnFromEdits.addEventListener("change", () => void persist());

async function load(): Promise<void> {
  reloadPending = false;
  try {
    const [settings, learned] = await Promise.all([
      invoke<DictionarySettings>("get_settings"),
      invoke<LearnedSuggestion[]>("get_learned_suggestions"),
    ]);
    vocabulary = settings.vocabularyHints ?? [];
    learnedVocabulary = settings.learnedVocabularyHints ?? [];
    corrections = settings.transcriptCorrections ?? [];
    snippets = settings.snippets ?? [];
    learnFromEdits.checked = settings.learnFromEdits ?? true;
    suggestions = learned;
    enabledPacks = settings.enabledPacks ?? [];
    knownLearned = new Set([
      ...learnedVocabulary.map(learnedKey),
      ...corrections.filter((correction) => correction.origin === "learned").map(learnedKey),
    ]);
    packs = await invoke<PackSummary[]>("list_vocabulary_packs");
  } catch (error) {
    console.error("dictionary:", errorMessage(error));
  }
  renderChips();
  renderCorrections();
  renderSnippets();
  renderSuggestions();
  renderPacks();
}

// Learning edits the dictionary from the backend; show what it added unless
// the user is typing in a row, which a re-render would interrupt. Then it
// shows as soon as focus leaves the rows (after any save that edit queued).
void listen("learned-updated", reloadWhenIdle).catch((error) => console.error("dictionary:", errorMessage(error)));
required("screen-dictionary").addEventListener("focusout", (event) => {
  if (!reloadPending) return;
  if ((event.relatedTarget as Element | null)?.closest(".dict-row, #vocabAdd")) return;
  void saveChain.then(load);
});

void load();
