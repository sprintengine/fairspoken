import Foundation
import Synchronization

/// Worker `state` values in `/v1/stats` and `worker_state` events.
public enum WorkerPhase: String, Sendable {
    case idle
    case loading
    case transcribing
    case modelUnavailable = "model-unavailable"
}

struct RunningJobInfo: Sendable {
    var id: UInt64
    var model: String
    var source: String
    var client: String?
    var audioSeconds: Double
    var startedAt: ContinuousClock.Instant
}

struct QueuedJobInfo: Sendable {
    var id: UInt64
    var model: String
    var source: String
    var client: String?
    var audioSeconds: Double
    var enqueuedAt: ContinuousClock.Instant
}

/// Progress of an operator-initiated model download (`modelDownload` in `/v1/stats`).
public struct ModelDownloadState: Sendable, Equatable {
    public var model: String
    public var stage: String
    public var percentage: Int
    public var error: String?
    public var inProgress: Bool { stage != "ready" && stage != "error" }
}

func nowEpochMs() -> UInt64 { UInt64((Date().timeIntervalSince1970 * 1000).rounded(.down)) }

/// Everything `/v1/stats` reports, mutated only under one lock that also orders event
/// publication: each mutation publishes while holding it, so a snapshot taken together
/// with a subscription (`/v1/events`) has no gap or overlap with the events that follow.
/// A port of `HostMetrics` in src-tauri/src/host/mod.rs.
public final class HostMetrics: Sendable {
    public static let recentCapacity = 50
    public static let maxTrackedClients = 32

    struct WorkerStatus {
        var state: WorkerPhase = .idle
        var loadedModel: String?
        var job: RunningJobInfo?
        var completedJobs: UInt64 = 0
        var lastError: String?
        var published: (WorkerPhase, String?)?
    }

    struct ClientStats {
        var requests: UInt64 = 0
        var completed: UInt64 = 0
        var rejected: UInt64 = 0
        var failed: UInt64 = 0
        var totalAudioSeconds: Double = 0
        var lastSeenMs: UInt64
        var lastModel: String?
    }

    struct Record {
        var id: UInt64
        var completedAtMs: UInt64
        var durationSeconds: Double
        var backend: String
        var model: String
        var source: String
        var client: String?
        var queueWaitMs: UInt64
        var processingMs: UInt64
    }

    struct State {
        var started = ContinuousClock.now
        var workerCount: Int
        var queueCapacity: Int
        var nextStreamID: UInt64 = 0
        var activeStreams: [(id: UInt64, client: String?, startedAt: ContinuousClock.Instant)] = []
        var queued: [QueuedJobInfo] = []
        var workers: [WorkerStatus]
        var rejectedJobs: UInt64 = 0
        var failedJobs: UInt64 = 0
        var startedJobs: UInt64 = 0
        var totalTranscriptions: UInt64 = 0
        var totalAudioSeconds: Double = 0
        var totalQueueWaitMs: UInt64 = 0
        var totalProcessingMs: UInt64 = 0
        var nextRecordID: UInt64 = 0
        var recent: [Record] = []
        var clients: [String: ClientStats] = [:]
        var modelDownload: ModelDownloadState?
    }

    let state: Mutex<State>
    public let events: EventHub

    public init(workerCount: Int, queueCapacity: Int, events: EventHub = EventHub()) {
        state = Mutex(State(workerCount: workerCount, queueCapacity: queueCapacity,
                            workers: Array(repeating: WorkerStatus(), count: workerCount)))
        self.events = events
    }

    /// Runs `body` under the metrics lock (snapshot + subscribe atomically).
    func locked<T>(_ body: (inout State) throws -> T) rethrows -> T {
        try state.withLock { s in try body(&s) }
    }

    private static func publish(_ event: HostWireEvent, _ hub: EventHub) {
        guard hub.hasListeners else { return }
        hub.publish(event.frame(at: nowEpochMs()))
    }

    private func publish(_ event: HostWireEvent) { Self.publish(event, events) }

    /// Publishes a worker's state only when it differs from the last one sent.
    private func publishWorkerState(_ s: inout State, _ index: Int, model: String?) {
        guard s.workers.indices.contains(index) else { return }
        let current = (s.workers[index].state, model)
        if let p = s.workers[index].published, p.0 == current.0, p.1 == current.1 { return }
        s.workers[index].published = current
        publish(.workerState(worker: index, state: current.0.rawValue, model: model))
    }

