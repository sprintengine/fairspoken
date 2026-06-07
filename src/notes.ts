import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// The durable notes library: every dictation is auto-saved server-side, and
// this screen lists, searches, edits, pins, copies, and deletes them.

interface Note {
  id: string;
  createdAt: number;
  updatedAt: number;
  text: string;
  pinned: boolean;
  durationSeconds: number;
}

const noteList = required("noteList");
const noteDetail = required("noteDetail");
const searchInput = document.getElementById("notesSearchInput") as HTMLInputElement | null;

let notes: Note[] = [];
let selectedId: string | null = null;
let query = "";
let copiedTimer: ReturnType<typeof setTimeout> | null = null;

function required(id: string): HTMLElement {
  const node = document.getElementById(id);
  if (!node) throw new Error(`Missing #${id}`);
  return node;
}

function reportError(error: unknown): void {
  console.error("notes:", error instanceof Error ? error.message : String(error));
}

function noteTitle(text: string): string {
  const firstLine = text.split("\n").map((line) => line.trim()).find((line) => line.length > 0);
  return firstLine && firstLine.length > 0 ? firstLine : "Untitled note";
}

function wordCount(text: string): number {
  const trimmed = text.trim();
  return trimmed ? trimmed.split(/\s+/).length : 0;
}

function formatDuration(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  const minutes = Math.floor(total / 60);
  return `${minutes}:${(total % 60).toString().padStart(2, "0")}`;
}

function relativeTime(ms: number): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "";
  const now = new Date();
  if (date.toDateString() === now.toDateString()) {
    return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) return "Yesterday";
  return date.toLocaleDateString([], { month: "short", day: "numeric" });
}

