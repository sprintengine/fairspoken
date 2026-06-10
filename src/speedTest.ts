import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { pickPassage } from "./speedTestPassages";
import { compareTranscript, type TranscriptComparison } from "./transcriptCompare";

// The speed test: type a passage, then speak it, and see how much faster your
// voice is. The typing leg measures real keystrokes; the speaking leg runs the
// real recording + transcription path through a side-effect-free capture
// (`stop_speed_test_capture`) that never touches the clipboard, history, or
// lifetime stats. Numbers shown are measured, and a garbled transcript is
// gated out of the celebratory multiplier rather than faked into a win.

// ── Pure measurement (no DOM; the spec for "real") ─────────────────────────

const CHARS_PER_WORD = 5; // standard net-wpm convention
const ACCURACY_GATE = 80; // below this, the spoken transcript is too rough to celebrate

/** Net words per minute from correctly-typed characters over elapsed time. */
export function netWpm(correctChars: number, elapsedSeconds: number): number {
  if (elapsedSeconds <= 0 || correctChars <= 0) return 0;
  return Math.round(correctChars / CHARS_PER_WORD / (elapsedSeconds / 60));
}

/** Typing accuracy: correct keystrokes over total keystrokes, as a percent. */
export function accuracyPct(totalTyped: number, errors: number): number {
  if (totalTyped <= 0) return 100;
  return Math.round(((totalTyped - errors) / totalTyped) * 100);
}

// ── Typing-leg recovery ────────────────────────────────────────────────────
// Comparing the typed text to the passage strictly by character index makes a
// single missing space cascade: every later character lands one slot early and
// reads as wrong, turning the rest of the passage red. This walks the passage
// with a tolerant pointer — a skipped space counts as one mistake but does not
// consume the typed character, so the following words realign instead of going
// all-red. It stays character-level (a typing test still cares about exact
// characters) and is kept separate from the spoken transcript comparison.

export type TypedCellState = "done" | "bad" | "pending";

export interface TypedCell {
  ch: string;
  state: TypedCellState;
  /** A passage space the typist ran past — rendered as a highlighted gap. */
  missingSpace: boolean;
}

export interface TypedAnalysis {
  cells: TypedCell[];
  /** Exact character matches — the numerator for net WPM. */
  correctChars: number;
  /** Wrong characters plus skipped spaces, within the region typed so far. */
  errors: number;
  /** The typist has covered the whole passage (the completion trigger). */
  reachedEnd: boolean;
  /** Passage index of the caret, or null once the passage is complete. */
  cursorIndex: number | null;
}

export function analyzeTyping(typed: string, target: string): TypedAnalysis {
  const cells: TypedCell[] = [...target].map((ch): TypedCell => ({
    ch,
    state: "pending",
    missingSpace: false,
  }));
  let correctChars = 0;
  let errors = 0;
  let typedIndex = 0;
  let i = 0;
  for (; i < target.length; i++) {
    if (typedIndex >= typed.length) break; // not reached yet — the rest stays pending
    const expected = target[i];
    if (typed[typedIndex] === expected) {
      cells[i].state = "done";
      correctChars++;
      typedIndex++;
    } else if (expected === " ") {
      // The typist ran two words together. Count the missing space once but keep
      // the typed character for the next passage word so the error can't cascade.
      cells[i].state = "bad";
      cells[i].missingSpace = true;
      errors++;
    } else {
      cells[i].state = "bad";
      errors++;
      typedIndex++;
    }
  }
  const reachedEnd = i >= target.length;
  return { cells, correctChars, errors, reachedEnd, cursorIndex: reachedEnd ? null : i };
}

// The spoken transcript is scored against the passage with the shared word-level
// sequence alignment in `transcriptCompare.ts` — order-aware, and able to tell a
// substitution from a dropped word — rather than an order-blind multiset match.

function wordCount(text: string): number {
  return text.trim().split(/\s+/).filter(Boolean).length;
}

// ── Backend contracts ──────────────────────────────────────────────────────

interface SpeedTestCapture {
  transcript: string;
  recordingSeconds: number;
  transcribeMs: number;
  engine: string;
  model: string;
  previewMode: "chunked" | "final-only" | "unknown";
}

interface SpeedTestRecord {
  typingWpm: number;
  speakingWpm: number;
  multiplier: number;
  accuracy: number;
  recordedAt: number;
}

interface SpeedTestSummary {
  last: SpeedTestRecord | null;
  best: SpeedTestRecord | null;
}

