import AppKit
import AVFoundation
import FairspokenCore
import Observation
import OSLog

/// The dictation state machine: hotkey/button → capture → (gate) → transcribe → post-process
/// → deliver (paste into the frontmost app, or show in our own window) → history.
///
/// The recording is written to disk as it is captured and deleted only once its transcript is
/// delivered (`PendingDictations`). If the host fails, the whole recording is sent again, then
/// transcribed on this Mac; if that fails too it is kept, to be retried later.
@Observable
final class DictationController {
    enum Trigger { case hotkey, fn, button }

    enum Delivery: Equatable {
        case pasted(app: String?)
        case copied(String)
        case shownInApp
        /// A dictation kept after a failure, transcribed later: in Recent, and copied if asked.
        case recovered(copied: Bool)

        var isRecovered: Bool { if case .recovered = self { true } else { false } }
    }

    struct Result: Equatable {
        var text: String
        var words: Int
        var delivery: Delivery
        var latencyMs: Int
        var placement: ComputePlacement
    }

    enum Phase: Equatable {
        case idle
        case listening(since: Date)
        case transcribing
        case done(Result)
        case failed(String)

        var isListening: Bool { if case .listening = self { true } else { false } }
        var isActive: Bool {
            switch self {
            case .listening, .transcribing: true
            default: false
            }
        }
    }

    private(set) var phase: Phase = .idle {
        didSet { if phase != oldValue { onPhaseChange?(phase) } }
    }
    /// Latest transcript produced while our own window was frontmost ("Try dictation").
    private(set) var lastInAppResult: Result?
    /// What `.transcribing` is doing when it isn't the first attempt ("Retrying the host (1 of 2)").
    private(set) var transcribingNote: String?

    @ObservationIgnored var onPhaseChange: ((Phase) -> Void)?
    @ObservationIgnored var onNeedsOnboarding: (() -> Void)?
    @ObservationIgnored let capture = AudioCapture()
    @ObservationIgnored private var session: (any TranscriptionSession)?
    @ObservationIgnored private var watchdog: Task<Void, Never>?
    @ObservationIgnored private var resetTask: Task<Void, Never>?
    @ObservationIgnored private var trigger: Trigger = .button
    @ObservationIgnored private var targetApp: NSRunningApplication?
    @ObservationIgnored private var placement: ComputePlacement = .neuralEngine
    /// The recording being captured, and the writer putting it on disk.
    @ObservationIgnored private var kept: (item: PendingDictation, writer: PendingAudioWriter?)?
    @ObservationIgnored private let postProcessor: any TranscriptPostProcessor = BasicPostProcessor()

    private let settings: SettingsStore
    private let models: ModelLibrary
    private let history: HistoryStore
    private let permissions: Permissions
    private let pending: PendingDictations
    @ObservationIgnored var hotkeys: HotkeyController?
    private static let log = Logger(subsystem: AppInfo.bundleID, category: "dictation")

    var meter: LevelMeter { capture.meter }

    init(settings: SettingsStore, models: ModelLibrary, history: HistoryStore, permissions: Permissions, pending: PendingDictations) {
        self.settings = settings
        self.models = models
        self.history = history
        self.permissions = permissions
        self.pending = pending
    }

    // MARK: Triggers

    /// Combo key-down. Toggle mode toggles; push-to-talk starts.
    func hotkeyPressed() {
        if settings.settings.recordingShortcutMode == .toggle {
            phase.isListening ? stop() : start(.hotkey)
        } else if !phase.isListening {
            start(.hotkey)
        }
    }

    func hotkeyReleased() {
        if settings.settings.recordingShortcutMode == .pushToTalk, phase.isListening, trigger == .hotkey { stop() }
    }

    func fnPressed() { if !phase.isActive { start(.fn) } }
    func fnReleased() { if phase.isListening, trigger == .fn { stop() } }

    func toggleFromUI() { phase.isListening ? stop() : start(.button) }

    // MARK: Lifecycle

