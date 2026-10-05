@_exported import FairspokenUpdateModel
import AppKit
import Observation
import OSLog
import Sparkle

private let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "ie.fairspoken", category: "updates")

/// The app's updater: Sparkle 2 driven by our own user driver, so the update shows in the
/// sidebar button, a toast and Settings rather than in Sparkle's windows (contract §6).
///
/// Sparkle owns the appcast, the EdDSA check, the download, the installer and the relaunch;
/// this class turns its callbacks into `UpdateState` and holds Sparkle's pending replies until
/// the person acts:
///
///   idle ──check──▶ checking ──▶ available ──install──▶ downloading ──▶ ready ──▶ relaunch
///     ▲                │  └──▶ idle ("up to date", manual checks only)
///     └── error ◀──────┘
///
/// Channel (contract §4): the saved `updateChannel`, else the build's own (`-nightly.` in
/// CFBundleShortVersionString). Nightly allows the appcast's `nightly` items; stable allows only
/// the default channel. Changing it checks at once.
@Observable
public final class UpdateController: NSObject {
    public struct Configuration {
        public var appName: String
        /// CFBundleShortVersionString.
        public var version: String
        /// CFBundleVersion (the UTC build timestamp in releases).
        public var build: String
        /// Debug builds only: an appcast to use instead of Info.plist's SUFeedURL.
        public var feedOverride: URL?
        public var defaults: UserDefaults

        public init(appName: String, version: String, build: String, feedOverride: URL? = nil, defaults: UserDefaults = .standard) {
            self.appName = appName
            self.version = version
            self.build = build
            self.feedOverride = feedOverride
            self.defaults = defaults
        }

        /// Reads `FAIRSPOKEN_APPCAST_URL` from the defaults (`defaults write <bundle id>
        /// FAIRSPOKEN_APPCAST_URL …` or `-FAIRSPOKEN_APPCAST_URL …` on the command line) or the
        /// environment. Call it from `#if DEBUG` code only: release builds always use SUFeedURL.
        public static func debugFeedOverride(defaults: UserDefaults = .standard) -> URL? {
            let value = defaults.string(forKey: "FAIRSPOKEN_APPCAST_URL")
                ?? ProcessInfo.processInfo.environment["FAIRSPOKEN_APPCAST_URL"]
            guard let value, !value.isEmpty else { return nil }
            return URL(string: value)
        }
    }

    /// What a toast says. The toast is one at a time; a newer one replaces it.
    public struct Toast: Equatable, Identifiable {
        public enum Kind: Equatable { case available, ready, upToDate, failed }
        public var kind: Kind
        public var title: String
        public var message: String?
        public var id: String { "\(kind)-\(title)" }

        /// The primary button, if the toast has one ("Update" / "Restart").
        public var actionTitle: String? {
            switch kind {
            case .available: "Update"
            case .ready: "Restart"
            case .upToDate, .failed: nil
            }
        }

        /// Up to date and failure notes go away on their own.
        public var autoDismissAfter: Duration? {
            switch kind {
            case .available, .ready: nil
            case .upToDate: .seconds(4)
            case .failed: .seconds(8)
            }
        }
    }

    public private(set) var state: UpdateState = .idle
    public private(set) var channel: UpdateChannel
    public private(set) var lastChecked: Date?
    public private(set) var toast: Toast?
    public let configuration: Configuration

    public var appName: String { configuration.appName }
    public var currentVersion: String { configuration.version }
    public var build: String { configuration.build }
    public var isUpdatePending: Bool { state.isPending }
    public var showsMoveToStableNote: Bool {
        UpdateChannel.showsMoveToStableNote(channel: channel, currentVersion: currentVersion)
    }
    public var presentation: UpdatePresentation {
        UpdatePresentation.make(state: state, appName: appName, currentVersion: currentVersion, channel: channel)
    }

    @ObservationIgnored private var updater: SPUUpdater?
    @ObservationIgnored private var foundReply: ((SPUUserUpdateChoice) -> Void)?
    @ObservationIgnored private var readyReply: ((SPUUserUpdateChoice) -> Void)?
    @ObservationIgnored private var download = DownloadProgress()
    @ObservationIgnored private var userInitiated = false
    @ObservationIgnored private var checkWhenSessionEnds = false
    @ObservationIgnored private var simulated = false
    @ObservationIgnored private var cycleFinishedThisLaunch = false
    @ObservationIgnored private var toastTask: Task<Void, Never>?

