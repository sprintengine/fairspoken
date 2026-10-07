import FairspokenUI
import FairspokenUpdates
import AppKit
import FairspokenCore
import SwiftUI

/// `--screenshots <dir>`: renders the real windows (demo data)
/// in light and dark, captures each window's own pixels and quits. `--screenshots <dir> --updates`
/// captures the update UI instead: every update state, the toast and Settings › Updates. Capturing our *own*
/// windows needs no Screen Recording permission, so this works from the command line.
@MainActor
final class ScreenshotHarness {
    private let model: AppModel
    private let windows: WindowCoordinator
    private let output: URL
    private var showcaseWindow: NSWindow?

    init(model: AppModel, windows: WindowCoordinator, output: URL) {
        self.model = model
        self.windows = windows
        self.output = output
    }

    func run() {
        try? FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        model.models.prepare(model.settings.settings.model)
        NSApp.setActivationPolicy(.regular)
        Task {
            windows.showDashboard(.home)
            // Activation is cooperative on macOS 14+; keep asking until we're frontmost so
            // windows render in their active state.
            for _ in 0..<40 where !NSApp.isActive {
                NSApp.activate()
                // Dev-only: the cooperative API can't take focus from a terminal; the legacy
                // call still can. Invoked dynamically to keep the build warning-free.
                _ = NSApp.perform(NSSelectorFromString("activateIgnoringOtherApps:"), with: true as NSNumber)
                try? await Task.sleep(for: .milliseconds(100))
            }
            try? await Task.sleep(for: .seconds(1))
            if AppInfo.arguments.contains("--updates") {
                await updateShots()
                NSApp.terminate(nil)
                return
            }
            // Let the engine finish warming so status chips read "Ready".
            for _ in 0..<150 where !model.models.engineState.isReady {
                try? await Task.sleep(for: .milliseconds(100))
            }
            for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", NSAppearance.Name.darkAqua)] {
                NSApp.appearance = NSAppearance(named: appearance)
                await dashboardShots(suffix: name)
                await onboardingShots(suffix: name)
                await pillShots(suffix: name)
            }
            NSApp.terminate(nil)
        }
    }

    private func dashboardShots(suffix: String) async {
        model.dictation.showcase(.idle, inAppResult: .init(
            text: "Please book a follow-up appointment in ten days and request a chest X-ray.",
            words: 14, delivery: .shownInApp, latencyMs: 74, placement: .neuralEngine))
        for section in AppModel.Section.allCases {
            windows.showDashboard(section)
            if section == .settings { model.settingsTab = .general }
            try? await Task.sleep(for: .milliseconds(1600))
            await freshFrame()
            if let window = windows.dashboard { await capture(window, as: "\(section.rawValue)-\(suffix)") }
        }
        // Home while listening, to show the live mic rings.
        windows.showDashboard(.home)
        model.dictation.showcase(.listening(since: Date().addingTimeInterval(-7)))
        let feeder = feedLevels(into: model.dictation.meter)
        try? await Task.sleep(for: .milliseconds(1400))
        await freshFrame()
        if let window = windows.dashboard { await capture(window, as: "home-listening-\(suffix)") }
        feeder.cancel()
        model.dictation.showcase(.idle)
        model.settingsTab = .transcription
        windows.showDashboard(.settings)
        try? await Task.sleep(for: .milliseconds(1000))
        await freshFrame()
        if let window = windows.dashboard { await capture(window, as: "settings-transcription-\(suffix)") }
        // The same tab with My host chosen (the sample host; no tailnet scan, so no real machines).
        let location = model.settings.settings.transcriptionLocation
        model.settings.update { $0.transcriptionLocation = .remoteHost }
        model.hostStatus.refresh(force: true)
        try? await Task.sleep(for: .milliseconds(800))
        await freshFrame()
        if let window = windows.dashboard { await capture(window, as: "settings-host-\(suffix)") }
        model.settings.update { $0.transcriptionLocation = location }
        windows.dashboard?.orderOut(nil)
    }

    /// The sidebar update button in each state (the window, Home), the available toast, and
    /// Settings › Updates with an update ready. Simulated: Sparkle is never started here.
    private func updateShots() async {
        for (suffix, appearance) in [("light", NSAppearance.Name.aqua), ("dark", NSAppearance.Name.darkAqua)] {
            NSApp.appearance = NSAppearance(named: appearance)
            for name in UpdateState.sampleNames {
                guard let state = UpdateState.sample(named: name) else { continue }
                model.updates.simulate(state, toast: name == "available")
                windows.showDashboard(.home)
                try? await Task.sleep(for: .milliseconds(900))
                await freshFrame()
                if let window = windows.dashboard { await capture(window, as: "update-\(name)-\(suffix)") }
                model.updates.dismissToast()
            }
            model.updates.simulate(.available(version: "0.3.0"))
            model.settingsTab = .updates
            windows.showDashboard(.settings)
            try? await Task.sleep(for: .milliseconds(1000))
            await freshFrame()
            if let window = windows.dashboard { await capture(window, as: "settings-updates-\(suffix)") }
        }
    }

    private func onboardingShots(suffix: String) async {
        for step in [OnboardingView.Step.welcome, .permissions] {
            let hosting = NSHostingController(rootView: OnboardingView(initialStep: step) {}.environment(model))
            let window = NSWindow(contentViewController: hosting)
            window.styleMask = [.titled, .closable, .fullSizeContentView]
            window.titlebarAppearsTransparent = true
            window.titleVisibility = .hidden
            window.isReleasedWhenClosed = false
            window.center()
            window.makeKeyAndOrderFront(nil)
            try? await Task.sleep(for: .milliseconds(1200))
            await capture(window, as: "onboarding-\(step == .welcome ? "welcome" : "permissions")-\(suffix)")
            window.orderOut(nil)
        }
    }

    /// The pill states over a wallpaper-like backdrop (the real pill samples the desktop,
    /// which a window capture can't include, so it is staged in a showcase window).
    private func pillShots(suffix: String) async {
        let phases: [(String, DictationController.Phase)] = [
            ("listening", .listening(since: Date().addingTimeInterval(-12))),
            ("transcribing", .transcribing),
            ("inserted", .done(.init(text: "x", words: 38, delivery: .pasted(app: "Mail"), latencyMs: 68, placement: .neuralEngine))),
            ("error", .failed("No speech detected")),
        ]
        var controllers: [DictationController] = []
        var feeders: [Task<Void, Never>] = []
        for (_, phase) in phases {
            let c = DictationController(settings: model.settings, models: model.models, history: model.history, permissions: model.permissions)
            c.showcase(phase)
            feeders.append(feedLevels(into: c.meter))
            controllers.append(c)
        }
        let view = PillShowcase(controllers: controllers, shortcut: HotkeyController.currentShortcutSymbols)
        let hosting = NSHostingController(rootView: view)
        let window = NSWindow(contentViewController: hosting)
        window.styleMask = [.titled, .fullSizeContentView]
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isReleasedWhenClosed = false
        window.setContentSize(NSSize(width: 760, height: 470))
        window.center()
        window.makeKeyAndOrderFront(nil)
        showcaseWindow = window
        try? await Task.sleep(for: .milliseconds(1500))
        window.contentViewController = NSHostingController(rootView: PillShowcase(controllers: controllers, shortcut: HotkeyController.currentShortcutSymbols))
        window.setContentSize(NSSize(width: 760, height: 470))
        try? await Task.sleep(for: .milliseconds(250))
        await capture(window, as: "pill-\(suffix)")
        feeders.forEach { $0.cancel() }
        window.orderOut(nil)
    }

    private func feedLevels(into meter: LevelMeter) -> Task<Void, Never> {
        Task {
            var t = 0.0
            while !Task.isCancelled {
                t += 0.05
                let speech = 0.5 + 0.5 * sin(t * 3.1) * sin(t * 1.3 + 0.6)
                meter.push(rms: Float(0.004 + 0.09 * max(0, speech) * (0.7 + 0.3 * sin(t * 11))))
                try? await Task.sleep(for: .milliseconds(30))
            }
        }
    }

    /// When the session's windows are occluded (screen locked, display asleep) AppKit stops
    /// driving SwiftUI frame updates; a fresh hosting controller always renders once.
    private func freshFrame() async {
        if windows.dashboard != nil {
            windows.rebuildDashboardContent()
            try? await Task.sleep(for: .milliseconds(350))
        }
    }

    // MARK: Capture

    private typealias CreateImage = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?

    /// `CGWindowListCreateImage` is unavailable to Swift in the macOS 15+ SDK but still works at
    /// runtime for the calling app's own windows (no Screen Recording grant needed). Dev-only.
    private func capture(_ window: NSWindow, as name: String) async {
        // Activation is cooperative and can lag; wait (briefly) for the window to be key so it
        // renders in its active state (coloured controls, prominent buttons).
        for _ in 0..<20 where !window.isKeyWindow || !NSApp.isActive {
            window.makeKeyAndOrderFront(nil)
            _ = NSApp.perform(NSSelectorFromString("activateIgnoringOtherApps:"), with: true as NSNumber)
            try? await Task.sleep(for: .milliseconds(100))
        }
        window.displayIfNeeded()
        guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage") else { return }
        let create = unsafeBitCast(symbol, to: CreateImage.self)
        // includingWindow = 1 << 3; boundsIgnoreFraming = 1 << 0, bestResolution = 1 << 3
        guard let image = create(.null, 1 << 3, UInt32(window.windowNumber), (1 << 0) | (1 << 3))?.takeRetainedValue() else { return }
        let rep = NSBitmapImageRep(cgImage: image)
        guard let png = rep.representation(using: .png, properties: [:]) else { return }
        try? png.write(to: output.appendingPathComponent("\(name).png"))
    }
}

private struct PillShowcase: View {
    var controllers: [DictationController]
    var shortcut: String

    var body: some View {
        ZStack {
            // A quiet, wallpaper-like backdrop: cool slate with a soft sage glow.
            LinearGradient(colors: [Color(red: 0.36, green: 0.42, blue: 0.47), Color(red: 0.58, green: 0.63, blue: 0.66),
                                    Color(red: 0.80, green: 0.82, blue: 0.80)],
                           startPoint: .topLeading, endPoint: .bottomTrailing)
            Circle().fill(Color(red: 0.55, green: 0.75, blue: 0.64).opacity(0.35)).frame(width: 420).blur(radius: 90).offset(x: -220, y: -120)
            VStack(spacing: 18) {
                ForEach(Array(controllers.enumerated()), id: \.offset) { _, c in
                    PillView(dictation: c, shortcut: shortcut).frame(width: 520, height: 76)
                }
            }
            .padding(.top, 20)
        }
        .ignoresSafeArea()
    }
}