    /// Frees a worker after its job ended: the job's terminal event, then the worker's idle state.
    private func releaseWorker(_ s: inout State, _ index: Int, terminal: (RunningJobInfo) -> HostWireEvent) {
        guard s.workers.indices.contains(index) else { return }
        s.workers[index].state = .idle
        let job = s.workers[index].job
        s.workers[index].job = nil
        if let job { publish(terminal(job)) }
        publishWorkerState(&s, index, model: s.workers[index].loadedModel)
    }

    private func touchClient(_ s: inout State, _ client: String?, _ change: (inout ClientStats) -> Void) {
        guard let address = client else { return }
        let now = nowEpochMs()
        if s.clients[address] == nil, s.clients.count >= Self.maxTrackedClients,
           let oldest = s.clients.min(by: { $0.value.lastSeenMs < $1.value.lastSeenMs })?.key {
            s.clients.removeValue(forKey: oldest)
        }
        var stats = s.clients[address] ?? ClientStats(lastSeenMs: now)
        stats.lastSeenMs = now
        change(&stats)
        s.clients[address] = stats
    }

    // MARK: Mutations (each publishes its event under the lock)

    func setModelDownload(_ next: ModelDownloadState) {
        state.withLock { s in
            if let c = s.modelDownload, c == next { return }
            publish(.modelDownload(model: next.model, stage: next.stage, percentage: next.percentage, error: next.error))
            s.modelDownload = next
        }
    }

    /// Starts a download unless one is already running.
    func beginModelDownload(_ model: String) -> Bool {
        state.withLock { s in
            if s.modelDownload?.inProgress == true { return false }
            let next = ModelDownloadState(model: model, stage: ModelDownloadStage.starting.rawValue, percentage: 0, error: nil)
            publish(.modelDownload(model: model, stage: next.stage, percentage: 0, error: nil))
            s.modelDownload = next
            return true
        }
    }

    var activeStreamCount: Int { state.withLock { $0.activeStreams.count } }

    /// Opens a stream unless `maxActiveStreams` are open (that rejection counts against the client).
    func tryBeginStream(client: String?, maxActiveStreams: Int) -> UInt64? {
        state.withLock { s in
            if s.activeStreams.count >= maxActiveStreams {
                s.rejectedJobs += 1
                touchClient(&s, client) { $0.rejected += 1 }
                return nil
            }
            s.nextStreamID += 1
            let id = s.nextStreamID
            publish(.streamStarted(streamId: id, client: client))
            s.activeStreams.append((id, client, .now))
            return id
        }
    }

    func finishStream(_ id: UInt64, audioSeconds: Double) {
        state.withLock { s in
            guard let i = s.activeStreams.firstIndex(where: { $0.id == id }) else { return }
            let stream = s.activeStreams.remove(at: i)
            publish(.streamFinished(streamId: id, client: stream.client, audioSeconds: audioSeconds))
        }
    }

    func enqueueJob(_ job: QueuedJobInfo) {
        state.withLock { s in
            publish(.jobQueued(jobId: job.id, client: job.client, source: job.source, model: job.model, audioSeconds: job.audioSeconds))
            s.queued.append(job)
        }
    }

    /// A job that never reached a worker (queue full or gone).
    func dropQueuedJob(_ id: UInt64, error: String, rejectClient: Bool) {
        state.withLock { s in
            let client = s.queued.first { $0.id == id }?.client
            s.queued.removeAll { $0.id == id }
            publish(.jobFailed(jobId: id, worker: nil, client: client, error: error))
            if rejectClient {
                s.rejectedJobs += 1
                touchClient(&s, client) { $0.rejected += 1 }
            }
        }
    }

    func startJob(worker: Int, queueWait: Duration, needsLoad: Bool, job: RunningJobInfo) {
        state.withLock { s in
            s.queued.removeAll { $0.id == job.id }
            s.startedJobs += 1
            let waitMs = Self.ms(queueWait)
            s.totalQueueWaitMs += waitMs
            publish(.jobStarted(jobId: job.id, worker: worker, model: job.model, client: job.client, queueWaitMs: waitMs))
            guard s.workers.indices.contains(worker) else { return }
            s.workers[worker].state = needsLoad ? .loading : .transcribing
            s.workers[worker].job = job
            publishWorkerState(&s, worker, model: job.model)
        }
    }

    func workerModelReady(_ worker: Int, model: String) {
        state.withLock { s in
            guard s.workers.indices.contains(worker) else { return }
            s.workers[worker].loadedModel = model
            s.workers[worker].state = .transcribing
            publishWorkerState(&s, worker, model: model)
        }
    }

    func workerPreloading(_ worker: Int, model: String) {
        state.withLock { s in
            guard s.workers.indices.contains(worker) else { return }
            s.workers[worker].state = .loading
            publishWorkerState(&s, worker, model: model)
        }
    }

