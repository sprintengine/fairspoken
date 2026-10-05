import Foundation

/// Identity of this build. The display name is read from the bundle so it is defined
/// once (`MV_DISPLAY_NAME` in Config/Base.xcconfig) and never hard-coded in Swift.
nonisolated enum AppInfo {
    static let displayName: String =
        (Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String) ?? "Fairspoken"
    static let bundleID: String = Bundle.main.bundleIdentifier ?? "ie.fairspoken.mac"
    static let version: String = (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String) ?? "0.0.0"
    static let build: String = (Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String) ?? "0"
    static var isDevBuild: Bool { bundleID.hasSuffix(".dev") }

    /// `~/Library/Application Support/<bundle id>/` (plan §2.8).
    static let supportDirectory: URL = {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent(bundleID, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }()

    /// Where the Tauri app keeps its JSON on macOS (`~/.config/fairspoken/`), imported once.
    /// Its builds before the rename used `~/.config/multivoice-tauri/`, read if that is all there is.
    static let tauriDirectory: URL = {
        let config = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".config", isDirectory: true)
        let current = config.appendingPathComponent("fairspoken", isDirectory: true)
        let legacy = config.appendingPathComponent("multivoice-tauri", isDirectory: true)
        let exists = { (url: URL) in FileManager.default.fileExists(atPath: url.path) }
        return !exists(current) && exists(legacy) ? legacy : current
    }()

    static let arguments = ProcessInfo.processInfo.arguments
    /// `--demo`: seed sample dictations and stats (nothing persisted).
    static let isDemo = arguments.contains("--demo") || arguments.contains("--screenshots")
    static var screenshotDirectory: URL? {
        guard let i = arguments.firstIndex(of: "--screenshots"), i + 1 < arguments.count else { return nil }
        return URL(fileURLWithPath: arguments[i + 1], isDirectory: true)
    }
}