    func start(_ trigger: Trigger) {
        guard !phase.isActive else { return }
        permissions.refresh()
        switch permissions.microphone {
        case .granted: break
        case .notDetermined:
            permissions.request(.microphone)
            return
        case .denied:
            fail("Microphone access is off")
            onNeedsOnboarding?()
            return
        }

        let s = settings.settings
        let hints = VocabularyPacks.hints(userTerms: s.vocabularyHints, enabledPacks: s.enabledPacks)
        let options = TranscriptionRequestOptions(language: s.language, vocabularyHints: hints)
        let newSession: any TranscriptionSession
        if s.transcriptionLocation == .remoteHost {
            do {
                let base = try RemoteURLPolicy.validateBaseURL(s.remoteUrl)
                newSession = RemoteStreamSession(
                    baseURL: base,
                    options: .init(token: settings.remoteToken, model: s.model, language: s.language, vocabularyHints: hints),
                    timeoutSeconds: s.remoteTimeoutSeconds)
                placement = .remoteHost
            } catch {
                fail(error.localizedDescription)
                return
            }
        } else {
            if !models.engineState.isReady && !models.engineState.isBusy { models.prepare(s.model) }
            newSession = LocalBatchSession(engine: models.engine, options: options)
            placement = .neuralEngine
        }

        let front = NSWorkspace.shared.frontmostApplication
        targetApp = front?.bundleIdentifier == Bundle.main.bundleIdentifier ? nil : front
        let now = Date()
        let item = PendingDictation(id: "dictation-\(Int64(now.timeIntervalSince1970 * 1000))", createdAt: now,
                                    appName: targetApp?.localizedName, appBundleId: targetApp?.bundleIdentifier)
        let writer = pending.begin(item)
        do {
            try capture.start(deviceName: s.audioDevice, maxSeconds: s.maxRecordingSeconds) { [newSession, writer] chunk in
                newSession.append(chunk)
                writer?.append(chunk)
            }
        } catch {
            newSession.cancel()
            pending.drop(item.id, writer: writer)
            fail("Couldn't start the microphone: \(error.localizedDescription)")
            return
        }
        session = newSession
        kept = (item, writer)
        self.trigger = trigger
        resetTask?.cancel()
        phase = .listening(since: Date())
        hotkeys?.setCancelEnabled(true)
        Sounds.play(.start, enabled: s.interactionSounds)
        let limit = s.maxRecordingSeconds
        watchdog = Task { [weak self] in
            try? await Task.sleep(for: .seconds(limit))
            guard !Task.isCancelled else { return }
            self?.stop()
        }
    }

    func cancel() {
        guard phase.isActive else { return }
        watchdog?.cancel()
        if capture.isRunning { capture.stop() }
        session?.cancel()
        session = nil
        if let kept { pending.drop(kept.item.id, writer: kept.writer) }
        kept = nil
        hotkeys?.setCancelEnabled(false)
        phase = .idle
    }

    func stop() {
        guard phase.isListening, let session, let kept else { return }
        watchdog?.cancel()
        hotkeys?.setCancelEnabled(false)
        self.kept = nil
        let released = ContinuousClock.now
        let samples = capture.stop()
        let s = settings.settings
        Sounds.play(.stop, enabled: s.interactionSounds)
        let stats = AudioStats(samples: samples, sampleRate: AudioCapture.sampleRate)
        switch RecordingGate.evaluate(stats) {
        case .tooShort:
            session.cancel()
            self.session = nil
            pending.drop(kept.item.id, writer: kept.writer)
            fail("Too short — hold the shortcut while you speak", quiet: true)
            return
        case .silent:
            session.cancel()
            self.session = nil
            pending.drop(kept.item.id, writer: kept.writer)
            fail("No speech detected")
            return
        case .accept:
            break
        }
        // All of it on disk before anything can fail; deleted once the transcript is delivered.
        pending.keep(kept.item, writer: kept.writer, samples: samples, durationSeconds: stats.durationSeconds)
        phase = .transcribing
        transcribingNote = nil
        let item = kept.item
        let targetPID = targetApp?.processIdentifier
        let placement = self.placement
        let models = self.models
        // After the live attempt: the whole recording to the host again, then this Mac.
        let routes = placement == .remoteHost ? pending.routes(for: s) : TranscriptionRoutes()
        let recovery = pending.recovery
        let progress: @Sendable (DictationRecovery.Progress) -> Void = { [weak self] progress in
            Task { @MainActor [weak self] in self?.show(progress) }
        }
        let deliver: @Sendable (TranscriptionOutput) async -> Void = { [weak self] output in
            await self?.complete(output, item: item, targetPID: targetPID, settings: s, durationSeconds: stats.durationSeconds,
                                 released: released)
        }
        Task { [weak self] in
            let outcome = await recovery.run(item.id, live: {
                if placement == .neuralEngine { try await models.waitUntilReady(timeout: .seconds(180)) }
                return try await session.finish(allSamples: samples)
            }, routes: routes, policy: .live(timeoutSeconds: s.remoteTimeoutSeconds), progress: progress, deliver: deliver)
            guard let self else { return }
            self.session = nil
            self.transcribingNote = nil
            switch outcome {
            case .delivered:
                break
            case .noSpeech:
                self.fail("No speech detected")
            case .kept(let kept):
                Self.log.error("Transcription failed; recording kept to retry: \(kept.lastError ?? "", privacy: .public)")
                self.pending.reload()
                self.fail("Couldn't transcribe — saved to retry", hold: .seconds(5))
            }
        }
    }

