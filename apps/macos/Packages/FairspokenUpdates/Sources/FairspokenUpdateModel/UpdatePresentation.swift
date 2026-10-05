import Foundation

/// How the sidebar update button, its menu twin and Settings show a state (contract §6):
///
/// | State       | Icon                         | Badge       | Click             |
/// |-------------|------------------------------|-------------|-------------------|
/// | idle        | sync                         | –           | check now         |
/// | checking    | sync, spinning               | –           | – (disabled)      |
/// | available   | download                     | `1`         | download, install |
/// | downloading | download inside a ring       | –           | –                 |
/// | ready       | restart                      | dot         | restart           |
/// | error       | sync                         | warning dot | retry the check   |
public struct UpdatePresentation: Equatable, Sendable {
    public enum Badge: Equatable, Sendable {
        case none
        /// The small numeric badge ("1").
        case count(Int)
        /// An accent dot: something is ready.
        case dot
        /// A warning dot: the last attempt failed.
        case warning
    }

    /// SF Symbol name.
    public var symbol: String
    public var spinning: Bool
    public var badge: Badge
    /// Shows the progress ring; nil progress with `showsRing` is an indeterminate ring.
    public var showsRing: Bool
    public var progress: Double?
    public var isEnabled: Bool
    /// Short caption beside the icon ("Update available").
    public var title: String
    /// Second line ("Fairspoken 0.3.0 (Nightly)").
    public var detail: String
    /// Tooltip and VoiceOver label: the state and the versions.
    public var accessibilityLabel: String
    /// The status menu's item for this state.
    public var menuTitle: String

    public static let syncSymbol = "arrow.triangle.2.circlepath"
    public static let downloadSymbol = "tray.and.arrow.down"
    public static let restartSymbol = "arrow.clockwise"

    public static func make(state: UpdateState, appName: String, currentVersion: String, channel: UpdateChannel) -> UpdatePresentation {
        let current = "\(appName) \(currentVersion)"
        switch state {
        case .idle:
            return UpdatePresentation(
                symbol: syncSymbol, spinning: false, badge: .none, showsRing: false, progress: nil, isEnabled: true,
                title: "Check for updates", detail: "\(currentVersion) · \(channel.title)",
                accessibilityLabel: "\(current) (\(channel.title)) — click to check for updates",
                menuTitle: "Check for Updates…")
        case .checking:
            return UpdatePresentation(
                symbol: syncSymbol, spinning: true, badge: .none, showsRing: false, progress: nil, isEnabled: false,
                title: "Checking…", detail: "\(currentVersion) · \(channel.title)",
                accessibilityLabel: "Checking for updates to \(current) (\(channel.title))",
                menuTitle: "Checking for Updates…")
        case .available(let version):
            let name = updateDisplayName(appName: appName, version: version)
            return UpdatePresentation(
                symbol: downloadSymbol, spinning: false, badge: .count(1), showsRing: false, progress: nil, isEnabled: true,
                title: "Update available", detail: name,
                accessibilityLabel: "Update available: \(name) — click to install",
                menuTitle: "Install \(name)…")
        case .downloading(let version, let progress):
            let name = updateDisplayName(appName: appName, version: version)
            let percent = progress.map { " \(Int(($0 * 100).rounded())) %" } ?? ""
            return UpdatePresentation(
                symbol: downloadSymbol, spinning: false, badge: .none, showsRing: true, progress: progress, isEnabled: false,
                title: "Downloading…\(percent)", detail: name,
                accessibilityLabel: "Downloading \(name)\(percent.isEmpty ? "" : ",\(percent)")",
                menuTitle: "Downloading \(name)…\(percent)")
        case .ready(let version):
            let name = updateDisplayName(appName: appName, version: version)
            return UpdatePresentation(
                symbol: restartSymbol, spinning: false, badge: .dot, showsRing: false, progress: nil, isEnabled: true,
                title: "Restart to update", detail: name,
                accessibilityLabel: "\(name) is ready — click to restart and finish updating",
                menuTitle: "Restart to Install \(name)")
        case .error(let message):
            return UpdatePresentation(
                symbol: syncSymbol, spinning: false, badge: .warning, showsRing: false, progress: nil, isEnabled: true,
                title: "Update check failed", detail: message,
                accessibilityLabel: "Couldn't check for updates to \(current): \(message) — click to try again",
                menuTitle: "Check for Updates Again…")
        }
    }
}
