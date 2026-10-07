import FairspokenUI
import FairspokenUpdates
import AppKit
import FairspokenCore
import SwiftUI

/// Menu-bar item: live status, start/stop, and doors into the dashboard.
@MainActor
final class StatusItemController: NSObject, NSMenuDelegate {
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let model: AppModel
    private let windows: WindowCoordinator
    private let statusLine = NSMenuItem(title: "", action: nil, keyEquivalent: "")
    private let toggleItem = NSMenuItem(title: "Start Dictation", action: #selector(toggleDictation), keyEquivalent: "")
    private let updateItem = NSMenuItem(title: "Check for Updates…", action: #selector(updateAction), keyEquivalent: "")

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
        menu.addItem(entry("Setup Guide…", #selector(openOnboarding), ""))
        updateItem.target = self
        menu.addItem(updateItem)
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
        item.button?.contentTintColor = phase.isListening ? NSColor(Crystal.live) : nil
    }

    func menuWillOpen(_ menu: NSMenu) {
        let state = model.models.engineState
        let remote = model.settings.settings.transcriptionLocation == .remoteHost
        statusLine.title = remote ? "My host · \(model.hostStatus.stateLabel)"
            : "\(model.models.activeModel.shortName) · \(state.label)"
        toggleItem.title = model.dictation.phase.isListening ? "Stop Dictation" : "Start Dictation   \(HotkeyController.currentShortcutSymbols)"
        let update = model.updates.presentation
        updateItem.title = update.menuTitle
        updateItem.setAccessibilityLabel(update.accessibilityLabel)
    }

    @objc private func toggleDictation() { model.dictation.toggleFromUI() }
    @objc private func openHome() { windows.showDashboard(.home) }
    @objc private func openModels() { windows.showDashboard(.models) }
    @objc private func openSettings() { windows.showDashboard(.settings) }
    @objc private func openOnboarding() { windows.showOnboarding() }
    /// Check, install or restart, by state; the window shows the progress and the answer.
    @objc private func updateAction() {
        windows.showDashboard()
        model.updates.performPrimaryAction()
    }
    @objc private func quit() { NSApp.terminate(nil) }
}

extension StatusItemController: NSMenuItemValidation {
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        menuItem == updateItem ? model.updates.presentation.isEnabled : true
    }
}
