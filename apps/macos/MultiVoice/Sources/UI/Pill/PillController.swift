import FairspokenUI
import AppKit
import SwiftUI

/// Non-activating, borderless panel that floats above everything on every Space.
/// Non-activation is the critical property: focus never leaves the target app, so ⌘V
/// lands where the user was typing (plan §2.1).
final class PillPanel: NSPanel {
    init(contentRect: NSRect) {
        super.init(contentRect: contentRect, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        isFloatingPanel = true
        level = .statusBar
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        hidesOnDeactivate = false
        becomesKeyOnlyIfNeeded = true
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        ignoresMouseEvents = true
        animationBehavior = .none
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class PillController {
    private let panel: PillPanel
    private var hideWork: DispatchWorkItem?
    private static let size = NSSize(width: 520, height: 76)

    init(model: AppModel) {
        panel = PillPanel(contentRect: NSRect(origin: .zero, size: Self.size))
        let root = PillView(dictation: model.dictation, shortcut: HotkeyController.currentShortcutSymbols)
        let host = NSHostingView(rootView: root)
        host.sizingOptions = []
        host.frame = NSRect(origin: .zero, size: Self.size)
        panel.contentView = host
        panel.setAccessibilityLabel("Dictation status")
    }

    func update(for phase: DictationController.Phase) {
        hideWork?.cancel()
        if phase == .idle {
            let work = DispatchWorkItem { [weak self] in self?.hide() }
            hideWork = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: work)
        } else {
            show()
        }
    }

    private func show() {
        if !panel.isVisible {
            position()
            panel.alphaValue = 0
            panel.orderFrontRegardless()
        }
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.18
            panel.animator().alphaValue = 1
        }
    }

    private func hide() {
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.25
            panel.animator().alphaValue = 0
        }, completionHandler: { [panel] in
            MainActor.assumeIsolated { if panel.alphaValue == 0 { panel.orderOut(nil) } }
        })
    }

    /// Bottom centre of the screen the pointer is on, just above the Dock.
    private func position() {
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { NSMouseInRect(mouse, $0.frame, false) } ?? NSScreen.main
        guard let visible = screen?.visibleFrame else { return }
        let origin = NSPoint(x: visible.midX - Self.size.width / 2, y: visible.minY + 28)
        panel.setFrameOrigin(origin)
    }
}
