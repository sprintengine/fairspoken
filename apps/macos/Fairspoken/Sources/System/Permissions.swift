import AppKit
import ApplicationServices
import AVFoundation
import CoreGraphics
import Observation

/// Live status of the three privacy grants the app needs. Never bypasses TCC: it only
/// asks (system prompts) and deep-links to System Settings, then polls while visible.
@Observable
final class Permissions {
    enum Status: Equatable {
        case granted
        case denied
        case notDetermined

        var isGranted: Bool { self == .granted }
    }

    enum Kind: String, CaseIterable, Identifiable {
        case microphone
        case accessibility
        case inputMonitoring
        var id: String { rawValue }
    }

    private(set) var microphone: Status = .notDetermined
    private(set) var accessibility: Status = .notDetermined
    private(set) var inputMonitoring: Status = .notDetermined

    @ObservationIgnored private var pollers = 0
    @ObservationIgnored private var timer: Timer?

    init() { refresh() }

    func status(_ kind: Kind) -> Status {
        switch kind {
        case .microphone: microphone
        case .accessibility: accessibility
        case .inputMonitoring: inputMonitoring
        }
    }

    func refresh() {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: set(\.microphone, .granted)
        case .notDetermined: set(\.microphone, .notDetermined)
        default: set(\.microphone, .denied)
        }
        set(\.accessibility, AXIsProcessTrusted() ? .granted : .denied)
        set(\.inputMonitoring, CGPreflightListenEventAccess() ? .granted : .denied)
    }

    private func set(_ key: ReferenceWritableKeyPath<Permissions, Status>, _ value: Status) {
        if self[keyPath: key] != value { self[keyPath: key] = value }
    }

    func request(_ kind: Kind) {
        switch kind {
        case .microphone:
            if microphone == .notDetermined {
                AVCaptureDevice.requestAccess(for: .audio) { _ in
                    Task { @MainActor in self.refresh() }
                }
            } else {
                openSettings(kind)
            }
        case .accessibility:
            // Shows the system prompt (once) and adds the app to the list, switched off.
            let options = ["AXTrustedCheckOptionPrompt": true] as CFDictionary
            if !AXIsProcessTrustedWithOptions(options) { openSettings(kind) }
        case .inputMonitoring:
            if !CGRequestListenEventAccess() { openSettings(kind) }
        }
        refresh()
    }

    func openSettings(_ kind: Kind) {
        let anchor = switch kind {
        case .microphone: "Privacy_Microphone"
        case .accessibility: "Privacy_Accessibility"
        case .inputMonitoring: "Privacy_ListenEvent"
        }
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") {
            NSWorkspace.shared.open(url)
        }
    }

    /// Ref-counted 1 s polling while onboarding/settings are on screen (cheap calls).
    func beginPolling() {
        pollers += 1
        guard timer == nil else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
            MainActor.assumeIsolated { self.refresh() }
        }
    }

    func endPolling() {
        pollers = max(0, pollers - 1)
        if pollers == 0 {
            timer?.invalidate()
            timer = nil
        }
    }

    /// The Globe/Fn key's own action (`com.apple.HIToolbox AppleFnUsageType`):
    /// 0 Do Nothing, 1 Change Input Source, 2 Show Emoji & Symbols, 3 Start Dictation.
    static var globeKeyAction: Int {
        UserDefaults(suiteName: "com.apple.HIToolbox")?.integer(forKey: "AppleFnUsageType") ?? 0
    }

    static var globeKeyActionName: String {
        switch globeKeyAction {
        case 0: "Do Nothing"
        case 1: "Change Input Source"
        case 2: "Show Emoji & Symbols"
        case 3: "Start Dictation"
        default: "another action"
        }
    }
}
