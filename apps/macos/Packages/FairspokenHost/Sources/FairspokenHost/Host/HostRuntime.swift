import Foundation
import Synchronization

/// Workers, queue, live configuration and metrics of a running host, without the network
/// layer (`HostRouter` speaks HTTP on top of it). Semantics follow `HostRuntime` and
/// `run_host_worker` in src-tauri/src/host/mod.rs: N workers share one bounded queue, each
/// worker serves its assigned model (the host's choice, never the client's), models are
/// preloaded while idle, and every `job_queued` gets exactly one terminal event.
public final class HostRuntime: Sendable {
    public static let backendID = "parakeet"
    static let streamAborted = "Stream upload aborted"
    static let workerIdlePoll: Duration = .milliseconds(250)

    public enum RuntimeError: Error, Equatable, Sendable {
        case queueFull(String)
        case workerFailed(String)
    }

    public let configuration: HostConfiguration
    public let metrics: HostMetrics
    public let backend: any HostSpeechBackend
    public let serverVersion: String
    public let pairingLimiter: PairingLimiter
    let queue: JobQueue
    private let live: Mutex<HostLiveSettings>
    private let access: Mutex<HostCredentials>
    private let name: Mutex<String>
    private let configURL: URL?
    private let nextJobID = Atomic<UInt64>(1)
    private let workerTasks = Mutex<[Task<Void, Never>]>([])
    private let sizes = Mutex<[String: Int64]>([:])
    /// Called (on an arbitrary thread) after the live settings change.
    private let onLiveChange = Mutex<(@Sendable (HostLiveSettings) -> Void)?>(nil)

    public var knownModels: Set<String> { Set(backend.catalog.map(\.id)) }

    public init(configuration: HostConfiguration, configURL: URL?, backend: any HostSpeechBackend, serverVersion: String,
                pairingLimiter: PairingLimiter = PairingLimiter()) {
        var config = configuration.normalized(knownModels: Set(backend.catalog.map(\.id)))
        if config.pairingEnabled && config.authToken == nil {
            // Pairing hands out the token, so a password needs one: make it and keep it.
            config.token = HostConfiguration.generateToken()
            if let configURL {
                var persisted = (try? HostConfigurationStore.load(configURL)) ?? config
                persisted.token = config.token
                // Unsaved, the token still works until the next start; clients then pair again.
                try? HostConfigurationStore.save(persisted, to: configURL)
            }
        }
        self.configuration = config
        self.configURL = configURL
        self.backend = backend
        self.serverVersion = serverVersion
        self.pairingLimiter = pairingLimiter
        access = Mutex(HostCredentials(token: config.authToken, pairingPassword: config.pairingEnabled ? config.pairingPassword : nil))
        name = Mutex(config.displayName.isEmpty ? MachineName.current() : config.displayName)
        metrics = HostMetrics(workerCount: config.workerCount, queueCapacity: config.queueCapacity)
        queue = JobQueue(capacity: config.queueCapacity)
        live = Mutex(HostLiveSettings(maxActiveStreams: config.maxActiveStreams, maxRecordingSeconds: config.maxRecordingSeconds,
                                      useGpu: config.useGpu, workerModels: config.workerModels))
        refreshModelSizes()
    }

    public var liveSettings: HostLiveSettings { live.withLock { $0 } }

    /// The token and pairing password in force now (the router checks every request against them).
    public var credentials: HostCredentials { access.withLock { $0 } }
    public var authToken: String? { credentials.token }
    public var pairingEnabled: Bool { credentials.pairingEnabled }

    /// The name `/v1/hello` and `/v1/pair` report.
    public var displayName: String { name.withLock { $0 } }

    /// Renames the host (app only); empty goes back to this Mac's name.
    public func setDisplayName(_ next: String) {
        let clean = HostConfiguration.cleanName(next)
        name.withLock { $0 = clean ?? MachineName.current() }
    }

    public func setLiveChangeHandler(_ handler: (@Sendable (HostLiveSettings) -> Void)?) {
        onLiveChange.withLock { $0 = handler }
    }

    // MARK: Lifecycle

    public func startWorkers() {
        let tasks = (0..<configuration.workerCount).map { index in
            Task.detached(priority: .userInitiated) { [weak self] in
                guard let self else { return }
                await self.runWorker(index)
            }
        }
        workerTasks.withLock { $0 = tasks }
    }

    /// Stops the workers. Waiting jobs fail; running jobs finish or are cancelled.
    public func stop() async {
        for job in queue.close() {
            metrics.dropQueuedJob(job.id, error: "Transcription host is shutting down", rejectClient: false)
            job.result.fulfil(.failure(.workerFailed("Transcription host is shutting down")))
            if case .stream(let feed) = job.audio { feed.finish() }
        }
        let tasks = workerTasks.withLock { t in defer { t.removeAll() }; return t }
        tasks.forEach { $0.cancel() }
        for t in tasks { await t.value }
        for i in 0..<configuration.workerCount { await backend.release(worker: i) }
        metrics.events.closeAll()
    }

