import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { pickPassage } from "./speedTestPassages";
import { compareTranscript, type TranscriptComparison } from "./transcriptCompare";

// The voice accuracy test: read a known passage aloud, then see what the real
// transcription backend actually heard. It answers "did it hear what I said?"
// rather than the speed test's "is my voice faster than typing?". Recording and
// transcription run through the real path via the side-effect-free
// `stop_voice_test_capture` command, so a test never touches the clipboard,
// transcript history, Notes, or usage stats — it must not pollute the numbers it
// sits beside. Scoring is the shared word-level alignment in transcriptCompare,
// the same primitive the speed test's speaking leg uses.

type PreviewMode = "chunked" | "final-only" | "unknown";

// Mirrors the Rust `SpeedTestCapture` returned by `stop_voice_test_capture`.
interface VoiceTestCapture {
  transcript: string;
  recordingSeconds: number;
  transcribeMs: number;
  engine: string;
  model: string;
  previewMode: PreviewMode;
}

// Global `transcript-preview` event; we honor it only while a test is active.
interface TranscriptPreviewEvent {
  index: number;
  text: string;
  finalPreview: boolean;
}

// We read only the fields we need from the full settings payload.
interface VoiceSettings {
  transcriptionLocation: "local" | "remote-host";
  model: string;
}

interface BackendInfo {
  engine: string;
  model: string;
  location: "local" | "remote-host";
  previewMode: PreviewMode;
}

// ── DOM helper (local to this controller, mirrors the speed test's) ─────────

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

/** Seconds to one decimal place, e.g. `6.2 s`. */
function secs(value: number): string {
  return `${Math.max(0, value).toFixed(1)} s`;
}

function engineDisplay(engine: string): string {
  return engine === "whisper" ? "Whisper" : engine;
}

function previewModeLabel(mode: PreviewMode): string {
  switch (mode) {
    case "chunked":
      return "chunked preview";
    case "final-only":
      return "final-only";
    default:
      return "unknown";
  }
}

/** The same local-settings → preview-mode mapping the backend uses, so the live
 *  label during recording matches the mode reported on the final capture. */
function inferPreviewMode(settings: VoiceSettings): PreviewMode {
  return settings.transcriptionLocation === "remote-host" ? "final-only" : "chunked";
}

// ── Controller ───────────────────────────────────────────────────────────

type State = "intro" | "recording" | "transcribing" | "results" | "micError" | "transcribeError";

export interface VoiceAccuracyTestController {
  start(): void;
  stop(): void;
}

