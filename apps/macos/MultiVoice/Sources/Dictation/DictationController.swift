import AppKit
import AVFoundation
import MultiVoiceCore
import Observation
import OSLog

/// The dictation state machine: hotkey/button → capture → (gate) → transcribe → post-process
/// → deliver (paste into the frontmost app, or show in our own window) → history.
@Observable
final class DictationController {
    enum Trigger { case hotkey, fn, button }

    enum Delivery: Equatable {
        case pasted(app: String?)
        case copied(String)
        case shownInApp
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

    @ObservationIgnored var onPhaseChange: ((Phase) -> Void)?
    @ObservationIgnored var onNeedsOnboarding: (() -> Void)?
    @ObservationIgnored let capture = AudioCapture()
    @ObservationIgnored private var session: (any TranscriptionSession)?
    @ObservationIgnored private var watchdog: Task<Void, Never>?
    @ObservationIgnored private var resetTask: Task<Void, Never>?
    @ObservationIgnored private var trigger: Trigger = .button
    @ObservationIgnored private var targetApp: NSRunningApplication?
    @ObservationIgnored private var placement: ComputePlacement = .neuralEngine
    @ObservationIgnored private let postProcessor: any TranscriptPostProcessor = BasicPostProcessor()

    private let settings: SettingsStore
    private let models: ModelLibrary
    private let history: HistoryStore
    private let permissions: Permissions
    @ObservationIgnored var hotkeys: HotkeyController?
    private static let log = Logger(subsystem: AppInfo.bundleID, category: "dictation")

    var meter: LevelMeter { capture.meter }

    init(settings: SettingsStore, models: ModelLibrary, history: HistoryStore, permissions: Permissions) {
        self.settings = settings
        self.models = models
        self.history = history
        self.permissions = permissions
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
        let options = TranscriptionRequestOptions(language: s.language, vocabularyHints: s.vocabularyHints)
        let newSession: any TranscriptionSession
        if s.transcriptionLocation == .remoteHost {
            do {
                let base = try RemoteURLPolicy.validateBaseURL(s.remoteUrl)
                newSession = RemoteStreamSession(
                    baseURL: base,
                    options: .init(token: settings.remoteToken, model: s.model, language: s.language, vocabularyHints: s.vocabularyHints),
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
        do {
            try capture.start(deviceName: s.audioDevice, maxSeconds: s.maxRecordingSeconds) { [newSession] chunk in
                newSession.append(chunk)
            }
        } catch {
            newSession.cancel()
            fail("Couldn't start the microphone: \(error.localizedDescription)")
            return
        }
        session = newSession
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
        hotkeys?.setCancelEnabled(false)
        phase = .idle
    }

    func stop() {
        guard phase.isListening, let session else { return }
        watchdog?.cancel()
        hotkeys?.setCancelEnabled(false)
        let released = ContinuousClock.now
        let samples = capture.stop()
        let s = settings.settings
        Sounds.play(.stop, enabled: s.interactionSounds)
        let stats = AudioStats(samples: samples, sampleRate: AudioCapture.sampleRate)
        switch RecordingGate.evaluate(stats) {
        case .tooShort:
            session.cancel()
            self.session = nil
            fail("Too short — hold the shortcut while you speak", quiet: true)
            return
        case .silent:
            session.cancel()
            self.session = nil
            fail("No speech detected")
            return
        case .accept:
            break
        }
        phase = .transcribing
        let target = targetApp
        let placement = self.placement
        Task { [weak self] in
            guard let self else { return }
            do {
                if placement == .neuralEngine { try await self.waitForEngine() }
                let output = try await session.finish(allSamples: samples)
                let transcribed = ContinuousClock.now
                let text = await self.postProcessor.process(
                    output.text, context: PostProcessContext(appBundleID: target?.bundleIdentifier, vocabulary: s.vocabularyHints))
                guard !text.isEmpty else {
                    self.fail("No speech detected")
                    return
                }
                let delivery = await self.deliver(text, settings: s, target: target)
                let total = Int((ContinuousClock.now - released) / .milliseconds(1))
                let transcribeMs = Int((transcribed - released) / .milliseconds(1))
                let now = Date()
                let item = TranscriptHistoryItem(
                    id: "transcript-\(Int64(now.timeIntervalSince1970 * 1000))",
                    createdAt: Int64(now.timeIntervalSince1970 * 1000), text: text, backend: output.backend,
                    location: placement == .remoteHost ? "remote-host" : "local", durationSeconds: stats.durationSeconds,
                    timings: DictationTimings(transcribeMs: transcribeMs, speechModelMs: output.modelMs, totalMs: total),
                    model: output.model, appName: target?.localizedName, appBundleId: target?.bundleIdentifier)
                self.history.add(item)
                let result = Result(text: text, words: WordCounter.count(text), delivery: delivery, latencyMs: total, placement: placement)
                if delivery == .shownInApp { self.lastInAppResult = result }
                self.session = nil
                self.phase = .done(result)
                Self.log.info("Dictation \(stats.durationSeconds, format: .fixed(precision: 1)) s → \(total) ms (model \(output.modelMs) ms)")
                self.scheduleReset(after: .milliseconds(1600))
            } catch {
                self.session = nil
                Self.log.error("Transcription failed: \(error.localizedDescription, privacy: .public)")
                self.fail(error.localizedDescription)
            }
        }
    }

    // MARK: Helpers

    private func deliver(_ text: String, settings s: AppSettings, target: NSRunningApplication?) async -> Delivery {
        // Our own window is frontmost (dashboard "Try dictation", onboarding): show it there.
        if target == nil || (NSApp.isActive && NSApp.keyWindow != nil) {
            TextInserter.copy(text)
            return .shownInApp
        }
        switch await TextInserter.deliver(text, paste: s.insertAtCursor, restoreClipboard: s.restoreClipboard) {
        case .pasted: return .pasted(app: target?.localizedName)
        case .copied(let reason): return .copied(reason)
        }
    }

    private func waitForEngine() async throws {
        let deadline = ContinuousClock.now + .seconds(180)
        while !models.engineState.isReady {
            if case .failed(let message) = models.engineState { throw NSError(domain: "engine", code: 1, userInfo: [NSLocalizedDescriptionKey: message]) }
            if ContinuousClock.now > deadline { throw NSError(domain: "engine", code: 2, userInfo: [NSLocalizedDescriptionKey: "The speech model is still loading"]) }
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    private func fail(_ message: String, quiet: Bool = false) {
        phase = .failed(message)
        if !quiet { Sounds.play(.error, enabled: settings.settings.interactionSounds) }
        scheduleReset(after: .milliseconds(quiet ? 1300 : 2600))
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
