import { invoke } from "@tauri-apps/api/core";
import { addEvent } from "./events";
import { errorMessage } from "./errors";

/* ── AI polish undo affordance ─────────────────────────────────
   After a polished transcript pastes, the pill shows "AI polished" with an
   Undo button for 8 s (or until the next recording). Undo copies the RAW
   transcript to the clipboard — deliberately not a synthetic ⌘Z+⌘V or AX
   replacement, both rejected as fragile. This module owns the offer and its
   window; main.ts decides what the pill shows. */

const POLISH_UNDO_WINDOW_MS = 8_000;
let polishUndoOffer: { id: string } | null = null;
let polishUndoTimer: ReturnType<typeof setTimeout> | null = null;

export function polishUndoOffered(): boolean {
  return polishUndoOffer !== null;
}

/** Offer Undo for this transcript; `onExpire` runs if the window lapses. */
export function offerPolishUndo(id: string, onExpire: () => void): void {
  polishUndoOffer = { id };
  if (polishUndoTimer !== null) clearTimeout(polishUndoTimer);
  polishUndoTimer = setTimeout(() => {
    dropPolishUndoOffer();
    onExpire();
  }, POLISH_UNDO_WINDOW_MS);
}

/** Withdraw the offer without touching the pill state (callers decide). */
export function dropPolishUndoOffer(): void {
  if (polishUndoTimer !== null) {
    clearTimeout(polishUndoTimer);
    polishUndoTimer = null;
  }
  polishUndoOffer = null;
}

/** Take the offer and copy its original transcript: "none" when nothing was
 *  offered, otherwise whether the copy worked. */
export async function copyOriginalTranscript(): Promise<"none" | "copied" | "failed"> {
  const offer = polishUndoOffer;
  if (offer === null) return "none";
  dropPolishUndoOffer();
  try {
    await invoke("copy_original_transcript", { id: offer.id });
    addEvent("info", "Original transcript copied — paste to replace.");
    return "copied";
  } catch (error) {
    addEvent("warning", errorMessage(error));
    return "failed";
  }
}
