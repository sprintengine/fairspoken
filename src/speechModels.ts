// People-facing names for the speech model ids in Settings.
export function speechModelName(model: string): string {
  if (model === "parakeet-ultra") return "Parakeet Ultra 0.6B";
  return model.startsWith("parakeet") ? `Parakeet TDT 0.6B ${model.endsWith("-v2") ? "v2 · English" : "v3"}` : `Whisper ${model}`;
}

// The short name the sidebar status card has room for: "Parakeet v3".
export function speechModelShortName(model: string): string {
  if (model === "parakeet-ultra") return "Parakeet Ultra";
  return model.startsWith("parakeet") ? `Parakeet ${model.endsWith("-v2") ? "v2" : "v3"}` : `Whisper ${model}`;
}