    // MARK: Stats

    func modelInfos() -> [HostMetrics.ModelInfo] {
        let sizes = sizes.withLock { $0 }
        return backend.catalog.map { d in
            let installed = backend.isInstalled(d.id)
            return HostMetrics.ModelInfo(descriptor: d, installed: installed, installedBytes: installed ? sizes[d.id] : nil)
        }
    }

    public func refreshModelSizes() {
        var next: [String: Int64] = [:]
        for d in backend.catalog where backend.isInstalled(d.id) {
            next[d.id] = backend.installedBytes(d.id)
        }
        sizes.withLock { $0 = next }
    }

    public func statsJSON() -> JSONValue {
        let live = liveSettings
        let pairing = pairingEnabled
        let models = modelInfos()
        return metrics.locked { s in
            HostMetrics.snapshot(s, bindAddr: configuration.bindAddr, serverVersion: serverVersion, live: live, models: models,
                                 pairingEnabled: pairing)
        }
    }

    /// Subscribes to `/v1/events` and takes the snapshot atomically (both under the metrics lock).
    public func subscribe() throws(EventHub.SubscribeError) -> (EventSubscription, JSONValue) {
        let live = liveSettings
        let pairing = pairingEnabled
        let models = modelInfos()
        let result = metrics.locked { s -> Result<(EventSubscription, JSONValue), EventHub.SubscribeError> in
            do {
                let sub = try metrics.events.subscribe()
                return .success((sub, HostMetrics.snapshot(s, bindAddr: configuration.bindAddr, serverVersion: serverVersion, live: live, models: models,
                                 pairingEnabled: pairing)))
            } catch let error as EventHub.SubscribeError {
                return .failure(error)
            } catch {
                return .failure(.tooMany)
            }
        }
        return try result.get()
    }

    /// The in-process equivalent of `/v1/events` for the app's own UI: a snapshot and the frames
    /// that follow it, with no subscriber limit.
    public func observe() -> (snapshot: JSONValue, frames: AsyncStream<String>) {
        let live = liveSettings
        let pairing = pairingEnabled
        let models = modelInfos()
        return metrics.locked { s in
            let frames = metrics.events.observe()
            return (HostMetrics.snapshot(s, bindAddr: configuration.bindAddr, serverVersion: serverVersion, live: live, models: models,
                                 pairingEnabled: pairing), frames)
        }
    }

    // MARK: Configuration

    /// Validates and applies a live update, then persists it (Rust `handle_config_update`).
    /// A pairing password on a host without a token also generates and saves the token, which
    /// every request then needs.
    public func applyConfigUpdate(_ update: HostConfigUpdate) throws(ConfigUpdateError) -> HostLiveSettings {
        let known = knownModels
        // Checked before anything applies, like every other field.
        if let password = update.pairingPassword, let problem = HostConfiguration.pairingPasswordProblem(password) {
            throw .invalid(problem)
        }
        let next: HostLiveSettings
        do {
            next = try live.withLock { current throws(HostConfigUpdate.ParseError) -> HostLiveSettings in
                let n = try current.applying(update, knownModels: known)
                current = n
                return n
            }
        } catch {
            throw .invalid(error.message)
        }
        var generatedToken: String?
        if let password = update.pairingPassword {
            access.withLock { a in
                a.pairingPassword = password.isEmpty ? nil : password
                if a.pairingPassword != nil && a.token == nil {
                    a.token = HostConfiguration.generateToken()
                    generatedToken = a.token
                }
            }
        }
        queue.wakeIdleWorkers()
        onLiveChange.withLock { $0 }?(next)
        if let configURL {
            var persisted = (try? HostConfigurationStore.load(configURL)) ?? configuration
            persisted.maxActiveStreams = next.maxActiveStreams
            persisted.maxRecordingSeconds = next.maxRecordingSeconds
            persisted.useGpu = next.useGpu
            persisted.workerModels = next.workerModels
            if let password = update.pairingPassword { persisted.pairingPassword = password }
            if let generatedToken { persisted.token = generatedToken }
            do { try HostConfigurationStore.save(persisted, to: configURL) } catch {
                throw .notSaved("Config applied for this session only — saving it failed: \(error.localizedDescription)")
            }
        }
        return next
    }

    public enum ConfigUpdateError: Error, Equatable {
        case invalid(String)
        case notSaved(String)
    }

    // MARK: Pairing

    public enum PairResult: Equatable, Sendable {
        /// The password matched (or the host has no token: `token` is nil).
        case paired(token: String?)
        case wrongPassword
        case disabled
        case limited(retryAfterSeconds: Int)
    }