    public init(configuration: Configuration) {
        self.configuration = configuration
        channel = UpdateChannel.resolve(saved: configuration.defaults.string(forKey: UpdateChannel.defaultsKey),
                                        version: configuration.version)
        super.init()
    }

    // MARK: Lifecycle

    /// Starts Sparkle (scheduled checks every SUScheduledCheckInterval, 6 h) and checks once
    /// about 30 s after launch.
    public func start(launchCheckDelay: Duration = .seconds(30)) {
        guard updater == nil, !simulated else { return }
        let updater = SPUUpdater(hostBundle: .main, applicationBundle: .main, userDriver: self, delegate: self)
        do {
            try updater.start()
        } catch {
            state = .error(message: error.localizedDescription)
            return
        }
        self.updater = updater
        lastChecked = updater.lastUpdateCheckDate
        Task { [weak self] in
            try? await Task.sleep(for: launchCheckDelay)
            self?.launchCheck()
        }
    }

    private func launchCheck() {
        guard let updater, updater.automaticallyChecksForUpdates, updater.canCheckForUpdates, !updater.sessionInProgress else { return }
        // Sparkle's own schedule may already have checked since launch. (Its last-check date can't
        // tell: on a first launch Sparkle stamps it without checking.)
        if cycleFinishedThisLaunch { return }
        updater.checkForUpdatesInBackground()
    }

    // MARK: Actions

    /// The sidebar button: check, install, or restart, by state.
    public func performPrimaryAction() {
        switch state.primaryAction {
        case .check: checkForUpdates()
        case .install: installUpdate()
        case .relaunch: relaunchToUpdate()
        case .none: break
        }
    }

    /// A check the person asked for ("Check for Updates…"): answers even when up to date. With
    /// an update already found, it shows that one again instead.
    public func checkForUpdates() {
        switch state {
        case .available(let version): showToast(.available, version: version)
        case .ready(let version): showToast(.ready, version: version)
        case .checking, .downloading: break
        case .idle, .error:
            if simulated { return }
            guard let updater else { return }
            if updater.canCheckForUpdates {
                updater.checkForUpdates()
            } else {
                checkWhenSessionEnds = true
            }
        }
    }

    /// Downloads and installs the offered update.
    public func installUpdate() {
        dismissToast()
        guard case .available(let version) = state else { return }
        if simulated { state = .downloading(version: version, progress: 0.35); return }
        guard let reply = foundReply else { return }
        foundReply = nil
        download = DownloadProgress()
        state = .downloading(version: version, progress: nil)
        reply(.install)
    }

    /// Quits, installs the downloaded update and relaunches.
    public func relaunchToUpdate() {
        dismissToast()
        guard case .ready = state, !simulated else { return }
        if let reply = readyReply ?? foundReply {
            readyReply = nil
            foundReply = nil
            reply(.install)
        }
    }

    public func setChannel(_ next: UpdateChannel) {
        guard next != channel else { return }
        channel = next
        configuration.defaults.set(next.rawValue, forKey: UpdateChannel.defaultsKey)
        guard !simulated, let updater else { return }
        updater.resetUpdateCycleAfterShortDelay()
        switch state {
        case .downloading, .ready:
            // Never interrupt an update already on its way; the next check uses the new channel.
            break
        case .available:
            // The offer came from the old channel: drop it, then check the new one when Sparkle
            // has closed the session (dismissUpdateInstallation).
            dismissToast()
            checkWhenSessionEnds = true
            let reply = foundReply
            foundReply = nil
            state = .idle
            reply?(.dismiss)
        case .idle, .checking, .error:
            if updater.canCheckForUpdates && !updater.sessionInProgress {
                updater.checkForUpdates()
            } else {
                checkWhenSessionEnds = true
            }
        }
    }

    /// "Later" and the toast's close button: the offer stays in the sidebar and Settings.
    public func dismissToast() {
        toastTask?.cancel()
        toastTask = nil
        toast = nil
    }

