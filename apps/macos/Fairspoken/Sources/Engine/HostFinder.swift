import Foundation
import FairspokenCore
import Observation
import SystemConfiguration

/// Settings › Transcription › Find my host: scans the tailnet for Fairspoken hosts (or probes
/// a typed machine name), pairs with a password, and saves the result as "My host".
@Observable
final class HostFinder {
    enum Scan: Equatable {
        case idle
        case searching
        case done
        case failed(String)
    }

    private(set) var scan: Scan = .idle
    /// What the last scan found, plus hosts looked up by name.
    private(set) var hosts: [DiscoveredHost] = []
    private(set) var lookingUp = false
    private(set) var lookupError: String?

    /// The host whose password prompt is showing.
    var pairing: DiscoveredHost? {
        didSet { if pairing?.id != oldValue?.id { pairingError = nil } }
    }
    private(set) var pairingBusy = false
    private(set) var pairingError: String?
    /// A host chosen that takes a token: its URL is saved and the token field waits.
    private(set) var needsToken: DiscoveredHost?

    @ObservationIgnored private let settings: SettingsStore
    @ObservationIgnored private let hostStatus: HostStatus
    @ObservationIgnored private let discovery = TailnetDiscovery()
    @ObservationIgnored private let pairingClient = HostPairingClient()
    @ObservationIgnored private var scanTask: Task<Void, Never>?
    /// Hosts looked up by name, kept in the list across rescans.
    @ObservationIgnored private var typedIDs: Set<DiscoveredHost.ID> = []

    init(settings: SettingsStore, hostStatus: HostStatus) {
        self.settings = settings
        self.hostStatus = hostStatus
    }

    /// This Mac's name as System Settings › Sharing shows it ("Conal's MacBook"), sent as `clientName`.
    nonisolated static var computerName: String {
        (SCDynamicStoreCopyComputerName(nil, nil) as String?) ?? ProcessInfo.processInfo.hostName
    }

    func findHosts() {
        scanTask?.cancel()
        scan = .searching
        scanTask = Task { [weak self, discovery] in
            let result: Result<[DiscoveredHost], any Error>
            do {
                result = .success(try await discovery.discover())
            } catch {
                result = .failure(error)
            }
            guard let self, !Task.isCancelled else { return }
            switch result {
            case .success(let found):
                // Keep hosts looked up by name that the scan didn't see.
                let foundIDs = Set(found.map(\.id))
                hosts = found + hosts.filter { typedIDs.contains($0.id) && !foundIDs.contains($0.id) }
                scan = .done
            case .failure(let error):
                scan = .failed(error.localizedDescription)
            }
        }
    }

    /// Probes a typed machine name or `name:port` and, when a host answers, chooses it.
    func lookUp(_ entry: String) {
        lookingUp = true
        lookupError = nil
        Task { [discovery] in
            defer { lookingUp = false }
            do {
                let host = try await discovery.probe(entry: entry)
                typedIDs.insert(host.id)
                hosts.removeAll { $0.id == host.id }
                hosts.append(host)
                choose(host)
            } catch {
                lookupError = error.localizedDescription
            }
        }
    }

    /// What picking a host does depends on its `auth`: none saves it, password asks for the
    /// pairing password, token saves the URL and leaves the token to the user.
    func choose(_ host: DiscoveredHost) {
        needsToken = nil
        switch host.auth {
        case .open:
            use(host.url, token: "")
        case .password:
            pairing = host
        case .token:
            settings.update { $0.remoteUrl = host.url.absoluteString }
            settings.remoteToken = ""
            hostStatus.refresh(force: true)
            needsToken = host
        }
    }

    func pair(password: String) {
        guard let host = pairing, !pairingBusy else { return }
        pairingBusy = true
        pairingError = nil
        Task { [pairingClient] in
            defer { pairingBusy = false }
            do {
                let response = try await pairingClient.pair(url: host.url, password: password, clientName: Self.computerName)
                // `token: null` means the host has none: nothing to pair, the URL is enough.
                use(host.url, token: response.token ?? "")
                pairing = nil
            } catch {
                pairingError = error.localizedDescription
            }
        }
    }

    func cancelPairing() {
        pairing = nil
    }

    /// Saves the host as "My host" (token in the Keychain), switches dictation to it and tests it.
    private func use(_ url: URL, token: String) {
        settings.update {
            $0.remoteUrl = url.absoluteString
            $0.transcriptionLocation = .remoteHost
        }
        settings.remoteToken = token
        hostStatus.refresh(force: true)
    }
}
