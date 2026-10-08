import AppKit
import FairspokenHost
import Foundation
import FairspokenCore
import Observation
import OSLog

/// Composition root of Fairspoken Server: owns the running `TranscriptionHost`, its
/// configuration file, and the render-ready `HostLiveState` the window shows. The window
/// reads the host in-process (the same snapshot + event frames `/v1/events` sends, decoded
/// by the same `HostEvent` / `HostLiveState` code the client uses), or the simulator in demo mode.
@Observable
final class ServerController {
    enum RunState: Equatable {
        case stopped
        case starting
        case running
        case failed(String)

        var isRunning: Bool { self == .running }
    }

    enum Source: String, CaseIterable, Identifiable {
        case live
        case demo
        var id: String { rawValue }
    }

    enum Section: String, CaseIterable, Identifiable {
        case activity, clients, models, connect, configuration
        var id: String { rawValue }
        var title: String {
            switch self {
            case .activity: "Activity"
            case .clients: "Clients"
            case .models: "Models"
            case .connect: "Connect"
            case .configuration: "Configuration"
            }
        }
        var symbol: String {
            switch self {
            case .activity: "point.3.connected.trianglepath.dotted"
            case .clients: "laptopcomputer.and.iphone"
            case .models: "cube"
            case .connect: "qrcode"
            case .configuration: "slider.horizontal.3"
            }
        }
    }

    var section: Section = .activity
    private(set) var runState: RunState = .stopped
    var source: Source {
        didSet { if source != oldValue { restartFeed() } }
    }
    /// The saved configuration (what the running host uses, unless a restart is pending).
    private(set) var configuration: HostConfiguration
    /// The config file could not be read; the server won't start until it is fixed or reset.
    private(set) var configIssue: String?
    private(set) var live = HostLiveState()
    /// Latest `/v1/stats` body, refreshed every 2 s, for the tables.
    private(set) var stats = HostStats()
    private(set) var addresses = NetworkAddresses.current()
    private(set) var loginItemEnabled = LoginItem.isEnabled
    /// Recent `POST /v1/pair` attempts, newest first (refreshed with `stats`).
    private(set) var pairingAttempts: [PairingAttempt] = []
    private(set) var notice: String?
    @ObservationIgnored let animator = FlowAnimator()
    @ObservationIgnored let backend = FluidAudioBackend()
    @ObservationIgnored let sleepGuard = SleepGuard()
    @ObservationIgnored private var host: TranscriptionHost?
    @ObservationIgnored private var hostConfiguration: HostConfiguration?
    @ObservationIgnored private var feedTask: Task<Void, Never>?
    @ObservationIgnored private var statsTask: Task<Void, Never>?
    @ObservationIgnored private var simulator = HostSimulator()
    @ObservationIgnored let configURL = ServerInfo.configURL
    nonisolated private static let log = Logger(subsystem: "ie.fairspoken.server", category: "server")

    init() {
        source = ServerInfo.isDemo ? .demo : .live
        configuration = HostConfiguration()
        loadConfiguration()
    }

    var knownModels: Set<String> { Set(backend.catalog.map(\.id)) }
    var runtime: HostRuntime? { host?.runtime }

    // MARK: Configuration file

    private func loadConfiguration() {
        do {
            if let file = try HostConfigurationStore.load(configURL) {
                configuration = file.normalized(knownModels: knownModels)
            } else {
                // First launch: a random token, saved so clients can be set up once.
                var fresh = try HostConfigurationStore.resolve(file: nil, environment: ServerInfo.environment, knownModels: knownModels)
                if fresh.token.isEmpty { fresh.token = HostConfiguration.generateToken() }
                try HostConfigurationStore.save(fresh, to: configURL)
                configuration = fresh
            }
            configIssue = nil
        } catch {
            configIssue = error.localizedDescription
        }
    }

    /// Replaces an unreadable config file with defaults (and a new token).
    func resetConfiguration() async {
        var fresh = HostConfiguration()
        fresh.token = HostConfiguration.generateToken()
        do {
            try HostConfigurationStore.save(fresh, to: configURL)
            configuration = fresh
            configIssue = nil
            await start()
        } catch {
            notice = error.localizedDescription
        }
    }

