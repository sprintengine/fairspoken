import FairspokenUI
import FairspokenCore
import FairspokenHost
import AppKit
import SwiftUI

@MainActor
final class ServerAppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate, NSWindowDelegate {
    private var controller: ServerController!
    private var window: NSWindow?
    private var statusItem: NSStatusItem?
    private let statusLine = NSMenuItem(title: "", action: nil, keyEquivalent: "")
    private let addressLine = NSMenuItem(title: "", action: nil, keyEquivalent: "")
    private let toggleItem = NSMenuItem(title: "", action: #selector(toggleServing), keyEquivalent: "")
    private let loginItem = NSMenuItem(title: "Start at Login", action: #selector(toggleLoginItem), keyEquivalent: "")
    private var harness: ServerScreenshotHarness?
    private var terminating = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        controller = ServerController()
        NSApp.mainMenu = buildMainMenu()
        installStatusItem()

        if let dir = ServerInfo.screenshotDirectory {
            harness = ServerScreenshotHarness(controller: controller, delegate: self, output: dir)
            harness?.run()
            return
        }
        Task {
            await controller.start()
            if controller.configIssue != nil || !controller.runState.isRunning { showWindow() }
        }
        let firstLaunch = !UserDefaults.standard.bool(forKey: "launchedBefore")
        UserDefaults.standard.set(true, forKey: "launchedBefore")
        if firstLaunch || ServerInfo.isDemo || ServerInfo.arguments.contains("--show-window") {
            showWindow()
        }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { showWindow() }
        return true
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminating, controller.runtime != nil else { return .terminateNow }
        terminating = true
        Task {
            await controller.stop()
            NSApp.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    // MARK: Window

    func showWindow(_ section: ServerController.Section? = nil) {
        if let section { controller.section = section }
        if window == nil {
            let hosting = NSHostingController(rootView: ServerWindowView().environment(controller))
            hosting.sceneBridgingOptions = [.toolbars]
            let w = NSWindow(contentViewController: hosting)
            w.styleMask = [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView]
            w.title = ServerInfo.displayName
            w.titleVisibility = .hidden
            w.titlebarAppearsTransparent = true
            w.toolbar = NSToolbar(identifier: "server")
            w.toolbarStyle = .unified
            w.isReleasedWhenClosed = false
            w.setContentSize(NSSize(width: 1360, height: 920))
            w.minSize = NSSize(width: 1120, height: 760)
            w.setFrameAutosaveName("ServerWindow")
            if !w.setFrameUsingName("ServerWindow") { w.center() }
            w.delegate = self
            window = w
        }
        NSApp.setActivationPolicy(.regular)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    var mainWindow: NSWindow? { window }

    /// Screenshot harness: a fresh hosting controller renders even when the window server has
    /// paused updates for an occluded window.
    func rebuildWindowContent() {
        guard let window else { return }
        let frame = window.frame
        let hosting = NSHostingController(rootView: ServerWindowView().environment(controller))
        hosting.sceneBridgingOptions = [.toolbars]
        window.contentViewController = hosting
        window.setFrame(frame, display: true)
    }

    func windowWillClose(_ notification: Notification) {
        DispatchQueue.main.async {
            if self.window?.isVisible != true { NSApp.setActivationPolicy(.accessory) }
        }
    }

    // MARK: Menu bar item

    private func installStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        let image = NSImage(systemSymbolName: "server.rack", accessibilityDescription: ServerInfo.displayName)
        image?.isTemplate = true
        item.button?.image = image
        item.button?.setAccessibilityLabel(ServerInfo.displayName)
        let menu = NSMenu()
        menu.delegate = self
        statusLine.isEnabled = false
        addressLine.isEnabled = false
        menu.addItem(statusLine)
        menu.addItem(addressLine)
        menu.addItem(.separator())
        menu.addItem(entry("Open \(ServerInfo.displayName)", #selector(openWindow), "o"))
        menu.addItem(entry("Copy Server Address", #selector(copyAddress), ""))
        menu.addItem(entry("Copy Token", #selector(copyToken), ""))
        menu.addItem(.separator())
        toggleItem.target = self
        menu.addItem(toggleItem)
        loginItem.target = self
        menu.addItem(loginItem)
        menu.addItem(.separator())
        menu.addItem(entry("Quit \(ServerInfo.displayName)", #selector(quit), "q"))
        item.menu = menu
        statusItem = item
    }

    private func entry(_ title: String, _ action: Selector, _ key: String) -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
        i.target = self
        return i
    }

    func menuWillOpen(_ menu: NSMenu) {
        let c = controller!
        switch c.runState {
        case .running:
            let busy = c.stats.runningJobs
            statusLine.title = "Serving · \(c.configuration.workerCount) worker\(c.configuration.workerCount == 1 ? "" : "s")"
                + (busy > 0 ? " · \(busy) busy" : "")
        case .starting: statusLine.title = "Starting…"
        case .stopped: statusLine.title = "Stopped"
        case .failed(let m): statusLine.title = m
        }
        addressLine.title = c.primaryEndpoint?.url ?? ""
        addressLine.isHidden = !c.runState.isRunning
        toggleItem.title = c.runState.isRunning ? "Stop Serving" : "Start Serving"
        loginItem.state = LoginItem.isEnabled ? .on : .off
    }

    @objc private func openWindow() { showWindow() }
    @objc private func copyAddress() { controller.primaryEndpoint.map { controller.copy($0.url) } }
    @objc private func copyToken() { controller.copy(controller.configuration.token) }
    @objc private func toggleServing() {
        Task { controller.runState.isRunning ? await controller.stop() : await controller.start() }
    }
    @objc private func toggleLoginItem() { controller.setLoginItem(!LoginItem.isEnabled) }
    @objc private func quit() { NSApp.terminate(nil) }
    @objc private func showConfiguration() { showWindow(.configuration) }

    private func buildMainMenu() -> NSMenu {
        let main = NSMenu()
        let name = ServerInfo.displayName
        let appMenu = NSMenu(title: name)
        appMenu.addItem(NSMenuItem(title: "About \(name)", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: ""))
        appMenu.addItem(.separator())
        appMenu.addItem(entry("Settings…", #selector(showConfiguration), ","))
        appMenu.addItem(.separator())
        appMenu.addItem(NSMenuItem(title: "Hide \(name)", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h"))
        appMenu.addItem(NSMenuItem(title: "Quit \(name)", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        main.addItem(submenu(appMenu))
        let edit = NSMenu(title: "Edit")
        edit.addItem(NSMenuItem(title: "Undo", action: Selector(("undo:")), keyEquivalent: "z"))
        edit.addItem(NSMenuItem(title: "Redo", action: Selector(("redo:")), keyEquivalent: "Z"))
        edit.addItem(.separator())
        edit.addItem(NSMenuItem(title: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x"))
        edit.addItem(NSMenuItem(title: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c"))
        edit.addItem(NSMenuItem(title: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v"))
        edit.addItem(NSMenuItem(title: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a"))
        main.addItem(submenu(edit))
        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(NSMenuItem(title: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m"))
        windowMenu.addItem(NSMenuItem(title: "Close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w"))
        main.addItem(submenu(windowMenu))
        NSApp.windowsMenu = windowMenu
        return main
    }

    private func submenu(_ menu: NSMenu) -> NSMenuItem {
        let item = NSMenuItem(title: menu.title, action: nil, keyEquivalent: "")
        item.submenu = menu
        return item
    }
}
