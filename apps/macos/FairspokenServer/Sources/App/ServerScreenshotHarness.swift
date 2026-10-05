import FairspokenUI
import AppKit
import FairspokenHost
import FairspokenCore
import SwiftUI

/// `--screenshots <dir>`: renders every section in light and dark with the demo simulator,
/// a sample configuration and sample addresses (no port is bound), captures the window's
/// own pixels and quits. Capturing our own window needs no Screen Recording permission.
@MainActor
final class ServerScreenshotHarness {
    private let controller: ServerController
    private let delegate: ServerAppDelegate
    private let output: URL

    init(controller: ServerController, delegate: ServerAppDelegate, output: URL) {
        self.controller = controller
        self.delegate = delegate
        self.output = output
    }

    func run() {
        try? FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        if ServerInfo.arguments.contains("--live") { return runLive() }
        controller.prepareForScreenshots()
        NSApp.setActivationPolicy(.regular)
        Task {
            delegate.showWindow(.activity)
            for _ in 0..<40 where !NSApp.isActive {
                NSApp.activate()
                _ = NSApp.perform(NSSelectorFromString("activateIgnoringOtherApps:"), with: true as NSNumber)
                try? await Task.sleep(for: .milliseconds(100))
            }
            try? await Task.sleep(for: .seconds(1))
            for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", NSAppearance.Name.darkAqua)] {
                NSApp.appearance = NSAppearance(named: appearance)
                for section in ServerController.Section.allCases {
                    delegate.showWindow(section)
                    try? await Task.sleep(for: .milliseconds(section == .activity ? 3500 : 1500))
                    if section == .activity { await waitForTraffic() }
                    await freshFrame()
                    if let window = delegate.mainWindow { capture(window, as: "\(section.rawValue)-\(name)") }
                }
            }
            NSApp.terminate(nil)
        }
    }

    /// `--screenshots <dir> --live [--config file]`: the real server and the real window (light
    /// only), for checking the in-process feed while a test drives traffic. Not for docs.
    private func runLive() {
        controller.source = .live
        NSApp.setActivationPolicy(.regular)
        Task {
            await controller.start()
            delegate.showWindow(.activity)
            try? await Task.sleep(for: .seconds(1))
            NSApp.appearance = NSAppearance(named: .aqua)
            for round in 0..<3 {
                try? await Task.sleep(for: .seconds(8))
                for section in [ServerController.Section.activity, .clients, .models] {
                    delegate.showWindow(section)
                    try? await Task.sleep(for: .milliseconds(700))
                    await freshFrame()
                    if let window = delegate.mainWindow { capture(window, as: "live-\(round)-\(section.rawValue)") }
                }
            }
            await controller.stop()
            NSApp.terminate(nil)
        }
    }

    /// Catch a frame with comets in flight and someone speaking.
    private func waitForTraffic() async {
        for _ in 0..<250 {
            let now = FlowAnimator.now()
            let flying = controller.animator.particles.filter { now > $0.start && now < $0.start + $0.duration }.count
            if flying >= 3 && !controller.live.streams.isEmpty && controller.live.workers.contains(where: \.isBusy) { return }
            try? await Task.sleep(for: .milliseconds(40))
        }
    }

    private func freshFrame() async {
        NSApp.activate()
        _ = NSApp.perform(NSSelectorFromString("activateIgnoringOtherApps:"), with: true as NSNumber)
        delegate.rebuildWindowContent()
        try? await Task.sleep(for: .milliseconds(450))
    }

    private typealias CreateImage = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?

    /// `CGWindowListCreateImage` is unavailable to Swift in the macOS 15+ SDK but still works at
    /// runtime for the calling app's own windows. Dev-only.
    private func capture(_ window: NSWindow, as name: String) {
        window.makeKeyAndOrderFront(nil)
        _ = NSApp.perform(NSSelectorFromString("activateIgnoringOtherApps:"), with: true as NSNumber)
        window.displayIfNeeded()
        guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage") else { return }
        let create = unsafeBitCast(symbol, to: CreateImage.self)
        guard let image = create(.null, 1 << 3, UInt32(window.windowNumber), (1 << 0) | (1 << 3))?.takeRetainedValue() else { return }
        let rep = NSBitmapImageRep(cgImage: image)
        guard let png = rep.representation(using: .png, properties: [:]) else { return }
        try? png.write(to: output.appendingPathComponent("\(name).png"))
    }
}