function timestamp(ms: number): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleString([], {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function visibleNotes(): Note[] {
  const q = query.trim().toLowerCase();
  if (!q) return notes;
  return notes.filter((note) => note.text.toLowerCase().includes(q));
}

function detailHasFocus(): boolean {
  const active = document.activeElement;
  return active instanceof HTMLElement && noteDetail.contains(active);
}

function renderList(): void {
  const shown = visibleNotes();

  if (shown.length === 0) {
    const empty = document.createElement("p");
    empty.className = "note-list-empty";
    empty.textContent = notes.length === 0 ? "No notes yet." : "No notes match your search.";
    noteList.replaceChildren(empty);
    return;
  }

  noteList.replaceChildren(
    ...shown.map((note) => {
      const row = document.createElement("button");
      row.type = "button";
      row.className = note.pinned ? "note-row pinned" : "note-row";
      row.setAttribute("role", "option");
      row.dataset.id = note.id;
      row.setAttribute("aria-selected", String(note.id === selectedId));

      const title = document.createElement("span");
      title.className = "note-title";
      title.textContent = noteTitle(note.text);

      const meta = document.createElement("span");
      meta.className = "note-meta";
      const time = document.createElement("span");
      time.className = "mono";
      time.textContent = relativeTime(note.createdAt);
      const count = wordCount(note.text);
      const words = document.createElement("span");
      words.className = "num";
      words.textContent = `${count} ${count === 1 ? "word" : "words"}`;
      meta.append(time, dot(), words);

      const pin = document.createElement("span");
      pin.className = "note-pin";
      pin.setAttribute("aria-hidden", "true");
      pin.innerHTML = pinIcon(note.pinned);

      row.append(title, meta, pin);
      row.addEventListener("click", () => {
        selectedId = note.id;
        render();
      });
      return row;
    }),
  );
}

function dot(): HTMLElement {
  const span = document.createElement("span");
  span.textContent = "·";
  return span;
}

function pinIcon(filled: boolean): string {
  return filled
    ? `<svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor"><path d="M14 3l7 7-3 1-4 4-1 5-3-3-5 5 5-5-3-3 5-1 4-4 1-3z"/></svg>`
    : `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"><path d="M14 3l7 7-3 1-4 4-1 5-3-3-5 5 5-5-3-3 5-1 4-4 1-3z" stroke-linejoin="round"/></svg>`;
}

function selectedNote(): Note | null {
  return notes.find((note) => note.id === selectedId) ?? null;
}

function renderDetail(): void {
  const note = selectedNote();

  if (!note) {
    const empty = document.createElement("div");
    empty.className = "empty";
    const title = document.createElement("div");
    title.className = "e-title";
    title.textContent = notes.length === 0 ? "No notes yet" : "Select a note";
    const sub = document.createElement("div");
    sub.className = "e-sub";
    sub.textContent =
      notes.length === 0
        ? "Your dictations are saved here automatically. Press the record shortcut to capture one."
        : "Choose a note from the list to read or edit it.";
    empty.append(title, sub);
    noteDetail.replaceChildren(empty);
    return;
  }

  const head = document.createElement("div");
  head.className = "nd-head";
  const title = document.createElement("div");
  title.className = "nd-title";
  title.textContent = noteTitle(note.text);

  const actions = document.createElement("div");
  actions.className = "nd-actions";
  actions.append(
    actionButton(note.pinned ? "Unpin" : "Pin", "btn btn-ghost btn-sm", () => togglePin(note)),
    actionButton("Delete", "btn btn-ghost btn-sm", () => removeNote(note)),
    actionButton("Copy", "btn btn-primary btn-sm", (button) => copy(note, button)),
  );
  head.append(title, actions);

  const body = document.createElement("div");
  body.className = "nd-body";
  const textarea = document.createElement("textarea");
  textarea.className = "nd-textarea";
  textarea.value = note.text;
  textarea.setAttribute("aria-label", "Note text");
  textarea.addEventListener("change", () => void saveEdit(note.id, textarea.value));
  body.append(textarea);

  const foot = document.createElement("div");
  foot.className = "nd-foot";
  foot.append(
    metaItem("Created", timestamp(note.createdAt)),
    metaItem("Words", String(wordCount(note.text))),
    metaItem("Duration", formatDuration(note.durationSeconds)),
  );

  noteDetail.replaceChildren(head, body, foot);
}

function actionButton(
  label: string,
  className: string,
  onClick: (button: HTMLButtonElement) => void,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = className;
  button.textContent = label;
  button.addEventListener("click", () => onClick(button));
  return button;
}

function metaItem(key: string, value: string): HTMLElement {
  const span = document.createElement("span");
  const k = document.createElement("span");
  k.className = "k";
  k.textContent = key;
  span.append(k, document.createTextNode(` ${value}`));
  return span;
}

function render(): void {
  renderList();
  renderDetail();
}

async function load(): Promise<void> {
  try {
    notes = await invoke<Note[]>("get_notes");
  } catch (error) {
    reportError(error);
    notes = [];
  }
  if (!notes.some((note) => note.id === selectedId)) {
    selectedId = visibleNotes()[0]?.id ?? notes[0]?.id ?? null;
  }
  render();
}

async function saveEdit(id: string, text: string): Promise<void> {
  const current = notes.find((note) => note.id === id);
  if (!current || current.text === text.trim()) return;
  if (text.trim().length === 0) {
    // Don't allow emptying a note; restore the previous text on screen.
    render();
    return;
  }
  try {
    const updated = await invoke<Note>("update_note", { id, text });
    notes = notes.map((note) => (note.id === id ? updated : note));
    render();
  } catch (error) {
    reportError(error);
    render();
  }
}

async function togglePin(note: Note): Promise<void> {
  try {
    const updated = await invoke<Note>("set_note_pinned", { id: note.id, pinned: !note.pinned });
    notes = notes.map((item) => (item.id === note.id ? updated : item));
    render();
  } catch (error) {
    reportError(error);
  }
}

async function removeNote(note: Note): Promise<void> {
  try {
    await invoke("delete_note", { id: note.id });
    notes = notes.filter((item) => item.id !== note.id);
    if (selectedId === note.id) {
      selectedId = visibleNotes()[0]?.id ?? notes[0]?.id ?? null;
    }
    render();
  } catch (error) {
    reportError(error);
  }
}

async function copy(note: Note, button: HTMLButtonElement): Promise<void> {
  try {
    await invoke("copy_note", { id: note.id });
    button.textContent = "Copied";
    if (copiedTimer !== null) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => {
      copiedTimer = null;
      button.textContent = "Copy";
    }, 1400);
  } catch (error) {
    reportError(error);
  }
}

searchInput?.addEventListener("input", () => {
  query = searchInput.value;
  if (!notes.some((note) => note.id === selectedId)) {
    selectedId = visibleNotes()[0]?.id ?? null;
  }
  render();
});

// A finished dictation is auto-saved as a note; refresh, but don't clobber an
// edit the user is in the middle of typing.
void listen("notes-updated", () => {
  if (detailHasFocus()) {
    void invoke<Note[]>("get_notes")
      .then((updated) => {
        notes = updated;
        renderList();
      })
      .catch(reportError);
    return;
  }
  void load();
}).catch(reportError);

void load();