export function createVoiceAccuracyTest(opts: {
  container: HTMLElement;
  onExit: () => void;
}): VoiceAccuracyTestController {
  const { container, onExit } = opts;
  let passage = pickPassage();
  let state: State = "intro";
  let ticker: number | null = null;
  let backend: BackendInfo | null = null;
  let unlistenPreview: UnlistenFn | null = null;
  // Bumped by stop(); a listen() that resolves for an older generation is
  // released at once instead of leaking a listener.
  let previewGeneration = 0;

  let previewText = "";
  let previewTextEl: HTMLElement | null = null;
  let previewScoreEl: HTMLElement | null = null;

  function clearTicker(): void {
    if (ticker !== null) {
      window.clearInterval(ticker);
      ticker = null;
    }
  }

  function resetPreview(): void {
    previewText = "";
    previewTextEl = null;
    previewScoreEl = null;
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape") return;
    if (state === "recording") cancelRecording();
    else if (state === "intro") onExit();
  }

  // Honor global preview events only while this test is actively recording or
  // finalizing — otherwise a dictation or speed-test preview could leak in.
  async function attachPreview(): Promise<void> {
    if (unlistenPreview) return;
    const generation = ++previewGeneration;
    try {
      const unlisten = await listen<TranscriptPreviewEvent>("transcript-preview", (event) => {
        if (state !== "recording" && state !== "transcribing") return;
        previewText = event.payload.text;
        updatePreviewView();
      });
      if (generation !== previewGeneration) unlisten();
      else unlistenPreview = unlisten;
    } catch {
      /* live preview is best-effort; the test still completes without it */
    }
  }

  async function loadBackend(): Promise<void> {
    try {
      const settings = await invoke<VoiceSettings>("get_settings");
      backend = {
        engine: "whisper",
        model: settings.model,
        location: settings.transcriptionLocation,
        previewMode: inferPreviewMode(settings),
      };
      if (state === "intro") renderIntro();
    } catch {
      /* the backend label is a nicety; results still show the real engine/model */
    }
  }

  // ── intro ──
  function renderIntro(): void {
    clearTicker();
    state = "intro";
    resetPreview();

    const body: Child[] = [
      
      h("div", { class: "st-h", text: "Did it hear what you said?" }),
      h("div", {
        class: "st-sub",
        text:
          "Read this aloud. Nothing is saved.",
      }),
      h("div", { class: "passage-wrap" }, [h("div", { class: "passage preview", text: passage })]),
    ];
    if (backend) {
      body.push(
        h("div", { class: "vt-source" }, [
          h("span", { class: "k", text: "Backend" }),
          ` ${engineDisplay(backend.engine)} ${backend.model}`,
          h("span", { class: "sep", text: "·" }),
          backend.location === "remote-host" ? "remote host" : "local",
        ]),
      );
    }
    body.push(
      h("div", { class: "st-actions" }, [
        h("button", { class: "btn btn-primary", text: "Start reading", onClick: startRecording }),
        h("button", {
          class: "st-link",
          text: "New passage",
          onClick: () => {
            passage = pickPassage(passage);
            renderIntro();
          },
        }),
        h("button", { class: "btn btn-ghost", text: "Close", onClick: onExit }),
      ]),
    );

    container.replaceChildren(h("div", {}, body));
  }

  // ── recording (real capture) ──
  async function startRecording(): Promise<void> {
    resetPreview();
    try {
      await invoke("start_recording");
    } catch (err) {
      renderCaptureError("mic", String(err));
      return;
    }
    renderRecording();
  }

  function renderRecording(): void {
    clearTicker();
    state = "recording";
    const sClock = h("span", { class: "clock num", text: "0:00" });
    const start = performance.now();
    ticker = window.setInterval(() => {
      sClock.textContent = clock((performance.now() - start) / 1000);
    }, 100);

    container.replaceChildren(
      h("div", {}, [
        
        h("div", { class: "listen" }, [h("span", { class: "rdot" }), "Listening…", sClock]),
        h("div", { class: "passage-wrap" }, [h("div", { class: "passage spoken", text: passage })]),
        buildPreviewBlock(),
        h("div", { class: "st-actions" }, [
          h("button", { class: "btn btn-primary", text: "Stop", onClick: stopRecording }),
          h("span", { class: "st-hint st-hint-inline" }, [
            "Read at your natural pace · ",
            h("kbd", { text: "Esc" }),
            " to cancel",
          ]),
        ]),
      ]),
    );
    updatePreviewView();
  }

  // Stop the real recorder through the side-effect-free capture and discard the
  // result — this guarantees the audio is never committed, even on cancel.
  function cancelRecording(): void {
    clearTicker();
    state = "intro";
    void invoke("stop_voice_test_capture").catch(() => {
      /* nothing to commit either way */
    });
    resetPreview();
    renderIntro();
  }

  async function stopRecording(): Promise<void> {
    clearTicker();
    state = "transcribing";
    container.replaceChildren(
      h("div", {}, [
        
        h("div", { class: "transcribe" }, [
          h("span", { class: "tdot" }),
          "Finalizing the transcript…",
        ]),
        h("div", { class: "passage-wrap" }, [h("div", { class: "passage preview", text: passage })]),
        buildPreviewBlock(),
      ]),
    );
    updatePreviewView();

    let capture: VoiceTestCapture;
    try {
      capture = await invoke<VoiceTestCapture>("stop_voice_test_capture");
    } catch (err) {
      renderCaptureError("transcribe", String(err));
      return;
    }
    renderResults(capture);
  }

  // ── live preview ("heard so far"; never the final score) ──
  function buildPreviewBlock(): HTMLElement {
    const mode = backend?.previewMode ?? "unknown";
    previewScoreEl = h("span", { class: "mode" });
    previewTextEl = h("div", { class: "ptext empty" });
    const block = h("div", { class: "vt-preview" }, [
      h("div", { class: "ph" }, [
        "Heard so far",
        previewScoreEl,
        h("span", { class: "mode-tag", text: previewModeLabel(mode) }),
      ]),
      previewTextEl,
    ]);
    block.setAttribute("aria-live", "polite");
    return block;
  }

  function updatePreviewView(): void {
    if (!previewTextEl) return;
    if (backend?.previewMode === "final-only") {
      previewTextEl.className = "ptext empty";
      previewTextEl.textContent =
        "This backend returns the transcript only after you stop — no live preview.";
      if (previewScoreEl) previewScoreEl.textContent = "";
      return;
    }
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

  // ── results ──
  function renderResults(capture: VoiceTestCapture): void {
    state = "results";
    const comparison = compareTranscript(capture.transcript, passage);
    const totalSeconds = capture.recordingSeconds + capture.transcribeMs / 1000;

    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "res-head" }, [
          h("div", { class: "res-h" }, [
            "It caught ",
            h("em", { text: `${comparison.hits} of ${comparison.total}` }),
            " words.",
          ]),
          h("span", { class: "res-best num", text: `${comparison.accuracy}% accurate` }),
        ]),
        h("div", { class: "vt-source" }, [
          h("span", { class: "k", text: "Engine" }),
          ` ${engineDisplay(capture.engine)} ${capture.model}`,
          h("span", { class: "sep", text: "·" }),
          h("span", { class: "k", text: "Preview" }),
          ` ${previewModeLabel(capture.previewMode)}`,
        ]),
        h("div", { class: "vt-metrics" }, [
          metric("Accuracy", `${comparison.accuracy}%`, `${comparison.hits} of ${comparison.total} words`),
          metric("Recording", secs(capture.recordingSeconds), "audio length"),
          metric("Transcribe", secs(capture.transcribeMs / 1000), "after stop"),
          metric("Total", secs(totalSeconds), "record + finalize"),
        ]),
        h("div", { class: "vt-label", text: "Target passage" }),
        comparedTarget(comparison),
        h("div", { class: "vt-label", text: "What we heard" }),
        heardTranscript(comparison),
        h("div", { class: "vt-legend" }, [
          h("span", {}, [h("span", { class: "sw miss" }), " omitted"]),
          h("span", {}, [h("span", { class: "sw acc" }), " heard differently / extra"]),
        ]),
        h("div", { class: "st-actions" }, [
          h("button", {
            class: "btn btn-primary",
            text: "Try again",
            onClick: () => {
              passage = pickPassage(passage);
              renderIntro();
            },
          }),
          h("button", {
            class: "st-link",
            text: "New passage",
            onClick: () => {
              passage = pickPassage(passage);
              renderIntro();
            },
          }),
          h("button", { class: "btn btn-ghost", text: "Done", onClick: onExit }),
        ]),
      ]),
    );
  }

  function metric(label: string, value: string, meta: string): HTMLElement {
    return h("div", { class: "vt-metric" }, [
      h("div", { class: "m-label", text: label }),
      h("div", { class: "m-value num", text: value }),
      h("div", { class: "m-meta", text: meta }),
    ]);
  }

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

  // ── capture errors (distinct mic vs transcription copy) ──
  function renderCaptureError(kind: "mic" | "transcribe", detail: string): void {
    state = kind === "mic" ? "micError" : "transcribeError";
    clearTicker();
    const head = kind === "mic" ? "Couldn't start the recording." : "Transcription didn't finish.";
    const backendNote = backend ? `the ${engineDisplay(backend.engine)} ${backend.model} backend` : "the backend";
    const sub =
      kind === "mic"
        ? `Check your microphone under Settings → Capture, then try again. (${detail})`
        : `${backendNote} stopped before returning a transcript. Your recording wasn't saved — try the read again. (${detail})`;

    container.replaceChildren(
      h("div", {}, [
        h("div", { class: "err" }, [
          h("span", { class: "e-dot" }),
          h("div", { class: "err-h", text: head }),
          h("div", { class: "err-sub", text: sub }),
        ]),
        h("div", { class: "st-actions" }, [
          h("button", { class: "btn btn-primary", text: "Retry", onClick: startRecording }),
          h("button", { class: "btn btn-ghost", text: "Done", onClick: renderIntro }),
        ]),
      ]),
    );
  }

  function start(): void {
    document.removeEventListener("keydown", onKeydown);
    document.addEventListener("keydown", onKeydown);
    void attachPreview();
    void loadBackend();
    renderIntro();
  }

  function stop(): void {
    clearTicker();
    document.removeEventListener("keydown", onKeydown);
    previewGeneration += 1;
    if (unlistenPreview) {
      unlistenPreview();
      unlistenPreview = null;
    }
    // Navigating away mid-recording: stop the recorder side-effect-free so the
    // audio is never committed. During `transcribing` a stop is already in flight.
    if (state === "recording") {
      void invoke("stop_voice_test_capture").catch(() => {
        /* nothing to commit either way */
      });
    }
    state = "intro";
    resetPreview();
  }

  return { start, stop };
}