    /// `POST /v1/pair` without the HTTP: rate limit, constant-time compare, `pairing` event.
    public func pair(password: String, clientName: String?, client: String?) -> PairResult {
        let current = credentials
        let result: PairResult
        let outcome: PairingAttempt.Outcome
        if current.token == nil {
            (result, outcome) = (.paired(token: nil), .open)
        } else if !current.pairingEnabled {
            (result, outcome) = (.disabled, .disabled)
        } else {
            switch pairingLimiter.attempt(address: client ?? HostRouter.unknownClient, { current.passwordMatches(password) }) {
            case .matched: (result, outcome) = (.paired(token: current.token), .paired)
            case .mismatched: (result, outcome) = (.wrongPassword, .wrongPassword)
            case .limited(let seconds): (result, outcome) = (.limited(retryAfterSeconds: seconds), .limited)
            }
        }
        metrics.recordPairing(client: client, clientName: clientName, outcome: outcome)
        return result
    }

    /// Recent pairing attempts, newest first (app only; not part of the protocol).
    public func pairingAttempts() -> [PairingAttempt] { metrics.locked { $0.pairings } }

    // MARK: Models

    public enum DownloadStartError: Error, Equatable {
        case unsupported(String)
        case inProgress
    }

    /// Starts a model download in the background (Rust `handle_model_download`).
    public func startDownload(_ model: String) throws(DownloadStartError) {
        guard knownModels.contains(model) else { throw .unsupported("Unsupported model: \(model)") }
        guard metrics.beginModelDownload(model) else { throw .inProgress }
        Task.detached { [self] in
            do {
                try await backend.download(model) { [metrics] stage, pct in
                    metrics.setModelDownload(ModelDownloadState(model: model, stage: stage.rawValue, percentage: min(100, max(0, pct)), error: nil))
                }
                refreshModelSizes()
                metrics.setModelDownload(ModelDownloadState(model: model, stage: ModelDownloadStage.ready.rawValue, percentage: 100, error: nil))
            } catch {
                metrics.setModelDownload(ModelDownloadState(model: model, stage: ModelDownloadStage.error.rawValue, percentage: 0,
                                                            error: error.localizedDescription))
            }
            queue.wakeIdleWorkers()
        }
    }

    /// Removes an installed model (app only; not part of the protocol).
    public func deleteModel(_ model: String) throws {
        try backend.delete(model)
        refreshModelSizes()
        queue.wakeIdleWorkers()
    }

    // MARK: Requests

    func validateDuration(_ seconds: Double) -> String? {
        let max = liveSettings.maxRecordingSeconds
        return seconds > Double(max) ? "Recording exceeds host maximum of \(max) seconds" : nil
    }

    /// Queues a job (publishing `job_queued`); a full queue publishes `job_failed` with no worker.
    func enqueue(_ audio: JobAudio, language: String, source: String, client: String?, audioSeconds: Double) -> Result<JobResult, RuntimeError> {
        let id = nextJobID.add(1, ordering: .relaxed).oldValue
        let result = JobResult()
        let job = TranscriptionJob(id: id, audio: audio, language: language, source: source, client: client, acceptedAt: .now, result: result)
        metrics.enqueueJob(QueuedJobInfo(id: id, model: liveSettings.modelSummary, source: source, client: client,
                                         audioSeconds: audioSeconds, enqueuedAt: .now))
        switch queue.tryEnqueue(job) {
        case .accepted:
            return .success(result)
        case .full:
            let message = "Transcription queue is full"
            metrics.dropQueuedJob(id, error: message, rejectClient: true)
            return .failure(.queueFull(message))
        case .closed:
            let message = "Transcription worker queue is unavailable"
            metrics.dropQueuedJob(id, error: message, rejectClient: false)
            return .failure(.workerFailed(message))
        }
    }

    // MARK: Workers

    private struct Target: Equatable {
        var model: String
        var accelerated: Bool
    }

    private func runWorker(_ index: Int) async {
        var loaded: Target?
        var lastFailed: (Target, Date?)?
        while !Task.isCancelled {
            let settings = liveSettings
            let model = index < settings.workerModels.count ? settings.workerModels[index] : HostConfiguration.defaultModel
            let target = Target(model: model, accelerated: settings.useGpu)
            if loaded != target {
                if !backend.isInstalled(model) {
                    if loaded != nil {
                        // Serving the previous model would be a silent fallback: hold nothing.
                        await backend.release(worker: index)
                        loaded = nil
                    }
                    lastFailed = nil
                    metrics.workerModelUnavailable(index, model: model, message: "Model \(model) is not installed on this host")
                } else {
                    let stamp = backend.installStamp(model)
                    if !(lastFailed.map { $0.0 == target && $0.1 == stamp } ?? false) {
                        metrics.workerPreloading(index, model: model)
                        do {
                            try await backend.prepare(worker: index, model: model, accelerated: target.accelerated)
                            loaded = target
                            lastFailed = nil
                            metrics.workerPreloadReady(index, model: model)
                        } catch {
                            if Task.isCancelled { return }
                            lastFailed = (target, stamp)
                            metrics.workerModelUnavailable(index, model: model, message: error.localizedDescription)
                        }
                    }
                }
            }

            guard let job = await queue.next(timeout: Self.workerIdlePoll) else { continue }
            await run(job, worker: index, target: target, loaded: &loaded)
        }
    }

