import Foundation
import FairspokenCore
import Observation
import OSLog

/// Owns `settings.json` (shared schema with the Rust app) and the Keychain-held token.
/// Unknown keys are preserved on save; writes are atomic, 0600 and debounced.
@Observable
final class SettingsStore {
    var settings: AppSettings {
        didSet {
            guard settings != oldValue else { return }
            scheduleSave()
        }
    }

    /// Remote host bearer token (Keychain, never in the JSON).
    var remoteToken: String {
        didSet {
            guard remoteToken != oldValue, persists else { return }
            Keychain.write(remoteToken.trimmingCharacters(in: .whitespacesAndNewlines), account: "remoteAuthToken")
        }
    }

    /// True when this launch imported the Tauri app's settings.
    private(set) var importedFromTauri = false

    @ObservationIgnored private var raw: [String: JSONValue] = [:]
    @ObservationIgnored private var saveTask: Task<Void, Never>?
    @ObservationIgnored private let url: URL
    @ObservationIgnored private let persists: Bool
    private static let log = Logger(subsystem: AppInfo.bundleID, category: "settings")

    init(directory: URL = AppInfo.supportDirectory, persists: Bool = !AppInfo.isDemo) {
        url = directory.appendingPathComponent("settings.json")
        self.persists = persists
        var loaded = AppSettings()
        var token = persists ? (Keychain.read("remoteAuthToken") ?? "") : ""
        var imported = false
        if let data = try? Data(contentsOf: url) {
            raw = (try? JSONDecoder().decode(JSONValue.self, from: data))?.objectValue ?? [:]
            loaded = (try? JSONDecoder().decode(AppSettings.self, from: data)) ?? AppSettings()
        } else if let data = try? Data(contentsOf: AppInfo.tauriDirectory.appendingPathComponent("settings.json")),
                  let decoded = try? JSONDecoder().decode(AppSettings.self, from: data) {
            // One-time import from the Tauri app (plan §2.8).
            raw = (try? JSONDecoder().decode(JSONValue.self, from: data))?.objectValue ?? [:]
            loaded = decoded
            imported = true
            Self.log.info("Imported settings from the Tauri app")
        }
        // Secrets move to the Keychain; the JSON keeps an empty string.
        if !loaded.remoteAuthToken.isEmpty {
            if token.isEmpty { token = loaded.remoteAuthToken.trimmingCharacters(in: .whitespacesAndNewlines) }
            loaded.remoteAuthToken = ""
            if persists { Keychain.write(token, account: "remoteAuthToken") }
        }
        raw["remoteAuthToken"] = .string("")
        if case .string(let cloud)? = raw["cloudAuthToken"], !cloud.isEmpty {
            if persists { Keychain.write(cloud, account: "cloudAuthToken") }
            raw["cloudAuthToken"] = .string("")
            loaded.cloudAuthToken = ""
        }
        // Screenshots show a sample practice host, not the user's own.
        if AppInfo.screenshotDirectory != nil { loaded.remoteUrl = "http://practice-mini.local:48173" }
        settings = loaded
        remoteToken = token
        importedFromTauri = imported
        if imported { saveNow() }
    }

    func update(_ change: (inout AppSettings) -> Void) {
        var next = settings
        change(&next)
        settings = next.normalized()
    }

    private func scheduleSave() {
        guard persists else { return }
        saveTask?.cancel()
        saveTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            self?.saveNow()
        }
    }

    func saveNow() {
        guard persists else { return }
        do {
            var copy = settings
            copy.remoteAuthToken = ""
            copy.cloudAuthToken = ""
            let data = try JSONMerge.encode(copy, over: raw)
            try data.write(to: url, options: [.atomic])
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        } catch {
            Self.log.error("Saving settings failed: \(error.localizedDescription, privacy: .public)")
        }
    }
}
