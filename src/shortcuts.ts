// Accelerators are stored in the Tauri global-shortcut grammar
// (e.g. "CommandOrControl+Shift+Digit1"). These helpers normalise them and
// render them for people: keycap glyphs, spoken words, and the pill's hint.

export function isMacOS(): boolean {
  return navigator.platform.toLowerCase().includes("mac");
}

const onMac = isMacOS();

const KEY_GLYPHS: Record<string, string> = {
  CommandOrControl: onMac ? "⌘" : "Ctrl",
  Command: "⌘", Cmd: "⌘", Meta: onMac ? "⌘" : "Win", Super: onMac ? "⌘" : "Win",
  Control: "⌃", Ctrl: "⌃",
  Alt: onMac ? "⌥" : "Alt", Option: "⌥",
  Shift: "⇧",
  ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
  Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Backslash: "\\",
  Semicolon: ";", Quote: "'", Comma: ",", Period: ".", Slash: "/", Backquote: "`",
};
const KEY_WORDS: Record<string, string> = {
  CommandOrControl: onMac ? "Command" : "Control",
  Command: "Command", Cmd: "Command", Meta: onMac ? "Command" : "Windows", Super: onMac ? "Command" : "Windows",
  Control: "Control", Ctrl: "Control",
  Alt: onMac ? "Option" : "Alt", Option: "Option",
  Shift: "Shift",
};

export function accelTokens(accelerator: string): string[] {
  return accelerator.split("+").map((token) => token.trim()).filter(Boolean);
}

/** Trim each part and drop empty ones; an empty result falls back. */
export function normalizeShortcut(value: string, fallback = ""): string {
  return accelTokens(value).join("+").slice(0, 80) || fallback;
}

// "Digit1", "Numpad1" and "KeyA" read as the key itself.
function bareKey(token: string): string {
  const digit = /^(?:Digit|Numpad)([0-9])$/.exec(token);
  if (digit) return digit[1];
  const letter = /^Key([A-Z])$/.exec(token);
  return letter ? letter[1] : token;
}

/** The keycap for one accelerator token. */
export function keyGlyph(token: string): string {
  return KEY_GLYPHS[token] ?? bareKey(token);
}

/** The spoken name of one accelerator token, for accessible labels. */
export function keyWord(token: string): string {
  return KEY_WORDS[token] ?? bareKey(token);
}

/** Compact hint for the pill: "⌘⇧1" on macOS, "Ctrl+Shift+1" elsewhere. */
export function shortcutHint(shortcut: string): string {
  const tokens = accelTokens(shortcut);
  if (onMac) return tokens.map(keyGlyph).join("");
  return tokens.map((token) => (token === "CommandOrControl" ? "Ctrl" : bareKey(token))).join("+");
}