    func workerPreloadReady(_ worker: Int, model: String) {
        state.withLock { s in
            guard s.workers.indices.contains(worker) else { return }
            s.workers[worker].state = .idle
            s.workers[worker].loadedModel = model
            s.workers[worker].lastError = nil
            publishWorkerState(&s, worker, model: model)
        }
    }

    func workerModelUnavailable(_ worker: Int, model: String, message: String) {
        state.withLock { s in
            guard s.workers.indices.contains(worker) else { return }
            s.workers[worker].state = .modelUnavailable
            s.workers[worker].loadedModel = nil
            s.workers[worker].lastError = message
            publishWorkerState(&s, worker, model: model)
        }
    }

    func rejectJob(client: String?) {
        state.withLock { s in
            s.rejectedJobs += 1
            touchClient(&s, client) { $0.rejected += 1 }
        }
    }

    func clientRequest(_ client: String?) {
        state.withLock { s in touchClient(&s, client) { $0.requests += 1 } }
    }

    /// The client's upload broke off; the handler already counted the rejection.
    func abandonJob(worker: Int) {
        state.withLock { s in
            releaseWorker(&s, worker) { .jobFailed(jobId: $0.id, worker: worker, client: $0.client, error: HostRuntime.streamAborted) }
        }
    }

    func failJob(worker: Int, client: String?, error: String) {
        state.withLock { s in
            s.failedJobs += 1
            if s.workers.indices.contains(worker) { s.workers[worker].lastError = error }
            releaseWorker(&s, worker) { .jobFailed(jobId: $0.id, worker: worker, client: $0.client, error: error) }
            touchClient(&s, client) { $0.failed += 1 }
        }
    }

    func completeJob(worker: Int, durationSeconds: Double, backend: String, model: String, source: String, client: String?,
                     queueWait: Duration, processing: Duration) {
        state.withLock { s in
            let processingMs = Self.ms(processing)
            s.totalTranscriptions += 1
            s.totalAudioSeconds += durationSeconds
            s.totalProcessingMs += processingMs
            s.nextRecordID += 1
            if s.workers.indices.contains(worker) {
                s.workers[worker].completedJobs += 1
                s.workers[worker].lastError = nil
            }
            releaseWorker(&s, worker) {
                .jobCompleted(jobId: $0.id, worker: worker, model: model, client: $0.client, audioSeconds: durationSeconds,
                              processingMs: processingMs)
            }
            touchClient(&s, client) {
                $0.completed += 1
                $0.totalAudioSeconds += durationSeconds
                $0.lastModel = model
            }
            s.recent.insert(Record(id: s.nextRecordID, completedAtMs: nowEpochMs(), durationSeconds: durationSeconds, backend: backend,
                                   model: model, source: source, client: client, queueWaitMs: Self.ms(queueWait),
                                   processingMs: processingMs), at: 0)
            if s.recent.count > Self.recentCapacity { s.recent.removeLast(s.recent.count - Self.recentCapacity) }
        }
    }

    static func ms(_ d: Duration) -> UInt64 {
        let c = d.components
        return UInt64(max(0, c.seconds)) * 1000 + UInt64(max(0, c.attoseconds) / 1_000_000_000_000_000)
    }

    // MARK: Snapshot

    struct ModelInfo {
        var descriptor: HostModelDescriptor
        var installed: Bool
        var installedBytes: Int64?
    }

