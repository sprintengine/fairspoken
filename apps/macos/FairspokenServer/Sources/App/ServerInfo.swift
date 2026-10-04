import MultiVoiceCore
import FairspokenHost
import Foundation

/// Identity of this build, read from the bundle (`MV_DISPLAY_NAME` in Config/Server.xcconfig).
nonisolated enum ServerInfo {
    static let displayName: String =
        (Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String) ?? "Fairspoken Server"
    static let bundleID: String = Bundle.main.bundleIdentifier ?? "ie.fairspoken.server"
    static let version: String = (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String) ?? "0.0.0"
    static let build: String = (Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String) ?? "0"
    static var isDevBuild: Bool { bundleID.hasSuffix(".dev") }

    static let arguments = ProcessInfo.processInfo.arguments
    static let environment = ProcessInfo.processInfo.environment
    /// `--headless`: serve without any UI (LaunchAgent / LaunchDaemon use).
    static let isHeadless = arguments.contains("--headless")
    /// `--demo`: the window shows the simulated practice host; the server still runs.
    static let isDemo = arguments.contains("--demo") || arguments.contains("--screenshots")
    static var screenshotDirectory: URL? {
        guard let i = arguments.firstIndex(of: "--screenshots"), i + 1 < arguments.count else { return nil }
        return URL(fileURLWithPath: arguments[i + 1], isDirectory: true)
    }

    /// `host-config.json`: `MULTIVOICE_HOST_CONFIG_PATH`, else Application Support/<bundle id>/.
    static var configURL: URL {
        if let i = arguments.firstIndex(of: "--config"), i + 1 < arguments.count { return URL(fileURLWithPath: arguments[i + 1]) }
        // Screenshots never read or write the real configuration.
        if screenshotDirectory != nil && !arguments.contains("--live") {
            return FileManager.default.temporaryDirectory.appendingPathComponent("fairspoken-server-screenshots/host-config.json")
        }
        return HostConfigurationStore.defaultURL(bundleID: bundleID, environment: environment)
    }

    /// The web dashboard (src-tauri/src/host/dashboard.html, bundled at build time).
    static let dashboardHTML: [UInt8] = {
        guard let url = Bundle.main.url(forResource: "dashboard", withExtension: "html"), let data = try? Data(contentsOf: url) else {
            return Array("<!doctype html><title>Fairspoken Server</title><p>The dashboard file is missing from this build.</p>".utf8)
        }
        return [UInt8](data)
    }()
}