    // MARK: Lifecycle

    func start() async {
        // `host` is only set once the bind finishes, so a second start while one is in flight
        // (a double click on the menu item) is refused by state, not by `host`.
        guard host == nil, configIssue == nil, runState != .starting, runState != .running else { return }
        runState = .starting
        do {
            let file = try HostConfigurationStore.load(configURL)
            let config = try HostConfigurationStore.resolve(file: file, environment: ServerInfo.environment, knownModels: knownModels)
            let started = try await TranscriptionHost.start(configuration: config, configURL: configURL, backend: backend,
                                                            dashboardHTML: ServerInfo.dashboardHTML, serverVersion: ServerInfo.version)
            host = started
            // The runtime's copy: it holds the token generated for a pairing password, if any.
            hostConfiguration = started.runtime.configuration
            configuration = started.runtime.configuration
            started.runtime.setLiveChangeHandler { [weak self] settings in
                Task { @MainActor in self?.adoptLiveSettings(settings) }
            }
            runState = .running
            applySleepGuard()
            Self.log.info("Serving on \(config.bindAddr, privacy: .public)")
        } catch {
            runState = .failed(error.localizedDescription)
            Self.log.error("Start failed: \(error.localizedDescription, privacy: .public)")
        }
        addresses = .current()
        restartFeed()
    }

    func stop() async {
        guard let host else { return }
        self.host = nil
        hostConfiguration = nil
        await host.stop()
        runState = .stopped
        sleepGuard.release()
        restartFeed()
    }

    func restart() async {
        await stop()
        await start()
    }

    private func applySleepGuard() {
        if runState.isRunning && configuration.preventSleep {
            sleepGuard.hold(reason: "\(ServerInfo.displayName) is serving transcription requests")
        } else {
            sleepGuard.release()
        }
    }

    private func adoptLiveSettings(_ s: HostLiveSettings) {
        configuration.maxActiveStreams = s.maxActiveStreams
        configuration.maxRecordingSeconds = s.maxRecordingSeconds
        configuration.useGpu = s.useGpu
        configuration.workerModels = s.workerModels
        // `POST /v1/config` may also have set the pairing password (and with it a new token).
        if let credentials = runtime?.credentials {
            configuration.pairingPassword = credentials.pairingPassword ?? ""
            if let token = credentials.token, token != configuration.authToken {
                configuration.token = token
                hostConfiguration?.token = token
            }
        }
    }

    // MARK: Editing

    enum SaveOutcome: Equatable {
        case saved
        case restarted
        case failed(String)
    }

    /// Saves an edited configuration: live settings apply at once (the same path as
    /// `POST /v1/config`), the rest restarts the server.
    func save(_ draft: HostConfiguration) async -> SaveOutcome {
        var next = draft
        next.bindAddress = next.bindAddress.trimmingCharacters(in: .whitespaces)
        next.token = next.token.trimmingCharacters(in: .whitespacesAndNewlines)
        if let problem = Self.validate(next) { return .failed(problem) }
        next = next.normalized(knownModels: knownModels)
        // Pairing hands out the token, so a pairing password needs one.
        if next.pairingEnabled && next.token.isEmpty { next.token = HostConfiguration.generateToken() }
        do { try HostConfigurationStore.save(next, to: configURL) } catch { return .failed(error.localizedDescription) }
        let needsRestart = hostConfiguration.map { running in
            running.workerCount != next.workerCount || running.queueCapacity != next.queueCapacity
                || running.bindAddress != next.bindAddress || running.port != next.port || running.token != next.token
        } ?? false
        configuration = next
        if needsRestart {
            await restart()
            return .restarted
        }
        if let runtime {
            do {
                _ = try runtime.applyConfigUpdate(HostConfigUpdate(maxActiveStreams: next.maxActiveStreams, maxRecordingSeconds: next.maxRecordingSeconds,
                                                                   useGpu: next.useGpu, workerModels: next.workerModels,
                                                                   pairingPassword: next.pairingPassword))
            } catch {
                return .failed("\(error)")
            }
            runtime.setDisplayName(next.displayName)
        }
        applySleepGuard()
        return .saved
    }

