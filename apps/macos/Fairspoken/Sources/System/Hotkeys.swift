import AppKit
import CoreGraphics
import KeyboardShortcuts
import FairspokenCore
import OSLog

extension KeyboardShortcuts.Name {
    /// ⌘⇧1 by default, matching the Tauri app's `recordingShortcut`.
    static let dictation = Self("dictation", default: .init(.one, modifiers: [.command, .shift]))
    /// Registered only while recording, so Esc is never stolen from other apps otherwise.
    static let cancelDictation = Self("cancelDictation", default: .init(.escape))
}

/// Global dictation triggers: the KeyboardShortcuts combo (Carbon hot key, no permission)
/// in toggle or push-to-talk mode, plus optional hold-Fn/Globe via a listen-only event tap.
@MainActor
final class HotkeyController {
    var onPress: (() -> Void)?
    var onRelease: (() -> Void)?
    var onCancel: (() -> Void)?
    var onFnPress: (() -> Void)?
    var onFnRelease: (() -> Void)?

    private let fnMonitor = FnKeyMonitor()

    init() {
        KeyboardShortcuts.onKeyDown(for: .dictation) { [weak self] in self?.onPress?() }
        KeyboardShortcuts.onKeyUp(for: .dictation) { [weak self] in self?.onRelease?() }
        KeyboardShortcuts.onKeyDown(for: .cancelDictation) { [weak self] in self?.onCancel?() }
        KeyboardShortcuts.disable(.cancelDictation)
        fnMonitor.onChange = { [weak self] down in
            if down { self?.onFnPress?() } else { self?.onFnRelease?() }
        }
    }

    func setCancelEnabled(_ enabled: Bool) {
        if enabled { KeyboardShortcuts.enable(.cancelDictation) } else { KeyboardShortcuts.disable(.cancelDictation) }
    }

    /// Starts/stops the Fn tap. Returns false when Input Monitoring is missing.
    @discardableResult
    func setFnEnabled(_ enabled: Bool) -> Bool {
        enabled ? fnMonitor.start() : { fnMonitor.stop(); return true }()
    }

    var fnActive: Bool { fnMonitor.isRunning }

    /// Seeds the KeyboardShortcuts binding from an imported Tauri accelerator string once.
    static func adoptAccelerator(_ accelerator: String) {
        guard let parsed = AcceleratorString(parsing: accelerator), let key = key(for: parsed.code) else { return }
        var mods: NSEvent.ModifierFlags = []
        if parsed.modifiers.contains(.command) { mods.insert(.command) }
        if parsed.modifiers.contains(.shift) { mods.insert(.shift) }
        if parsed.modifiers.contains(.option) { mods.insert(.option) }
        if parsed.modifiers.contains(.control) { mods.insert(.control) }
        KeyboardShortcuts.setShortcut(.init(key, modifiers: mods), for: .dictation)
    }

    /// The current binding in the shared schema's accelerator format.
    static func currentAccelerator() -> String? {
        guard let shortcut = KeyboardShortcuts.getShortcut(for: .dictation), let key = shortcut.key,
              let code = codes.first(where: { $0.value == key })?.key else { return nil }
        var mods: AcceleratorString.Modifiers = []
        if shortcut.modifiers.contains(.command) { mods.insert(.command) }
        if shortcut.modifiers.contains(.shift) { mods.insert(.shift) }
        if shortcut.modifiers.contains(.option) { mods.insert(.option) }
        if shortcut.modifiers.contains(.control) { mods.insert(.control) }
        return AcceleratorString(modifiers: mods, code: code).string
    }

    static var currentShortcutSymbols: String {
        KeyboardShortcuts.getShortcut(for: .dictation)?.description ?? "Not set"
    }

    private static func key(for code: String) -> KeyboardShortcuts.Key? { codes[code] }

    private static let codes: [String: KeyboardShortcuts.Key] = {
        var map: [String: KeyboardShortcuts.Key] = [
            "Digit0": .zero, "Digit1": .one, "Digit2": .two, "Digit3": .three, "Digit4": .four,
            "Digit5": .five, "Digit6": .six, "Digit7": .seven, "Digit8": .eight, "Digit9": .nine,
            "Space": .space, "Escape": .escape, "Enter": .return, "Tab": .tab,
            "F1": .f1, "F2": .f2, "F3": .f3, "F4": .f4, "F5": .f5, "F6": .f6,
            "F7": .f7, "F8": .f8, "F9": .f9, "F10": .f10, "F11": .f11, "F12": .f12,
            "Semicolon": .semicolon, "Period": .period, "Comma": .comma, "Slash": .slash,
            "Minus": .minus, "Equal": .equal, "Backquote": .backtick,
        ]
        let letters: [KeyboardShortcuts.Key] = [.a, .b, .c, .d, .e, .f, .g, .h, .i, .j, .k, .l, .m,
                                                .n, .o, .p, .q, .r, .s, .t, .u, .v, .w, .x, .y, .z]
        for (i, key) in letters.enumerated() {
            map["Key\(Character(UnicodeScalar(UInt8(65 + i))))"] = key
        }
        return map
    }()
}

/// Listen-only `CGEventTap` on `flagsChanged`, watching keycode 63 (Fn/Globe), exactly as
/// `macos_input.rs` does. Needs Input Monitoring (not Accessibility). Re-enables itself
/// when macOS disables the tap for timeout or user input.
@MainActor
final class FnKeyMonitor {
    var onChange: ((Bool) -> Void)?
    private var tap: CFMachPort?
    private var source: CFRunLoopSource?
    private var fnDown = false
    private static let log = Logger(subsystem: AppInfo.bundleID, category: "hotkeys")

    var isRunning: Bool { tap != nil }

    func start() -> Bool {
        guard tap == nil else { return true }
        guard CGPreflightListenEventAccess() else { return false }
        let mask = CGEventMask(1 << CGEventType.flagsChanged.rawValue)
        let refcon = Unmanaged.passUnretained(self).toOpaque()
        guard let tap = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap, options: .listenOnly,
                                          eventsOfInterest: mask, callback: fnTapCallback, userInfo: refcon) else {
            Self.log.error("Creating the Fn event tap failed")
            return false
        }
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        self.tap = tap
        self.source = source
        return true
    }

    func stop() {
        if let source { CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes) }
        if let tap { CFMachPortInvalidate(tap) }
        tap = nil
        source = nil
        if fnDown { fnDown = false; onChange?(false) }
    }

    fileprivate func handle(type: CGEventType, keyCode: Int64, flags: CGEventFlags) {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
            return
        }
        guard type == .flagsChanged, keyCode == 63 else { return }
        let down = flags.contains(.maskSecondaryFn)
        guard down != fnDown else { return }
        fnDown = down
        onChange?(down)
    }
}

/// C callback; the tap's run-loop source is on the main run loop, so this runs on main.
nonisolated private func fnTapCallback(proxy: CGEventTapProxy, type: CGEventType, event: CGEvent, refcon: UnsafeMutableRawPointer?) -> Unmanaged<CGEvent>? {
    if let refcon {
        let monitor = Unmanaged<FnKeyMonitor>.fromOpaque(refcon).takeUnretainedValue()
        let keyCode = event.getIntegerValueField(.keyboardEventKeycode)
        let flags = event.flags
        MainActor.assumeIsolated { monitor.handle(type: type, keyCode: keyCode, flags: flags) }
    }
    return Unmanaged.passUnretained(event)
}
