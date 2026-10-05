import Foundation
import FairspokenCore
import Observation

/// Reachability of the user's own transcription host ("My host"): one `GET /v1/health`
/// when asked (Settings, Home and the Test button), never a background poll.
@Observable
final class HostStatus {
    enum State: Equatable {
        case notConfigured
        case checking
        case connected(version: String?)
        case failed(String)
    }

    private(set) var state: State = .notConfigured
    private(set) var checkedAt: Date?
    @ObservationIgnored private let settings: SettingsStore
    @ObservationIgnored private var task: Task<Void, Never>?

    init(settings: SettingsStore) {
        self.settings = settings
        if !settings.settings.remoteUrl.isEmpty { state = .checking }
    }

    /// The host's name as the user typed it: `practice-mini.local:48173`.
    var hostName: String {
        guard let url = URL(string: settings.settings.remoteUrl.trimmingCharacters(in: .whitespaces)), let host = url.host() else {
            return settings.settings.remoteUrl
        }
        return url.port.map { "\(host):\($0)" } ?? host
    }

    /// "Using host: practice-mini.local:48173 · connected".
    var summary: String {
        switch state {
        case .notConfigured: "No host set"
        case .checking: "Using host: \(hostName) · checking…"
        case .connected: "Using host: \(hostName) · connected"
        case .failed: "Using host: \(hostName) · not reachable"
        }
    }

    var detail: String? {
        switch state {
        case .connected(let version): version.map { "Server version \($0)" }
        case .failed(let message): message
        default: nil
        }
    }

    /// Checks now; repeated calls within 20 s reuse the last answer unless `force`.
    func refresh(force: Bool = false) {
        let url = settings.settings.remoteUrl.trimmingCharacters(in: .whitespaces)
        guard !url.isEmpty else {
            task?.cancel()
            state = .notConfigured
            return
        }
        if AppInfo.screenshotDirectory != nil {
            state = .connected(version: AppInfo.version)
            return
        }
        if !force, let checkedAt, Date().timeIntervalSince(checkedAt) < 20, state != .checking { return }
        let token = settings.remoteToken
        task?.cancel()
        state = .checking
        task = Task { [weak self] in
            let result: State
            do {
                let health = try await RemoteHostClient.health(baseURL: url, token: token)
                result = health.ok ? .connected(version: health.serverVersion) : .failed("The host says it isn't ready")
            } catch is CancellationError {
                return
            } catch {
                result = .failed(error.localizedDescription)
            }
            guard let self, !Task.isCancelled else { return }
            self.state = result
            self.checkedAt = Date()
        }
    }
}