    static func validate(_ c: HostConfiguration) -> String? {
        guard (1...65535).contains(c.port) else { return "The port must be between 1 and 65535." }
        let a = c.bindAddress
        let isIP = a.split(separator: ".").count == 4 && a.split(separator: ".").allSatisfy { UInt8($0) != nil }
        guard isIP || a == "::" || a == "::1" || a.contains(":") else {
            return "The address must be an IP address of this Mac: 127.0.0.1, 0.0.0.0 or one interface's address."
        }
        guard HostConfiguration.workerCountRange.contains(c.workerCount) else { return "Use 1 to 8 workers." }
        guard HostConfiguration.queueCapacityRange.contains(c.queueCapacity) else { return "The queue holds 1 to 64 jobs." }
        if HostConfiguration.pairingPasswordProblem(c.pairingPassword) != nil { return pairingPasswordHint }
        return nil
    }

    static let pairingPasswordHint = "The pairing password needs \(HostConfiguration.pairingPasswordLength.lowerBound) to \(HostConfiguration.pairingPasswordLength.upperBound) characters. Six digits are fine."

    /// Who can reach the server: the one choice on the Connect screen.
    enum Access: Equatable {
        /// 127.0.0.1: only apps on this Mac.
        case thisMac
        /// This Mac's Tailscale address (the host also answers on 127.0.0.1 there).
        case tailnet
        /// Anything else (every interface, one LAN address), set under Configuration › Advanced.
        case custom
    }

    var access: Access {
        if isLoopbackOnly { return .thisMac }
        if NetworkAddresses.isTailscale(configuration.bindAddress) { return .tailnet }
        return .custom
    }

    /// "This Mac only", "Tailnet" or the address, for the toolbar and the menu bar item.
    var accessSummary: String {
        switch access {
        case .thisMac: "This Mac only"
        case .tailnet: "This Mac and tailnet"
        case .custom: configuration.bindAddr
        }
    }

    /// Saves the choice and restarts the server on the new address.
    func setAccess(_ access: Access) async -> SaveOutcome {
        switch access {
        case .thisMac:
            var next = configuration
            next.bindAddress = "127.0.0.1"
            return await save(next)
        case .tailnet:
            return await listenOnTailscale()
        case .custom:
            return .saved
        }
    }

    /// Listens on this Mac's Tailscale address (default port), where clients scanning the
    /// tailnet look. Restarts the server.
    func listenOnTailscale() async -> SaveOutcome {
        refreshAddresses()
        guard let tailscale = addresses.tailscale else { return .failed("Tailscale isn't connected on this Mac.") }
        var next = configuration
        next.bindAddress = tailscale.address
        next.port = HostConfiguration.defaultPort
        return await save(next)
    }

    /// Sets, changes or (with "") turns off the pairing password. Applies at once, unless
    /// pairing needs a new token, which restarts the server.
    func setPairingPassword(_ password: String) async -> SaveOutcome {
        var next = configuration
        next.pairingPassword = password
        return await save(next)
    }

    func setPreventSleep(_ on: Bool) {
        var next = configuration
        next.preventSleep = on
        try? HostConfigurationStore.save(next, to: configURL)
        configuration = next
        applySleepGuard()
    }

    func setLoginItem(_ on: Bool) {
        do {
            try LoginItem.set(on)
            notice = LoginItem.needsApproval ? "Allow \(ServerInfo.displayName) in System Settings › General › Login Items." : nil
        } catch {
            notice = "Couldn't change the login item: \(error.localizedDescription)"
        }
        loginItemEnabled = LoginItem.isEnabled
    }

    func dismissNotice() { notice = nil }

    // MARK: Models

    func download(_ model: String) {
        guard let runtime else { return }
        do { try runtime.startDownload(model) } catch {
            notice = error == .inProgress ? "A model download is already in progress." : "Couldn't download \(model)."
        }
    }

    func delete(_ model: String) {
        guard let runtime else { return }
        do {
            try runtime.deleteModel(model)
            refreshStats()
        } catch {
            notice = "Couldn't delete the model: \(error.localizedDescription)"
        }
    }

    func assign(model: String, toWorker index: Int) {
        guard let runtime else { return }
        var models = runtime.liveSettings.workerModels
        guard models.indices.contains(index) else { return }
        models[index] = model
        do { _ = try runtime.applyConfigUpdate(HostConfigUpdate(workerModels: models)) } catch {
            notice = "\(error)"
        }
    }

