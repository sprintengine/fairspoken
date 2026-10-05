import Foundation

/// The Tauri accelerator strings stored in `settings.json` (`"CommandOrControl+Shift+Digit1"`).
/// The Mac app binds shortcuts with KeyboardShortcuts and mirrors the choice into this
/// format so the shared schema stays meaningful in both apps.
public struct AcceleratorString: Equatable, Sendable {
    public struct Modifiers: OptionSet, Hashable, Sendable {
        public let rawValue: Int
        public init(rawValue: Int) { self.rawValue = rawValue }
        public static let command = Modifiers(rawValue: 1)
        public static let shift = Modifiers(rawValue: 2)
        public static let option = Modifiers(rawValue: 4)
        public static let control = Modifiers(rawValue: 8)
    }

    public static let defaultRecording = "CommandOrControl+Shift+Digit1"
    public static let defaultTranscriptStack = "CommandOrControl+Shift+Digit2"

    public var modifiers: Modifiers
    /// W3C `KeyboardEvent.code` name: `Digit1`, `KeyA`, `Space`, `F5`, `Escape`…
    public var code: String

    public init(modifiers: Modifiers, code: String) {
        self.modifiers = modifiers
        self.code = code
    }

    public init?(parsing raw: String) {
        var mods: Modifiers = []
        var code: String?
        for part in raw.split(separator: "+").map({ $0.trimmingCharacters(in: .whitespaces) }) where !part.isEmpty {
            switch part.lowercased() {
            case "commandorcontrol", "cmdorctrl", "command", "cmd", "super", "meta": mods.insert(.command)
            case "shift": mods.insert(.shift)
            case "alt", "option": mods.insert(.option)
            case "control", "ctrl": mods.insert(.control)
            default:
                guard code == nil else { return nil }
                code = Self.normalizeCode(part)
            }
        }
        guard let code else { return nil }
        self.modifiers = mods
        self.code = code
    }

    public var string: String {
        var parts: [String] = []
        if modifiers.contains(.command) { parts.append("CommandOrControl") }
        if modifiers.contains(.control) { parts.append("Control") }
        if modifiers.contains(.option) { parts.append("Alt") }
        if modifiers.contains(.shift) { parts.append("Shift") }
        parts.append(code)
        return parts.joined(separator: "+")
    }

    /// Human form for UI: `⌘⇧1`.
    public var symbols: String {
        var s = ""
        if modifiers.contains(.control) { s += "⌃" }
        if modifiers.contains(.option) { s += "⌥" }
        if modifiers.contains(.shift) { s += "⇧" }
        if modifiers.contains(.command) { s += "⌘" }
        if code.hasPrefix("Digit") { return s + code.dropFirst(5) }
        if code.hasPrefix("Key") { return s + code.dropFirst(3) }
        let names = ["Space": "Space", "Escape": "⎋", "Enter": "↩", "Tab": "⇥", "Backspace": "⌫"]
        return s + (names[code] ?? code)
    }

    static func normalizeCode(_ part: String) -> String {
        if part.count == 1, let ch = part.first {
            if ch.isNumber { return "Digit\(ch)" }
            if ch.isLetter { return "Key\(ch.uppercased())" }
        }
        return part
    }
}