    /// Development and screenshots: show a state without Sparkle. Nothing is checked,
    /// downloaded or installed afterwards. Gate calls behind `#if DEBUG`.
    public func simulate(_ state: UpdateState, lastChecked: Date? = Date(), toast: Bool = false) {
        simulated = true
        self.state = state
        self.lastChecked = lastChecked
        if toast { showUpdateInFocus() }
    }

    /// `-FAIRSPOKEN_UPDATE_STATE <idle|checking|available|downloading|ready|error>` (and
    /// `-FAIRSPOKEN_UPDATE_TOAST YES`) on the command line: simulate that state instead of
    /// starting Sparkle. Returns whether it did. Call from `#if DEBUG` code only.
    public func simulateFromDefaults() -> Bool {
        let defaults = configuration.defaults
        guard let name = defaults.string(forKey: "FAIRSPOKEN_UPDATE_STATE"), let sample = UpdateState.sample(named: name) else {
            return false
        }
        simulate(sample, toast: defaults.bool(forKey: "FAIRSPOKEN_UPDATE_TOAST"))
        return true
    }

    // MARK: Toasts

    private func showToast(_ kind: Toast.Kind, version: String? = nil, message: String? = nil) {
        let name = version.map { updateDisplayName(appName: appName, version: $0) } ?? appName
        let next: Toast
        switch kind {
        case .available:
            next = Toast(kind: .available, title: "\(name) is available", message: "You have \(currentVersion).")
        case .ready:
            next = Toast(kind: .ready, title: "\(name) is ready", message: "Restart \(appName) to finish updating.")
        case .upToDate:
            next = Toast(kind: .upToDate, title: "\(appName) is up to date",
                         message: "\(currentVersion) is the newest \(channel.title) version.")
        case .failed:
            next = Toast(kind: .failed, title: "Couldn't check for updates", message: message)
        }
        toastTask?.cancel()
        toast = next
        if let delay = next.autoDismissAfter {
            toastTask = Task { [weak self] in
                try? await Task.sleep(for: delay)
                guard !Task.isCancelled, self?.toast == next else { return }
                self?.toast = nil
            }
        }
    }

    private func offer(_ kind: Toast.Kind, version: String, userInitiated: Bool) {
        let defaults = configuration.defaults
        let key = UpdateOfferLedger.defaultsKey + (kind == .ready ? ".ready" : "")
        guard UpdateOfferLedger.shouldOffer(version: version, lastOffered: defaults.string(forKey: key), userInitiated: userInitiated) else { return }
        defaults.set(version, forKey: key)
        showToast(kind, version: version)
    }

    private func refreshLastChecked() {
        lastChecked = updater?.lastUpdateCheckDate ?? lastChecked
    }
}

// MARK: - SPUUserDriver

extension UpdateController: SPUUserDriver {
    public func show(_ request: SPUUpdatePermissionRequest, reply: @escaping (SUUpdatePermissionResponse) -> Void) {
        // SUEnableAutomaticChecks is in Info.plist, so Sparkle shouldn't ask; answer the same if it does.
        reply(SUUpdatePermissionResponse(automaticUpdateChecks: true, sendSystemProfile: false))
    }

    public func showUserInitiatedUpdateCheck(cancellation: @escaping () -> Void) {
        userInitiated = true
        dismissToast()
        state = .checking
    }

    public func showUpdateFound(with appcastItem: SUAppcastItem, state updateState: SPUUserUpdateState, reply: @escaping (SPUUserUpdateChoice) -> Void) {
        let version = appcastItem.displayVersionString
        log.info("Update found: \(version, privacy: .public) (\(appcastItem.versionString, privacy: .public)) on \(self.channel.rawValue, privacy: .public)")
        let asked = updateState.userInitiated || userInitiated
        userInitiated = false
        foundReply = reply
        switch updateState.stage {
        case .notDownloaded:
            state = .available(version: version)
            offer(.available, version: version, userInitiated: asked)
        case .downloaded, .installing:
            // Downloaded in an earlier session: installing is a restart away.
            state = .ready(version: version)
            offer(.ready, version: version, userInitiated: asked)
        @unknown default:
            state = .available(version: version)
        }
        refreshLastChecked()
    }

    public func showUpdateReleaseNotes(with downloadData: SPUDownloadData) {}