    // MARK: Feed

    private func restartFeed() {
        feedTask?.cancel()
        statsTask?.cancel()
        animator.reset()
        live = HostLiveState()
        stats = HostStats()
        pairingAttempts = []
        switch source {
        case .demo: startDemo()
        case .live: startLive()
        }
    }

    private func ingest(_ events: [HostEvent]) {
        guard !events.isEmpty else { return }
        let now = Date().timeIntervalSince1970 * 1000
        var next = live
        var cues: [FlowCue] = []
        for event in events { cues += next.apply(event, now: now) }
        live = next
        animator.play(cues)
    }

    private func startLive() {
        guard let runtime else { return }
        let (snapshot, frames) = runtime.observe()
        if let stats = Self.decodeStats(snapshot.serialized) {
            self.stats = stats
            ingest([.snapshot(stats)])
        }
        feedTask = Task { [weak self] in
            var parser = SSEParser()
            for await frame in frames {
                var batch: [HostEvent] = []
                for message in parser.feed(frame) {
                    if case .event(let event) = message, let decoded = try? HostEvent.decode(event) { batch.append(decoded) }
                }
                guard let self, !Task.isCancelled else { return }
                self.ingest(batch)
            }
        }
        statsTask = Task { [weak self] in
            var tick = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(2))
                guard let self, !Task.isCancelled else { return }
                tick += 1
                self.refreshStats(reconcile: tick % 5 == 0)
            }
        }
    }

    func refreshStats(reconcile: Bool = false) {
        if source == .demo {
            stats = simulator.snapshot()
            return
        }
        guard let runtime, let s = Self.decodeStats(runtime.statsJSON().serialized) else { return }
        stats = s
        pairingAttempts = runtime.pairingAttempts()
        if reconcile { live.reconcile(with: s, now: Date().timeIntervalSince1970 * 1000) }
    }

    static func decodeStats(_ json: String) -> HostStats? {
        try? JSONDecoder().decode(HostStats.self, from: Data(json.utf8))
    }

    private func startDemo() {
        // The simulated clock is the wall clock: it starts a few seconds in the past and is
        // pre-rolled to now, so the first frame already shows traffic.
        var config = HostSimulator.Config()
        config.meanArrivalSeconds = 2.0
        let wall = Date().timeIntervalSince1970 * 1000
        var sim = HostSimulator(config: config, now: wall - 6_000)
        ingest([.snapshot(sim.snapshot())])
        ingest(sim.advance(to: wall))
        animator.reset()
        stats = sim.snapshot()
        simulator = sim
        feedTask = Task { [weak self] in
            var tick = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(100))
                guard let self else { return }
                let now = Date().timeIntervalSince1970 * 1000
                self.ingest(self.simulator.advance(to: now))
                tick += 1
                if tick % 20 == 0 { self.stats = self.simulator.snapshot() }
            }
        }
    }

    // MARK: Addresses

    struct Endpoint: Identifiable, Equatable {
        var label: String
        var url: String
        var isLoopback = false
        var id: String { url }
    }

    func refreshAddresses() {
        if presenting { return }
        addresses = .current()
    }

    // MARK: Screenshots

    @ObservationIgnored private var presenting = false
    /// Screenshot mode: Configuration opens with Advanced expanded.
    @ObservationIgnored var revealAdvanced = false

    /// Screenshot mode: the demo simulator, a sample configuration and sample addresses,
    /// shown as serving. Nothing is bound and nothing is written to the real config file.
    func prepareForScreenshots() {
        presenting = true
        var sample = HostConfiguration()
        sample.bindAddress = "100.101.102.103"
        sample.workerCount = 3
        sample.workerModels = ["parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v3", "parakeet-ultra"]
        sample.token = "k7pq2m9xw4hr8tcv3nd6jy5bfz2ga8es" // gitleaks:allow (made-up sample for screenshots)
        sample.pairingPassword = "482913" // gitleaks:allow (made-up sample for screenshots)
        sample.displayName = "Practice mini"
        configuration = sample
        configIssue = nil
        addresses = Self.sampleAddresses(tailscale: true)
        runState = .running
        source = .demo
        restartFeed()
    }

    /// Screenshot mode: shows Connect as "This Mac only" on a Mac without Tailscale (or back).
    func presentSampleAccess(tailnet: Bool) {
        guard presenting else { return }
        addresses = Self.sampleAddresses(tailscale: tailnet)
        configuration.bindAddress = tailnet ? "100.101.102.103" : "127.0.0.1"
        configuration.pairingPassword = tailnet ? "482913" : "" // gitleaks:allow (made-up sample)
    }

    private static func sampleAddresses(tailscale: Bool) -> NetworkAddresses {
        var interfaces: [NetworkAddresses.Interface] = [.init(name: "en0", address: "192.168.1.20", kind: .lan)]
        if tailscale { interfaces.append(.init(name: "utun4", address: "100.101.102.103", kind: .tailscale)) }
        return NetworkAddresses(localHostName: "practice-mini.local", interfaces: interfaces)
    }

    /// URLs clients use to reach this server, for the Connect panel and the menu. Bound to
    /// one non-loopback address, the host also listens on 127.0.0.1, so apps on this Mac keep
    /// working.
    var endpoints: [Endpoint] {
        let port = configuration.port
        let bind = configuration.bindAddress
        func url(_ host: String) -> String { host.contains(":") ? "http://[\(host)]:\(port)" : "http://\(host):\(port)" }
        let thisMac = Endpoint(label: "This Mac", url: url("127.0.0.1"), isLoopback: true)
        switch bind {
        case "127.0.0.1", "::1":
            return [Endpoint(label: "This Mac", url: url(bind), isLoopback: true)]
        case "0.0.0.0", "::":
            var list = [thisMac]
            if let t = addresses.tailscale { list.append(Endpoint(label: "Tailnet", url: url(t.address))) }
            if let name = addresses.localHostName { list.append(Endpoint(label: "Local name", url: url(name))) }
            for i in addresses.lan { list.append(Endpoint(label: "Local network (\(i.name))", url: url(i.address))) }
            return list
        default:
            return [thisMac, Endpoint(label: NetworkAddresses.isTailscale(bind) ? "Tailnet" : "Network", url: url(bind))]
        }
    }

    /// The address to give another device: the first one that isn't loopback, else this Mac's.
    var primaryEndpoint: Endpoint? { shareableEndpoint ?? endpoints.first }

    /// The first address another device can reach; nil when only this Mac can connect.
    var shareableEndpoint: Endpoint? { endpoints.first { !$0.isLoopback } }

    /// The name clients scanning the tailnet see (`/v1/hello`).
    var hostName: String {
        runtime?.displayName ?? (configuration.displayName.isEmpty ? MachineName.current() : configuration.displayName)
    }

    /// Whether a client scanning the tailnet probes this listener directly: on the Tailscale
    /// address (or every interface) at the default port. Behind `tailscale serve` (127.0.0.1)
    /// it finds the HTTPS name instead, which this app can't see from here.
    var isDiscoverableDirectly: Bool {
        guard configuration.port == HostConfiguration.defaultPort else { return false }
        let bind = configuration.bindAddress
        return bind == "0.0.0.0" || bind == "::" || NetworkAddresses.isTailscale(bind)
    }

    /// The name a client sent with its last successful pairing from `address`, if any.
    func pairedName(for address: String) -> String? {
        pairingAttempts.first { $0.client == address && $0.ok && $0.clientName != nil }?.clientName
    }

    /// Whether other devices can reach the listener directly.
    var isLoopbackOnly: Bool { ["127.0.0.1", "::1"].contains(configuration.bindAddress) }

    /// What the QR code carries: the dashboard URL with the token (it authenticates GETs).
    /// Nil when no other device can reach the server.
    var pairingURL: String? { shareableEndpoint.map(dashboardURL) }

    /// The web dashboard, signed in, at the address other devices use (or this Mac's).
    var dashboardURL: String? { primaryEndpoint.map(dashboardURL) }

    private func dashboardURL(at endpoint: Endpoint) -> String {
        guard let token = configuration.authToken else { return endpoint.url + "/" }
        return endpoint.url + "/?token=" + (token.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? token)
    }

    func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}