    /// The `/v1/stats` body. Call with the lock held (`locked`) when pairing with a subscription.
    static func snapshot(_ s: State, bindAddr: String, serverVersion: String, live: HostLiveSettings, models: [ModelInfo]) -> JSONValue {
        let now = ContinuousClock.now
        let installed = Dictionary(uniqueKeysWithValues: models.map { ($0.descriptor.id, $0.installed) })
        let workers: [JSONValue] = s.workers.enumerated().map { index, w in
            let assigned = index < live.workerModels.count ? live.workerModels[index] : HostConfiguration.defaultModel
            let job: JSONValue = w.job.map { j in
                .object([("id", .int(Int64(j.id))), ("model", .string(j.model)), ("source", .string(j.source)),
                         ("client", .optionalString(j.client)), ("audioSeconds", .double(j.audioSeconds)),
                         ("elapsedMs", .int(Int64(ms(now - j.startedAt))))])
            } ?? .null
            return .object([
                ("index", .int(index)), ("state", .string(w.state.rawValue)), ("assignedModel", .string(assigned)),
                ("modelAvailable", .bool(installed[assigned] ?? false)), ("loadedModel", .optionalString(w.loadedModel)),
                ("completedJobs", .int(Int64(w.completedJobs))), ("lastError", .optionalString(w.lastError)), ("job", job),
            ])
        }
        let queue: [JSONValue] = s.queued.map { j in
            .object([("id", .int(Int64(j.id))), ("model", .string(j.model)), ("source", .string(j.source)),
                     ("client", .optionalString(j.client)), ("audioSeconds", .double(j.audioSeconds)),
                     ("waitingMs", .int(Int64(ms(now - j.enqueuedAt))))])
        }
        let streams: [JSONValue] = s.activeStreams.map { st in
            .object([("id", .int(Int64(st.id))), ("client", .optionalString(st.client)),
                     ("elapsedMs", .int(Int64(ms(now - st.startedAt))))])
        }
        let clients: [JSONValue] = s.clients.sorted { $0.value.lastSeenMs > $1.value.lastSeenMs }.map { address, c in
            .object([("address", .string(address)), ("requests", .int(Int64(c.requests))), ("completed", .int(Int64(c.completed))),
                     ("rejected", .int(Int64(c.rejected))), ("failed", .int(Int64(c.failed))),
                     ("totalAudioSeconds", .double(c.totalAudioSeconds)), ("lastSeenMs", .int(Int64(c.lastSeenMs))),
                     ("lastModel", .optionalString(c.lastModel))])
        }
        let recent: [JSONValue] = s.recent.map { r in
            .object([("id", .int(Int64(r.id))), ("completedAtMs", .int(Int64(r.completedAtMs))),
                     ("durationSeconds", .double(r.durationSeconds)), ("backend", .string(r.backend)), ("model", .string(r.model)),
                     ("source", .string(r.source)), ("client", .optionalString(r.client)), ("queueWaitMs", .int(Int64(r.queueWaitMs))),
                     ("processingMs", .int(Int64(r.processingMs)))])
        }
        let modelList: [JSONValue] = models.map { m in
            let assigned = live.workerModels.enumerated().filter { $0.element == m.descriptor.id }.map { JSONValue.int($0.offset) }
            let size = m.installed ? (m.installedBytes.flatMap { $0 > 0 ? $0 : nil } ?? m.descriptor.approxBytes) : m.descriptor.approxBytes
            return .object([("id", .string(m.descriptor.id)), ("name", .string(m.descriptor.name)),
                            ("publisher", .string(m.descriptor.publisher)), ("sizeBytes", .int(size)),
                            ("installed", .bool(m.installed)), ("assignedWorkers", .array(assigned))])
        }
        let download: JSONValue = s.modelDownload.map { d in
            .object([("model", .string(d.model)), ("stage", .string(d.stage)), ("percentage", .int(d.percentage)),
                     ("error", .optionalString(d.error))])
        } ?? .null
        let running = s.workers.filter { $0.job != nil }.count
        func average(_ total: UInt64, _ count: UInt64) -> Int64 { count == 0 ? 0 : Int64(total / count) }
        return .object([
            ("serverVersion", .string(serverVersion)),
            ("bindAddr", .string(bindAddr)),
            ("uptimeSeconds", .int(Int64((now - s.started).components.seconds))),
            ("activeSessions", .int(s.activeStreams.count + running)),
            ("activeStreams", .int(s.activeStreams.count)),
            ("queuedJobs", .int(s.queued.count)),
            ("runningJobs", .int(running)),
            ("workerCount", .int(s.workerCount)),
            ("queueCapacity", .int(s.queueCapacity)),
            ("maxActiveStreams", .int(live.maxActiveStreams)),
            ("maxRecordingSeconds", .int(live.maxRecordingSeconds)),
            ("useGpu", .bool(live.useGpu)),
            ("model", .string(live.modelSummary)),
            ("models", .array(modelList)),
            ("modelDownload", download),
            ("rejectedJobs", .int(Int64(s.rejectedJobs))),
            ("failedJobs", .int(Int64(s.failedJobs))),
            ("totalTranscriptions", .int(Int64(s.totalTranscriptions))),
            ("totalAudioSeconds", .double(s.totalAudioSeconds)),
            ("averageQueueMs", .int(average(s.totalQueueWaitMs, s.startedJobs))),
            ("averageProcessingMs", .int(average(s.totalProcessingMs, s.totalTranscriptions))),
            ("workers", .array(workers)),
            ("queue", .array(queue)),
            ("streams", .array(streams)),
            ("clients", .array(clients)),
            ("recent", .array(recent)),
        ])
    }
}