interface SpeedTestOutcome {
  summary: SpeedTestSummary;
  isBest: boolean;
}

interface UsageProjection {
  hasData: boolean;
  totalWords: number;
}

export interface TypingResult {
  seconds: number;
  wpm: number;
  accuracy: number;
}

// ── DOM helper ─────────────────────────────────────────────────────────────

type Child = Node | string;
interface ElProps {
  class?: string;
  text?: string;
  disabled?: boolean;
  ariaLabel?: string;
  onClick?: () => void;
}

function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: ElProps = {},
  children: Child[] = [],
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (props.class) node.className = props.class;
  if (props.text !== undefined) node.textContent = props.text;
  if (props.ariaLabel) node.setAttribute("aria-label", props.ariaLabel);
  if (props.disabled && (node instanceof HTMLButtonElement || node instanceof HTMLInputElement)) {
    node.disabled = true;
  }
  if (props.onClick) node.addEventListener("click", props.onClick);
  for (const child of children) node.append(child);
  return node;
}

function clock(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function span(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  const hours = Math.floor(s / 3600);
  const minutes = Math.round((s % 3600) / 60);
  if (hours > 0) return `${hours} h ${minutes} m`;
  if (s >= 60) return `${minutes} m`;
  return `${s} s`;
}

function engineLabel(engine: string, model: string): string {
  const name = engine === "whisper" ? "Whisper" : engine;
  return model ? `${name} ${model}` : name;
}

// ── Controller ───────────────────────────────────────────────────────────

type State =
  | "intro"
  | "typing"
  | "typed"
  | "speaking"
  | "transcribing"
  | "results"
  | "lowAccuracy"
  | "micError"
  | "transcribeError";

export interface SpeedTestController {
  start(): void;
  stop(): void;
}

export function createSpeedTest(opts: {
  container: HTMLElement;
  onExit: () => void;
}): SpeedTestController {
  const { container, onExit } = opts;
  let passage = pickPassage();
  let state: State = "intro";
  let ticker: number | null = null;
  let typingResult: TypingResult | null = null;
  let best: SpeedTestRecord | null = null;
  let unlistenPreview: UnlistenFn | null = null;
  let previewText = "";
  let previewTextEl: HTMLElement | null = null;
  let previewScoreEl: HTMLElement | null = null;

  function clearTicker(): void {
    if (ticker !== null) {
      window.clearInterval(ticker);
      ticker = null;
    }
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape") return;
    if (state === "typing") renderIntro();
    else if (state === "speaking") renderTyped();
    else if (state === "intro") onExit();
  }

  // ── intro ──
  function renderIntro(): void {
    clearTicker();
    state = "intro";
    typingResult = null;
    const actions: Child[] = [
      h("button", { class: "btn btn-primary", text: "Start typing", onClick: renderTyping }),
      h("button", {
        class: "st-link",
        text: "New passage",
        onClick: () => {
          passage = pickPassage(passage);
          renderIntro();
        },
      }),
      h("button", { class: "btn btn-ghost", text: "Close", onClick: onExit }),
    ];
    if (best) {
      actions.push(h("div", { class: "st-spacer" }));
      actions.push(
        h("span", { class: "st-best", text: `Your best: ${best.multiplier.toFixed(1)}× faster` }),
      );
    }
    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "st-step", text: "Speed test" }),
        h("div", { class: "st-h", text: "How much faster is your voice?" }),
        h("div", {
          class: "st-sub",
          text:
            "Type this passage, then say it out loud. We'll time both — from your real keystrokes and a real transcription, not an average.",
        }),
        h("div", { class: "passage-wrap" }, [h("div", { class: "passage preview", text: passage })]),
        h("div", { class: "st-actions" }, actions),
      ]),
    );
  }

  // ── typing (real keystroke timing) ──
  function renderTyping(): void {
    clearTicker();
    state = "typing";
    const text = passage;

    const rendered = h("div", { class: "passage live", ariaLabel: "Passage to type" });
    const capture = h("textarea", { class: "capture", ariaLabel: "Type the passage" });
    capture.setAttribute("autocomplete", "off");
    capture.setAttribute("autocapitalize", "off");
    capture.setAttribute("autocorrect", "off");
    capture.spellcheck = false;

    const tClock = h("b", { class: "num", text: "0:00" });
    const tWpm = h("b", { class: "num", text: "0" });
    const pasteNote = h("div", { class: "st-paste" });
    const wrap = h("div", { class: "passage-wrap" }, [rendered, capture]);

    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "st-step", text: "Speed test · step 1 of 2" }),
        h("div", { class: "st-live" }, [
          h("div", { class: "lv" }, [tClock, h("span", { text: "elapsed" })]),
          h("div", { class: "lv" }, [tWpm, h("span", { text: "wpm" })]),
        ]),
        wrap,
        pasteNote,
        h("div", { class: "st-hint" }, ["Start typing to begin the clock. ", h("kbd", { text: "Esc" }), " to cancel."]),
      ]),
    );

    let startedAt: number | null = null;

    const paint = (analysis: TypedAnalysis): void => {
      rendered.replaceChildren(
        ...analysis.cells.map((cell, i) => {
          const node = document.createElement("span");
          node.className = "ch";
          if (cell.state === "done") {
            node.classList.add("done");
          } else if (cell.state === "bad") {
            node.classList.add("bad");
            if (cell.missingSpace) node.classList.add("space");
          }
          if (i === analysis.cursorIndex) node.classList.add("cur");
          node.textContent = cell.ch;
          return node;
        }),
      );
    };

    const tick = (): void => {
      if (startedAt === null) return;
      const secs = (performance.now() - startedAt) / 1000;
      tClock.textContent = clock(secs);
      tWpm.textContent = String(netWpm(analyzeTyping(capture.value, text).correctChars, secs));
    };

    capture.addEventListener("paste", (event) => {
      event.preventDefault();
      pasteNote.textContent = "Paste is off for the test — type it so the time is real.";
    });

    capture.addEventListener("input", () => {
      let typed = capture.value;
      if (typed.length > text.length) {
        typed = typed.slice(0, text.length);
        capture.value = typed;
      }
      if (startedAt === null && typed.length > 0) {
        startedAt = performance.now();
        ticker = window.setInterval(tick, 100);
      }
      const analysis = analyzeTyping(typed, text);
      paint(analysis);
      tick();

      if (analysis.reachedEnd) {
        clearTicker();
        const seconds = startedAt !== null ? (performance.now() - startedAt) / 1000 : 0;
        typingResult = {
          seconds,
          wpm: netWpm(analysis.correctChars, seconds),
          accuracy: accuracyPct(typed.length, analysis.errors),
        };
        renderTyped();
      }
    });

    paint(analyzeTyping("", text));
    wrap.addEventListener("click", () => capture.focus());
    window.setTimeout(() => capture.focus(), 0);
  }

  // ── typed (leg-1 result; primary action starts the speaking leg) ──
  function renderTyped(): void {
    clearTicker();
    state = "typed";
    const result = typingResult;
    const legLine = result
      ? h("div", { class: "leg-line" }, [
          "Typed in ",
          h("span", { class: "v num", text: clock(result.seconds) }),
          h("span", { class: "sep", text: "·" }),
          h("span", { class: "v num", text: String(result.wpm) }),
          " wpm ",
          h("span", { class: "sep", text: "·" }),
          h("span", { class: "v num", text: `${result.accuracy}%` }),
          " accurate",
        ])
      : h("div", { class: "leg-line", text: "Typing leg complete." });

    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "st-step", text: "Speed test · step 1 of 2" }),
        legLine,
        h("div", { class: "st-h st-h-sm", text: "Now read it aloud." }),
        h("div", { class: "passage-wrap" }, [h("div", { class: "passage", text: passage })]),
        h("div", { class: "st-actions" }, [
          h("button", { class: "btn btn-primary", text: "Start reading", onClick: startSpeaking }),
          h("button", { class: "st-link", text: "Retype", onClick: renderTyping }),
          h("button", { class: "btn btn-ghost", text: "Done", onClick: onExit }),
        ]),
        h("div", { class: "st-hint", text: "We'll record and transcribe it for real, then compare." }),
      ]),
    );
  }

  // ── speaking (real recording) ──
  async function startSpeaking(): Promise<void> {
    try {
      await invoke("start_recording");
    } catch (err) {
      renderCaptureError("mic", String(err));
      return;
    }
    renderSpeaking();
  }

  function renderSpeaking(): void {
    clearTicker();
    state = "speaking";
    resetPreview();
    const sClock = h("span", { class: "clock num", text: "0:00" });
    const start = performance.now();
    ticker = window.setInterval(() => {
      sClock.textContent = clock((performance.now() - start) / 1000);
    }, 100);

    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "st-step", text: "Speed test · step 2 of 2" }),
        h("div", { class: "listen" }, [h("span", { class: "rdot" }), "Listening…", sClock]),
        h("div", { class: "passage-wrap" }, [h("div", { class: "passage spoken", text: passage })]),
        buildPreviewBlock(),
        h("div", { class: "st-actions" }, [
          h("button", { class: "btn btn-primary", text: "Stop", onClick: stopSpeaking }),
          h("span", { class: "st-hint st-hint-inline" }, ["Read it at your natural pace · ", h("kbd", { text: "Esc" }), " to cancel"]),
        ]),
      ]),
    );
    updatePreviewView();
  }

  async function stopSpeaking(): Promise<void> {
    clearTicker();
    state = "transcribing";
    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "st-step", text: "Speed test · step 2 of 2" }),
        h("div", { class: "transcribe" }, [h("span", { class: "tdot" }), "Transcribing your audio…"]),
        h("div", { class: "passage-wrap" }, [h("div", { class: "passage preview", text: passage })]),
        buildPreviewBlock(),
      ]),
    );
    updatePreviewView();

    let capture: SpeedTestCapture;
    try {
      capture = await invoke<SpeedTestCapture>("stop_speed_test_capture");
    } catch (err) {
      renderCaptureError("transcribe", String(err));
      return;
    }

    const comparison = compareTranscript(capture.transcript, passage);
    if (comparison.accuracy < ACCURACY_GATE) {
      renderLowAccuracy(capture, comparison);
      return;
    }
    await finalizeResults(capture, comparison, false);
  }

  // ── results ──
  async function finalizeResults(
    capture: SpeedTestCapture,
    comparison: TranscriptComparison,
    gated: boolean,
  ): Promise<void> {
    const typing = typingResult;
    if (!typing) {
      renderIntro();
      return;
    }
    const speakingWpm = Math.round(wordCount(passage) / Math.max(capture.recordingSeconds, 0.001) * 60);
    const multiplier = capture.recordingSeconds > 0 ? typing.seconds / capture.recordingSeconds : 0;

    let isBest = false;
    if (!gated && multiplier > 0) {
      try {
        const outcome = await invoke<SpeedTestOutcome>("save_speed_test_result", {
          input: {
            typingWpm: typing.wpm,
            speakingWpm,
            multiplier,
            accuracy: comparison.accuracy,
          },
        });
        isBest = outcome.isBest;
        best = outcome.summary.best;
      } catch {
        /* persistence is best-effort; the result still shows */
      }
    }

    let projection: UsageProjection | null = null;
    try {
      projection = await invoke<UsageProjection>("get_usage_stats");
    } catch {
      /* projection is optional */
    }

    renderResults({ capture, comparison, typing, speakingWpm, multiplier, gated, isBest, projection });
  }

  function renderResults(data: {
    capture: SpeedTestCapture;
    comparison: TranscriptComparison;
    typing: TypingResult;
    speakingWpm: number;
    multiplier: number;
    gated: boolean;
    isBest: boolean;
    projection: UsageProjection | null;
  }): void {
    state = "results";
    const { capture, comparison, typing, speakingWpm, multiplier, gated, isBest, projection } = data;
    const maxSeconds = Math.max(typing.seconds, capture.recordingSeconds, 0.001);

    const heading = gated
      ? h("div", { class: "res-h", text: "Here's how the two legs compared." })
      : h("div", { class: "res-h" }, [
          "You spoke ",
          h("em", { text: `${multiplier.toFixed(1)}×` }),
          " faster than you typed.",
        ]);

    const headRow: Child[] = [heading];
    if (isBest && !gated) headRow.push(h("span", { class: "res-best", text: "New best" }));

    const noteLines: Child[] = [
      `Transcription added ${(capture.transcribeMs / 1000).toFixed(1)} s on ${engineLabel(capture.engine, capture.model)}.`,
    ];
    if (projection?.hasData && projection.totalWords > 0) {
      noteLines.push(h("br"));
      noteLines.push(
        h("span", {}, [
          "At ",
          h("span", { class: "strong num", text: String(typing.wpm) }),
          " wpm, your ",
          h("span", { class: "strong num", text: projection.totalWords.toLocaleString() }),
          " lifetime words would take about ",
          h("span", { class: "strong", text: span((projection.totalWords / typing.wpm) * 60) }),
          " to type.",
        ]),
      );
    }

    const useWpmLink = h("button", {
      class: "st-link",
      text: `Use ${typing.wpm} wpm as my typing speed →`,
      onClick: () => {
        void invoke<unknown>("set_measured_typing_wpm", { wpm: typing.wpm })
          .then(() => {
            useWpmLink.textContent = `Saved — your time-saved stat now uses ${typing.wpm} wpm`;
            useWpmLink.classList.add("is-saved");
            (useWpmLink as HTMLButtonElement).disabled = true;
          })
          .catch(() => {
            useWpmLink.textContent = "Couldn't save your typing speed — try again";
          });
    },
    });

    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "res-head" }, headRow),
        h("div", { class: "compare" }, [
          h("div", { class: "col" }, [
            h("div", { class: "c-label" }, [h("span", { class: "swatch typed" }), "Typed"]),
            h("div", { class: "c-time num", text: clock(typing.seconds) }),
            h("div", { class: "c-meta num", text: `${typing.wpm} wpm · ${typing.accuracy}% accurate` }),
          ]),
          h("div", { class: "col" }, [
            h("div", { class: "c-label" }, [h("span", { class: "swatch spoke" }), "Spoke"]),
            h("div", { class: "c-time num", text: clock(capture.recordingSeconds) }),
            h("div", { class: "c-meta num", text: `${speakingWpm} wpm · ${comparison.accuracy}% accurate` }),
          ]),
        ]),
        h("div", { class: "bars" }, [
          bar("Typed", "typed", typing.seconds / maxSeconds, clock(typing.seconds)),
          bar("Spoke", "spoke", capture.recordingSeconds / maxSeconds, clock(capture.recordingSeconds)),
        ]),
        h("div", { class: "res-note" }, noteLines),
        diagnostics(comparison),
        h("div", { class: "st-actions" }, [
          h("button", {
            class: "btn btn-primary",
            text: "Try again",
            onClick: () => {
              passage = pickPassage(passage);
              renderIntro();
            },
          }),
          h("button", { class: "btn btn-ghost", text: "Done", onClick: onExit }),
          h("div", { class: "st-spacer" }),
          useWpmLink,
        ]),
      ]),
    );
  }

  function bar(label: string, kind: "typed" | "spoke", fraction: number, value: string): HTMLElement {
    const fill = h("span", { class: `bar-fill ${kind}` });
    fill.style.width = `${Math.max(2, Math.min(100, fraction * 100)).toFixed(1)}%`;
    return h("div", { class: "bar-row" }, [
      h("span", { class: "k", text: label }),
      h("span", { class: "bar-track" }, [fill]),
      h("span", { class: "t num", text: value }),
    ]);
  }

  // ── live preview + shared diagnostics ──
  // Both legs of the spoken comparison use the shared sequence alignment in
  // transcriptCompare.ts; these are only the small DOM wrappers, kept in step
  // with the voice accuracy test's rendering.
  function resetPreview(): void {
    previewText = "";
    previewTextEl = null;
    previewScoreEl = null;
  }

  // Honor global preview events only while this leg is recording or finalizing,
  // so a dictation or voice-test preview can't leak into the speed test.
  async function attachPreview(): Promise<void> {
    if (unlistenPreview) return;
    try {
      unlistenPreview = await listen<{ text: string }>("transcript-preview", (event) => {
        if (state !== "speaking" && state !== "transcribing") return;
        previewText = event.payload.text;
        updatePreviewView();
      });
    } catch {
      /* live preview is best-effort; the leg still completes without it */
    }
  }

  function buildPreviewBlock(): HTMLElement {
    previewScoreEl = h("span", { class: "mode" });
    previewTextEl = h("div", { class: "ptext empty" });
    const block = h("div", { class: "vt-preview" }, [
      h("div", { class: "ph" }, ["Heard so far", previewScoreEl]),
      previewTextEl,
    ]);
    block.setAttribute("aria-live", "polite");
    return block;
  }

  function updatePreviewView(): void {
    if (!previewTextEl) return;
    if (!previewText) {
      previewTextEl.className = "ptext empty";
      previewTextEl.textContent =
        state === "transcribing" ? "Finalizing…" : "Listening for your first words…";
      if (previewScoreEl) previewScoreEl.textContent = "";
      return;
    }
    previewTextEl.className = "ptext";
    previewTextEl.textContent = previewText;
    if (previewScoreEl) {
      const heard = compareTranscript(previewText, passage);
      previewScoreEl.textContent = `${heard.hits} of ${heard.total} so far`;
    }
  }

  // The target passage coloured by the shared alignment plus the raw transcript —
  // the same trust evidence the voice accuracy test shows.
  function comparedTarget(comparison: TranscriptComparison): HTMLElement {
    const el = h("div", { class: "passage compared" });
    comparison.targetTokens.forEach((token, i) => {
      if (i > 0) el.append(" ");
      const cls = token.state === "hit" ? "w" : token.state === "substituted" ? "w sub" : "w miss";
      el.append(h("span", { class: cls, text: token.raw }));
    });
    return el;
  }

  function heardTranscript(comparison: TranscriptComparison): HTMLElement {
    if (comparison.transcriptTokens.length === 0) {
      return h("div", { class: "vt-transcript" }, [
        h("span", { class: "empty", text: "No transcript came back from the backend." }),
      ]);
    }
    const el = h("div", { class: "vt-transcript" });
    comparison.transcriptTokens.forEach((token, i) => {
      if (i > 0) el.append(" ");
      const cls =
        token.state === "inserted" ? "w ins" : token.state === "substituted" ? "w sub" : "w";
      el.append(h("span", { class: cls, text: token.raw }));
    });
    return el;
  }

  function diagnostics(comparison: TranscriptComparison): HTMLElement {
    return h("div", {}, [
      h("div", { class: "vt-label", text: "Target passage" }),
      comparedTarget(comparison),
      h("div", { class: "vt-label", text: "What we heard" }),
      heardTranscript(comparison),
      h("div", { class: "vt-legend" }, [
        h("span", {}, [h("span", { class: "sw miss" }), " omitted"]),
        h("span", {}, [h("span", { class: "sw acc" }), " heard differently / extra"]),
      ]),
    ]);
  }

  // ── low accuracy (honesty guard) ──
  function renderLowAccuracy(capture: SpeedTestCapture, comparison: TranscriptComparison): void {
    state = "lowAccuracy";
    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "err" }, [
          h("span", { class: "e-dot warn" }),
          h("div", { class: "err-h", text: `Only ${comparison.hits} of ${comparison.total} words came through.` }),
          h("div", {
            class: "err-sub",
            text: "A fast run on a garbled transcript isn't a real win — let's get a clean read before we call it.",
          }),
        ]),
        diagnostics(comparison),
        h("div", { class: "st-actions" }, [
          h("button", { class: "btn btn-primary", text: "Run speaking leg again", onClick: startSpeaking }),
          h("button", {
            class: "st-link",
            text: "Skip — show times anyway",
            onClick: () => {
              void finalizeResults(capture, comparison, true);
            },
          }),
        ]),
      ]),
    );
  }

  // ── capture errors (distinct mic vs. transcription copy) ──
  function renderCaptureError(kind: "mic" | "transcribe", detail: string): void {
    state = kind === "mic" ? "micError" : "transcribeError";
    const head =
      kind === "mic" ? "Couldn't start the recording." : "Transcription didn't finish.";
    const sub =
      kind === "mic"
        ? `Check your microphone under Settings → Capture, then run the speaking leg again. (${detail})`
        : `Your typing time is saved — retry the speaking leg to finish the comparison. (${detail})`;

    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "err" }, [
          h("span", { class: "e-dot" }),
          h("div", { class: "err-h", text: head }),
          h("div", { class: "err-sub", text: sub }),
        ]),
        h("div", { class: "st-actions" }, [
          h("button", { class: "btn btn-primary", text: "Retry", onClick: startSpeaking }),
          h("button", { class: "btn btn-ghost", text: "Done", onClick: renderTyped }),
        ]),
      ]),
    );
  }

  function start(): void {
    document.removeEventListener("keydown", onKeydown);
    document.addEventListener("keydown", onKeydown);
    void attachPreview();
    renderIntro();
    // Best-effort: show the personal best on the intro once it loads.
    void invoke<SpeedTestSummary>("get_speed_test_summary")
      .then((summary) => {
        best = summary.best;
        if (state === "intro") renderIntro();
      })
      .catch(() => {
        /* no best yet, or running outside the app */
      });
  }

  function stop(): void {
    clearTicker();
    document.removeEventListener("keydown", onKeydown);
    if (unlistenPreview) {
      unlistenPreview();
      unlistenPreview = null;
    }
    resetPreview();
  }

  return { start, stop };
}