    public func showUpdateReleaseNotesFailedToDownloadWithError(_ error: any Error) {}

    public func showUpdateNotFoundWithError(_ error: any Error, acknowledgement: @escaping () -> Void) {
        let asked = userInitiated || ((error as NSError).userInfo[SPUNoUpdateFoundUserInitiatedKey] as? Bool ?? false)
        userInitiated = false
        state = .idle
        refreshLastChecked()
        if asked { showToast(.upToDate) }
        acknowledgement()
    }

    public func showUpdaterError(_ error: any Error, acknowledgement: @escaping () -> Void) {
        let nsError = error as NSError
        log.error("Updater error \(nsError.domain, privacy: .public) \(nsError.code): \(nsError.localizedDescription, privacy: .public)")
        let asked = userInitiated
        userInitiated = false
        foundReply = nil
        readyReply = nil
        if nsError.domain == SUSparkleErrorDomain && nsError.code == 4007 /* SUInstallationCanceledError */ {
            state = .idle
        } else {
            state = .error(message: nsError.localizedDescription)
            if asked { showToast(.failed, message: nsError.localizedDescription) }
        }
        refreshLastChecked()
        acknowledgement()
    }

    public func showDownloadInitiated(cancellation: @escaping () -> Void) {
        download = DownloadProgress()
        state = .downloading(version: state.version ?? "", progress: nil)
    }

    public func showDownloadDidReceiveExpectedContentLength(_ expectedContentLength: UInt64) {
        download.expectedLength = expectedContentLength
        download.receivedLength = 0
        publishDownload()
    }

    public func showDownloadDidReceiveData(ofLength length: UInt64) {
        download.receivedLength += length
        publishDownload()
    }

    public func showDownloadDidStartExtractingUpdate() {
        download.extraction = 0
        publishDownload()
    }

    public func showExtractionReceivedProgress(_ progress: Double) {
        download.extraction = progress
        publishDownload()
    }

    private func publishDownload() {
        let version = state.version ?? ""
        let fraction = download.fraction
        // Coarse steps: a 1 % change redraws the ring; every byte callback would not.
        if case .downloading(_, let shown) = state, let shown, let fraction, abs(shown - fraction) < 0.01 { return }
        state = .downloading(version: version, progress: fraction)
    }

    public func showReady(toInstallAndRelaunch reply: @escaping (SPUUserUpdateChoice) -> Void) {
        let version = state.version ?? ""
        readyReply = reply
        state = .ready(version: version)
        offer(.ready, version: version, userInitiated: true)
    }

    public func showInstallingUpdate(withApplicationTerminated applicationTerminated: Bool, retryTerminatingApplication: @escaping () -> Void) {}

    public func showUpdateInstalledAndRelaunched(_ relaunched: Bool, acknowledgement: @escaping () -> Void) {
        state = .idle
        acknowledgement()
    }

    public func showUpdateInFocus() {
        if let version = state.version {
            showToast(state.primaryAction == .relaunch ? .ready : .available, version: version)
        }
    }

    public func dismissUpdateInstallation() {
        // The session is over. Errors and "up to date" were already shown; anything in flight is not.
        switch state {
        case .checking, .available, .downloading, .ready: state = .idle
        case .idle, .error: break
        }
        foundReply = nil
        readyReply = nil
        userInitiated = false
        if checkWhenSessionEnds {
            checkWhenSessionEnds = false
            Task { [weak self] in self?.checkForUpdates() }
        }
    }
}

// MARK: - SPUUpdaterDelegate

extension UpdateController: SPUUpdaterDelegate {
    public func allowedChannels(for updater: SPUUpdater) -> Set<String> {
        channel.sparkleChannels
    }

    public func feedURLString(for updater: SPUUpdater) -> String? {
        configuration.feedOverride?.absoluteString
    }

    public func updater(_ updater: SPUUpdater, didFinishUpdateCycleFor updateCheck: SPUUpdateCheck, error: (any Error)?) {
        cycleFinishedThisLaunch = true
        let outcome = (error as NSError?).map { "\($0.domain) \($0.code)" } ?? "ok"
        log.info("Update cycle finished (\(self.channel.rawValue, privacy: .public)): \(outcome, privacy: .public)")
        refreshLastChecked()
    }
}
