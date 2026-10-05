import AppKit
import SwiftUI

/// Owns the dashboard and onboarding windows and switches the activation policy:
/// regular (Dock + ⌘Tab) while one is open, accessory (menu bar only) otherwise.
@MainActor
final class WindowCoordinator: NSObject, NSWindowDelegate {
    private let model: AppModel
    private(set) var dashboard: NSWindow?
    private(set) var onboarding: NSWindow?

    init(model: AppModel) {
        self.model = model
        super.init()
    }

    func showDashboard(_ section: AppModel.Section? = nil) {
        if let section { model.section = section }
        if dashboard == nil {
            let hosting = NSHostingController(rootView: DashboardView().environment(model))
            hosting.sceneBridgingOptions = [.toolbars]
            let window = NSWindow(contentViewController: hosting)
            window.styleMask = [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView]
            window.title = AppInfo.displayName
            window.titleVisibility = .hidden
            window.titlebarAppearsTransparent = true
            window.toolbar = NSToolbar(identifier: "dashboard")
            window.toolbarStyle = .unified
            window.isReleasedWhenClosed = false
            window.setContentSize(NSSize(width: 1280, height: 900))
            window.minSize = NSSize(width: 1080, height: 740)
            window.setFrameAutosaveName("Dashboard")
            if !window.setFrameUsingName("Dashboard") { window.center() }
            window.delegate = self
            window.identifier = NSUserInterfaceItemIdentifier("dashboard")
            dashboard = window
        }
        present(dashboard!)
    }

    func showOnboarding() {
        if onboarding == nil {
            let view = OnboardingView { [weak self] in
                self?.onboarding?.close()
                self?.showDashboard(.home)
            }
            .environment(model)
            let hosting = NSHostingController(rootView: view)
            let window = NSWindow(contentViewController: hosting)
            window.styleMask = [.titled, .closable, .fullSizeContentView]
            window.title = "Welcome to \(AppInfo.displayName)"
            window.titleVisibility = .hidden
            window.titlebarAppearsTransparent = true
            window.isMovableByWindowBackground = true
            window.isReleasedWhenClosed = false
            window.center()
            window.delegate = self
            onboarding = window
        }
        present(onboarding!)
    }

    /// Screenshot harness: swap in a fresh hosting controller so the next frame reflects the
    /// current state even when the window server has paused updates for an occluded window.
    func rebuildDashboardContent() {
        guard let window = dashboard else { return }
        let frame = window.frame
        let hosting = NSHostingController(rootView: DashboardView().environment(model))
        hosting.sceneBridgingOptions = [.toolbars]
        window.contentViewController = hosting
        window.setFrame(frame, display: true)
    }

    private func present(_ window: NSWindow) {
        NSApp.setActivationPolicy(.regular)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    func windowWillClose(_ notification: Notification) {
        guard let window = notification.object as? NSWindow else { return }
        if window == onboarding { onboarding = nil }
        DispatchQueue.main.async { [weak self] in self?.updatePolicy() }
    }

    private func updatePolicy() {
        let anyVisible = [dashboard, onboarding].contains { $0?.isVisible == true }
        if !anyVisible { NSApp.setActivationPolicy(.accessory) }
    }
}
