import recordingStartUrl from "./assets/sounds/recording-start.wav";
import recordingStopUrl from "./assets/sounds/recording-stop.wav";

let interactionSoundsEnabled = true;
let context: AudioContext | null = null;
const bufferCache = new Map<string, Promise<AudioBuffer>>();

export function setInteractionSoundsEnabled(enabled: boolean): void {
  interactionSoundsEnabled = enabled;
  if (enabled) preloadInteractionSounds();
}

export function preloadInteractionSounds(): void {
  void loadBuffer(recordingStartUrl).catch(() => bufferCache.delete(recordingStartUrl));
  void loadBuffer(recordingStopUrl).catch(() => bufferCache.delete(recordingStopUrl));
}

export function playRecordingStartSound(): void {
  void play(recordingStartUrl);
}

export function playRecordingStopSound(): void {
  void play(recordingStopUrl);
}

function audioContext(): AudioContext {
  if (!context) {
    context = new AudioContext({ latencyHint: "interactive" });
  }
  return context;
}

async function loadBuffer(url: string): Promise<AudioBuffer> {
  let pending = bufferCache.get(url);
  if (!pending) {
    pending = fetch(url)
      .then((response) => response.arrayBuffer())
      .then((data) => audioContext().decodeAudioData(data));
    bufferCache.set(url, pending);
  }
  return pending;
}

async function play(url: string): Promise<void> {
  if (!interactionSoundsEnabled) return;
  try {
    const ctx = audioContext();
    if (ctx.state === "suspended") await ctx.resume();
    const source = ctx.createBufferSource();
    source.buffer = await loadBuffer(url);
    source.connect(ctx.destination);
    source.start();
  } catch {
    bufferCache.delete(url);
  }
}