    /// The transcript is in: post-process, deliver, record in history, show the result.
    private func complete(_ output: TranscriptionOutput, item: PendingDictation, targetPID: pid_t?, settings s: AppSettings,
                          durationSeconds: Double, released: ContinuousClock.Instant) async {
        let transcribed = ContinuousClock.now
        let text = await postProcessor.process(
            output.text, context: PostProcessContext(appBundleID: item.appBundleId, vocabulary: s.vocabularyHints))
        guard !text.isEmpty else {
            fail("No speech detected")
            return
        }
        let delivery = await deliver(text, settings: s, targetPID: targetPID, appName: item.appName)
        let total = Int((ContinuousClock.now - released) / .milliseconds(1))
        let transcribeMs = Int((transcribed - released) / .milliseconds(1))
        let now = Date()
        let entry = TranscriptHistoryItem(
            id: "transcript-\(Int64(now.timeIntervalSince1970 * 1000))",
            createdAt: Int64(now.timeIntervalSince1970 * 1000), text: text, backend: output.backend,
            location: output.placement == .remoteHost ? "remote-host" : "local", durationSeconds: durationSeconds,
            timings: DictationTimings(transcribeMs: transcribeMs, speechModelMs: output.modelMs, totalMs: total),
            model: output.model, appName: item.appName, appBundleId: item.appBundleId)
        history.add(entry)
        let result = Result(text: text, words: WordCounter.count(text), delivery: delivery, latencyMs: total, placement: output.placement)
        if delivery == .shownInApp { lastInAppResult = result }
        phase = .done(result)
        Self.log.info("Dictation \(durationSeconds, format: .fixed(precision: 1)) s → \(total) ms (model \(output.modelMs) ms)")
        scheduleReset(after: .milliseconds(1600))
    }

    private func show(_ progress: DictationRecovery.Progress) {
        guard phase == .transcribing else { return }
        switch progress {
        case .retrying(let attempt, let count): transcribingNote = "Retrying the host (\(attempt) of \(count))"
        case .transcribingOnThisMac: transcribingNote = "Transcribing on this Mac"
        }
    }

    /// A dictation kept after a failure was transcribed later: say so when the pill is free.
    func announceRecovered(words: Int, copied: Bool, placement: ComputePlacement) {
        guard phase == .idle else { return }
        phase = .done(Result(text: "", words: words, delivery: .recovered(copied: copied), latencyMs: 0, placement: placement))
        scheduleReset(after: .milliseconds(3000))
    }

    // MARK: Helpers

    private func deliver(_ text: String, settings s: AppSettings, targetPID: pid_t?, appName: String?) async -> Delivery {
        // Our own window is frontmost (dashboard "Try dictation", onboarding): show it there.
        guard let targetPID, !(NSApp.isActive && NSApp.keyWindow != nil) else {
            TextInserter.copy(text)
            return .shownInApp
        }
        // The user moved on while it was transcribed (a slow host, a retry): never type into
        // whatever is in front now.
        guard NSWorkspace.shared.frontmostApplication?.processIdentifier == targetPID else {
            TextInserter.copy(text)
            return .copied("Copied — you'd switched apps")
        }
        switch await TextInserter.deliver(text, paste: s.insertAtCursor, restoreClipboard: s.restoreClipboard) {
        case .pasted: return .pasted(app: appName)
        case .copied(let reason): return .copied(reason)
        }
    }

    private func fail(_ message: String, quiet: Bool = false, hold: Duration? = nil) {
        phase = .failed(message)
        if !quiet { Sounds.play(.error, enabled: settings.settings.interactionSounds) }
        scheduleReset(after: hold ?? .milliseconds(quiet ? 1300 : 2600))
    }

    private func scheduleReset(after delay: Duration) {
        resetTask?.cancel()
        let snapshot = phase
        resetTask = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled, let self, self.phase == snapshot else { return }
            self.phase = .idle
        }
    }

    // MARK: Demo/screenshot support

    func showcase(_ phase: Phase, inAppResult: Result? = nil) {
        resetTask?.cancel()
        self.phase = phase
        if let inAppResult { lastInAppResult = inAppResult }
    }
}
