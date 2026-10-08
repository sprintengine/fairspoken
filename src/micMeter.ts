import { required } from "./dom";
import { addEvent } from "./events";
import type { Settings, SettingsFormHost } from "./settingsSchema";

// Settings → Audio: the microphone list and the live input level under it.
// The meter opens its own capture stream (separate from dictation) only
// while the Audio page is on screen in a visible window.

const refreshBtn = required<HTMLButtonElement>("refreshDevices");
const audioDeviceSelect = required<HTMLSelectElement>("audioDeviceSelect");
const inputGain = required<HTMLSelectElement>("inputGain");
const inputMeter = required<HTMLElement>("inputMeter");

let host: SettingsFormHost;
let meterStream: MediaStream | null = null;
let meterContext: AudioContext | null = null;
let meterAnimation: number | null = null;
let meterSessionId = 0;
// Device and processing the running (or starting) meter was opened with.
let meterKey: string | null = null;

export async function loadAudioDevices(): Promise<void> {
  const selected = audioDeviceSelect.value || host.current().audioDevice || "";
  const devices = await navigator.mediaDevices?.enumerateDevices().catch(() => []) ?? [];
  const inputs = devices.filter((device) => device.kind === "audioinput");

  audioDeviceSelect.replaceChildren(new Option("Default microphone", ""));
  inputs.forEach((device, index) => {
    audioDeviceSelect.appendChild(new Option(device.label || `Microphone ${index + 1}`, device.deviceId));
  });

  if (Array.from(audioDeviceSelect.options).some((option) => option.value === selected)) {
    audioDeviceSelect.value = selected;
  }
}

function meterConfig(settings: Settings): string {
  return JSON.stringify([settings.audioDevice ?? "", settings.echoCancellation, settings.noiseSuppression]);
}

async function startMeter(): Promise<void> {
  stopMeter();
  const sessionId = ++meterSessionId;
  const settings = host.readFromForm();
  meterKey = meterConfig(settings);

  try {
    const constraints: MediaTrackConstraints = {
      sampleRate: 16000,
      channelCount: 1,
      echoCancellation: settings.echoCancellation,
      noiseSuppression: settings.noiseSuppression,
    };
    if (settings.audioDevice) {
      constraints.deviceId = { exact: settings.audioDevice };
    }

    const stream = await navigator.mediaDevices.getUserMedia({ audio: constraints });
    if (sessionId !== meterSessionId) {
      stream.getTracks().forEach((track) => track.stop());
      return;
    }

    const context = new AudioContext({ sampleRate: 16000 });
    meterStream = stream;
    meterContext = context;
    const source = context.createMediaStreamSource(stream);
    const analyser = context.createAnalyser();
    analyser.fftSize = 128;
    source.connect(analyser);

    const data = new Uint8Array(analyser.frequencyBinCount);
    const tick = () => {
      if (sessionId !== meterSessionId) return;
      analyser.getByteTimeDomainData(data);
      let sumSq = 0;
      for (let i = 0; i < data.length; i += 1) {
        const value = (data[i] - 128) / 128;
        sumSq += value * value;
      }
      const rms = Math.sqrt(sumSq / data.length);
      inputMeter.style.transform = `scaleX(${Math.min(1, rms * Number(inputGain.value || 2) * 8).toFixed(3)})`;
      meterAnimation = requestAnimationFrame(tick);
    };
    tick();
    await loadAudioDevices();
  } catch {
    if (sessionId === meterSessionId) {
      meterKey = null;
      addEvent("warning", "Microphone meter could not start");
      inputMeter.style.transform = "scaleX(0)";
    }
  }
}

function stopMeter(): void {
  meterSessionId += 1;
  meterKey = null;
  if (meterAnimation !== null) {
    cancelAnimationFrame(meterAnimation);
    meterAnimation = null;
  }
  meterStream?.getTracks().forEach((track) => track.stop());
  meterStream = null;
  if (meterContext && meterContext.state !== "closed") {
    void meterContext.close();
  }
  meterContext = null;
}

let meterScreenVisible = false;

// The mic meter is the one always-on cost on this window: getUserMedia holds the
// microphone open and a rAF loop runs every frame. Scope it to when the Settings
// screen is actually on-screen and the window is visible, so navigating to
// another screen — or hiding the window — releases the mic and stops the loop.
// Reopening the microphone is not free (and briefly drops the level), so a
// refresh that would reopen the same device with the same processing is a
// no-op unless `force` asks for a restart (the Refresh microphones button).
export function refreshMeter(force = false): void {
  if (meterScreenVisible && required<HTMLElement>("screen-settings").classList.contains("active") && !document.querySelector<HTMLElement>("[data-settings-page=audio]")?.hidden && document.visibilityState === "visible") {
    if (!force && meterKey !== null && meterKey === meterConfig(host.readFromForm())) return;
    void startMeter();
  } else {
    stopMeter();
  }
}

export function initMicMeter(formHost: SettingsFormHost): void {
  host = formHost;
  refreshBtn.addEventListener("click", () => {
    void loadAudioDevices();
    refreshMeter(true);
  });
  // Observe only Audio; hidden categories release this meter's own stream.
  // The backend dictation capture has a separate lifetime.
  new IntersectionObserver((entries) => {
    meterScreenVisible = entries.some((entry) => entry.isIntersecting);
    refreshMeter();
  }).observe(document.querySelector<HTMLElement>("[data-settings-page=audio]")!);
  const settingsScreen = required<HTMLElement>("screen-settings");
  document.addEventListener("home-screen-changed", () => refreshMeter());
  document.addEventListener("settings-category-changed", () => refreshMeter());
  document.addEventListener("visibilitychange", () => refreshMeter());
  new MutationObserver(() => {
    if (!settingsScreen.classList.contains("active")) {
      meterScreenVisible = false;
      refreshMeter();
    }
  }).observe(settingsScreen, { attributes: true, attributeFilter: ["class", "hidden"] });
  window.addEventListener("beforeunload", stopMeter);
}
