import FairspokenUI
import AppKit
import MultiVoiceCore

/// Menu-bar item: live status, start/stop, and doors into the dashboard.
@MainActor
final class StatusItemController: NSObject, NSMenuDelegate {
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let model: AppModel
    private let windows: WindowCoordinator
    private let statusLine = NSMenuItem(title: "", action: nil, keyEquivalent: "")
    private let toggleItem = NSMenuItem(title: "Start Dictation", action: #selector(toggleDictation), keyEquivalent: "")

    init(model: AppModel, windows: WindowCoordinator) {
        self.model = model
        self.windows = windows
        super.init()
        item.button?.setAccessibilityLabel(AppInfo.displayName)
        let menu = NSMenu()
        menu.delegate = self
        statusLine.isEnabled = false
        menu.addItem(statusLine)
        menu.addItem(.separator())
        toggleItem.target = self
        menu.addItem(toggleItem)
        menu.addItem(.separator())
        menu.addItem(entry("Open \(AppInfo.displayName)", #selector(openHome), "o"))
        menu.addItem(entry("Models", #selector(openModels), ""))
        menu.addItem(entry("Settings…", #selector(openSettings), ","))
        menu.addItem(.separator())
        menu.addItem(entry("Setup & Permissions…", #selector(openOnboarding), ""))
        menu.addItem(.separator())
        menu.addItem(entry("Quit \(AppInfo.displayName)", #selector(quit), "q"))
        item.menu = menu
        update(for: .idle)
    }

    private func entry(_ title: String, _ action: Selector, _ key: String) -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
        i.target = self
        return i
    }

    func update(for phase: DictationController.Phase) {
        let symbol: String
        switch phase {
        case .listening: symbol = "mic.fill"
        case .transcribing: symbol = "ellipsis.circle"
        case .failed: symbol = "exclamationmark.triangle"
        default: symbol = "waveform"
        }
        let image = NSImage(systemSymbolName: symbol, accessibilityDescription: AppInfo.displayName)
        image?.isTemplate = true
        item.button?.image = image
        item.button?.contentTintColor = phase.isListening ? .systemRed : nil
    }

    func menuWillOpen(_ menu: NSMenu) {
        let state = model.models.engineState
        let remote = model.settings.settings.transcriptionLocation == .remoteHost
        let engine = remote ? "Your host" : "\(model.models.activeModel.shortName) · \(state.isReady ? "Ready" : state.label)"
        statusLine.title = "\(AppInfo.displayName) — \(engine)"
        toggleItem.title = model.dictation.phase.isListening ? "Stop Dictation" : "Start Dictation   \(HotkeyController.currentShortcutSymbols)"
    }

    @objc private func toggleDictation() { model.dictation.toggleFromUI() }
    @objc private func openHome() { windows.showDashboard(.home) }
    @objc private func openModels() { windows.showDashboard(.models) }
    @objc private func openSettings() { windows.showDashboard(.settings) }
    @objc private func openOnboarding() { windows.showOnboarding() }
    @objc private func quit() { NSApp.terminate(nil) }
}