    private struct StreamAborted: Error {}

    private func run(_ job: TranscriptionJob, worker index: Int, target: Target, loaded: inout Target?) async {
        let queueWait = ContinuousClock.now - job.acceptedAt
        let model = target.model
        let needsLoad = loaded != target
        var duration: Double
        if case .recording(let r) = job.audio { duration = r.durationSeconds } else { duration = 0 }
        metrics.startJob(worker: index, queueWait: queueWait, needsLoad: needsLoad,
                         job: RunningJobInfo(id: job.id, model: model, source: job.source, client: job.client, audioSeconds: duration,
                                             startedAt: .now))
        var started = ContinuousClock.now
        let result: Result<String, Error>
        do {
            if needsLoad {
                try await backend.prepare(worker: index, model: model, accelerated: target.accelerated)
                loaded = target
            }
            metrics.workerModelReady(index, model: model)
            switch job.audio {
            case .recording(let recording):
                result = .success(try await backend.transcribe(worker: index, samples16k: Resampler.to16k(recording.samples[...], sampleRate: recording.sampleRate),
                                                               language: job.language))
            case .stream(let feed):
                let streamed = try await transcribeStream(feed, worker: index, language: job.language)
                duration = streamed.duration
                // Report the wait after the client stopped sending, not the time spent listening.
                started = streamed.uploadFinishedAt
                result = .success(streamed.text)
            }
        } catch {
            result = .failure(error)
        }
        let processing = ContinuousClock.now - started

        switch result {
        case .failure(let error) where error is StreamAborted:
            metrics.abandonJob(worker: index)
            job.result.fulfil(.failure(.workerFailed(Self.streamAborted)))
        case .success(let text):
            metrics.completeJob(worker: index, durationSeconds: duration, backend: Self.backendID, model: model, source: job.source,
                                client: job.client, queueWait: queueWait, processing: processing)
            job.result.fulfil(.success(TranscriptionOutcome(text: text, backend: Self.backendID, model: model, durationSeconds: duration)))
        case .failure(let error):
            let message = error.localizedDescription
            if needsLoad && loaded != target {
                // The load itself failed; the idle loop re-reports the worker's model state.
                loaded = nil
            }
            metrics.failJob(worker: index, client: job.client, error: message)
            job.result.fulfil(.failure(.workerFailed(message)))
        }
    }

    private struct StreamedTranscript {
        var text: String
        var duration: Double
        var uploadFinishedAt: ContinuousClock.Instant
    }

    /// Decodes a live stream in segments while the client speaks (see `StreamSegmenter`),
    /// leaving only the tail for after the upload ends.
    private func transcribeStream(_ feed: StreamFeed, worker: Int, language: String) async throws -> StreamedTranscript {
        let segmenter = StreamSegmenter()
        var pending: [Int16] = []
        var rate = 16_000
        var total = 0
        var texts: [String] = []
        for await input in feed.stream {
            switch input {
            case .abort:
                throw StreamAborted()
            case .frame(let samples, let sampleRate):
                rate = sampleRate
                total += samples.count
                pending += samples
                if let cut = segmenter.cutPoint(pending[...], sampleRate: rate) {
                    let segment = Resampler.to16k(pending[..<cut], sampleRate: rate)
                    pending.removeFirst(cut)
                    let text = try await backend.transcribe(worker: worker, samples16k: segment, language: language)
                    if !text.trimmingCharacters(in: .whitespaces).isEmpty { texts.append(text.trimmingCharacters(in: .whitespaces)) }
                }
            }
        }
        if Task.isCancelled { throw StreamAborted() }
        let finishedAt = feed.endedAt ?? ContinuousClock.now
        if !pending.isEmpty {
            let text = try await backend.transcribe(worker: worker, samples16k: Resampler.to16k(pending[...], sampleRate: rate), language: language)
            if !text.trimmingCharacters(in: .whitespaces).isEmpty { texts.append(text.trimmingCharacters(in: .whitespaces)) }
        }
        return StreamedTranscript(text: texts.joined(separator: " "), duration: Double(total) / Double(max(1, rate)), uploadFinishedAt: finishedAt)
    }
}
