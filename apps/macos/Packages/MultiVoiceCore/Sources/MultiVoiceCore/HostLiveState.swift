import Foundation

/// Visual cue emitted by `HostLiveState.apply` so the view can animate what just happened
/// (particles, pulses) without diffing state itself.
public enum FlowCue: Equatable, Sendable {
    case streamStarted(client: String)
    case streamFinished(client: String)
    case queued(jobID: String, client: String)
    case dispatched(jobID: String, client: String, worker: Int)
    case completed(jobID: String, client: String, worker: Int, model: String)
    case failed(jobID: String, client: String, worker: Int?)
}

/// Live, render-ready model of a transcription host, built from a snapshot plus
/// incremental events. Pure value type: the Server view, polling fallback and the demo
/// simulator all drive it the same way, and tests cover it without UI.
public struct HostLiveState: Equatable, Sendable {
    public struct WorkerNode: Identifiable, Equatable, Sendable {
        public var id: Int
        public var state: String
        public var model: String
        public var jobID: String?
        public var jobClient: String?
        public var jobStartedAt: Double?
        public var jobAudioSeconds: Double = 0
        public var completedJobs: Int = 0
        public var lastError: String?
        public var isBusy: Bool { jobID != nil || state == "transcribing" }
    }

    public struct ClientNode: Identifiable, Equatable, Sendable {
        public var id: String
        public var activeStreams: Int = 0
        public var requests: Int = 0
        public var completed: Int = 0
        public var failed: Int = 0
        public var lastSeenMs: Double = 0
        public var lastModel: String?
        public var displayName: String { HostLiveState.displayName(forClient: id) }
    }

    public struct ModelNode: Identifiable, Equatable, Sendable {
        public var id: String
        public var name: String
        public var publisher: String
        public var sizeBytes: Double
        public var installed: Bool
        public var assignedWorkers: [Int]
        public var downloadPercentage: Double?
        public var downloadStage: String?
        public var downloadError: String?
    }

    public struct QueueItem: Identifiable, Equatable, Sendable {
        public var id: String
        public var client: String
        public var model: String
        public var audioSeconds: Double
        public var enqueuedAt: Double
        public var source: String = ""
    }

    public struct Completion: Equatable, Sendable {
        public var at: Double
        public var client: String
        public var model: String
        public var worker: Int?
        public var audioSeconds: Double
        public var processingMs: Double
        public var queueWaitMs: Double
        public var failed: Bool
        public var source: String = ""

        /// Release to text on the host. A streamed job's processing time already runs from the
        /// end of the upload, and its queue wait overlaps the time the client was speaking.
        public var latencyMs: Double { source == "stream" ? processingMs : queueWaitMs + processingMs }
    }

    public static let unknownClient = "unknown"
    public static let maxClients = 12
    static let maxCompletions = 240

    public var serverVersion = ""
    public var bindAddr = ""
    public var useGpu = false
    public var queueCapacity = 0
    public var maxActiveStreams = 0
    public var uptimeSeconds: Double = 0
    public var uptimeMeasuredAt: Double = 0
    public var totalTranscriptions = 0
    public var failedJobs = 0
    public var rejectedJobs = 0
    public var totalAudioSeconds: Double = 0
    public var averageProcessingMs: Double = 0
    public var averageQueueMs: Double = 0

    public var workers: [WorkerNode] = []
    public var clients: [ClientNode] = []
    public var models: [ModelNode] = []
    public var queue: [QueueItem] = []
    /// streamId → client key.
    public var streams: [String: String] = [:]
    public var completions: [Completion] = []
    /// Wall-clock ms of the last applied event or snapshot.
    public var lastEventAt: Double = 0

    public init() {}

    // MARK: Reducer

    @discardableResult
    public mutating func apply(_ event: HostEvent, now: Double = Date().timeIntervalSince1970 * 1000) -> [FlowCue] {
        lastEventAt = now
        switch event {
        case .snapshot(let stats):
            reconcile(with: stats, now: now)
            return []

        case .streamStarted(let e):
            let client = Self.key(e.client)
            streams[e.streamId] = client
            touchClient(client, at: e.at) { $0.activeStreams += 1; $0.requests += 1 }
            return [.streamStarted(client: client)]

        case .streamFinished(let e):
            let client = streams.removeValue(forKey: e.streamId) ?? Self.key(e.client)
            touchClient(client, at: e.at) { $0.activeStreams = max(0, $0.activeStreams - 1) }
            return [.streamFinished(client: client)]

        case .jobQueued(let e):
            let client = Self.key(e.client)
            ensureModel(e.model)
            queue.removeAll { $0.id == e.jobId }
            queue.append(QueueItem(id: e.jobId, client: client, model: e.model, audioSeconds: e.audioSeconds, enqueuedAt: e.at, source: e.source))
            touchClient(client, at: e.at) { $0.lastModel = e.model }
            return [.queued(jobID: e.jobId, client: client)]

        case .jobStarted(let e):
            let queued = queue.first { $0.id == e.jobId }
            queue.removeAll { $0.id == e.jobId }
            let client = queued?.client ?? Self.key(e.client)
            ensureModel(e.model)
            pendingQueueWait[e.jobId] = e.queueWaitMs
            pendingSource[e.jobId] = queued?.source ?? ""
            if pendingQueueWait.count > 512 { pendingQueueWait.removeAll(); pendingSource.removeAll() }
            updateWorker(e.worker, model: e.model) {
                $0.state = "transcribing"
                $0.jobID = e.jobId
                $0.jobClient = client
                $0.jobStartedAt = e.at
                $0.jobAudioSeconds = queued?.audioSeconds ?? 0
            }
            return [.dispatched(jobID: e.jobId, client: client, worker: e.worker)]

        case .jobCompleted(let e):
            let client = Self.key(e.client)
            queue.removeAll { $0.id == e.jobId }
            updateWorker(e.worker, model: e.model) {
                $0.state = "idle"
                $0.jobID = nil
                $0.jobClient = nil
                $0.jobStartedAt = nil
                $0.completedJobs += 1
            }
            let queueWait = pendingQueueWait.removeValue(forKey: e.jobId) ?? 0
            let source = pendingSource.removeValue(forKey: e.jobId) ?? ""
            totalTranscriptions += 1
            totalAudioSeconds += e.audioSeconds
            touchClient(client, at: e.at) { $0.completed += 1; $0.lastModel = e.model }
            record(Completion(at: e.at, client: client, model: e.model, worker: e.worker, audioSeconds: e.audioSeconds,
                              processingMs: e.processingMs, queueWaitMs: queueWait, failed: false, source: source))
            return [.completed(jobID: e.jobId, client: client, worker: e.worker, model: e.model)]

        case .jobFailed(let e):
            let queued = queue.first { $0.id == e.jobId }
            queue.removeAll { $0.id == e.jobId }
            let client = queued?.client ?? Self.key(e.client)
            if let w = e.worker {
                updateWorker(w, model: nil) {
                    $0.state = "idle"
                    $0.jobID = nil
                    $0.jobClient = nil
                    $0.jobStartedAt = nil
                    $0.lastError = e.error
                }
            }
            failedJobs += 1
            touchClient(client, at: e.at) { $0.failed += 1 }
            record(Completion(at: e.at, client: client, model: queued?.model ?? "", worker: e.worker, audioSeconds: queued?.audioSeconds ?? 0,
                              processingMs: 0, queueWaitMs: 0, failed: true))
            return [.failed(jobID: e.jobId, client: client, worker: e.worker)]

        case .workerState(let e):
            updateWorker(e.worker, model: e.model) {
                $0.state = e.state
                if e.state != "transcribing" {
                    $0.jobID = nil
                    $0.jobClient = nil
                    $0.jobStartedAt = nil
                }
            }
            return []

        case .modelDownload(let e):
            ensureModel(e.model)
            if let i = models.firstIndex(where: { $0.id == e.model }) {
                let finished = e.stage == "ready" || e.stage == "error"
                models[i].downloadPercentage = finished ? nil : e.percentage
                models[i].downloadStage = finished ? nil : e.stage
                models[i].downloadError = e.stage == "error" ? (e.error ?? "Download failed") : nil
                if e.stage == "ready" { models[i].installed = true }
            }
            return []

        case .unknown:
            return []
        }
    }

    /// queue wait per job, remembered at dispatch so completions can report wait + processing.
    private var pendingQueueWait: [String: Double] = [:]
    private var pendingSource: [String: String] = [:]

    /// Folds a full snapshot in without disturbing stable node ordering.
    public mutating func reconcile(with s: HostStats, now: Double) {
        serverVersion = s.serverVersion
        bindAddr = s.bindAddr
        useGpu = s.useGpu
        queueCapacity = s.queueCapacity
        maxActiveStreams = s.maxActiveStreams
        uptimeSeconds = s.uptimeSeconds
        uptimeMeasuredAt = now
        totalTranscriptions = s.totalTranscriptions
        failedJobs = s.failedJobs
        rejectedJobs = s.rejectedJobs
        totalAudioSeconds = s.totalAudioSeconds
        averageProcessingMs = s.averageProcessingMs
        averageQueueMs = s.averageQueueMs

        // Workers: authoritative from the snapshot.
        var nextWorkers: [WorkerNode] = []
        for w in s.workers.sorted(by: { $0.index < $1.index }) {
            var node = workers.first { $0.id == w.index } ?? WorkerNode(id: w.index, state: w.state, model: w.assignedModel)
            node.state = w.state
            node.model = w.loadedModel ?? w.assignedModel
            if node.model.isEmpty { node.model = w.assignedModel }
            node.completedJobs = w.completedJobs
            node.lastError = w.lastError
            if let job = w.job {
                node.jobClient = Self.key(job.client)
                node.jobAudioSeconds = job.audioSeconds
                if let id = job.id { node.jobID = id } else if node.jobID == nil { node.jobID = "snapshot-\(w.index)" }
                node.jobStartedAt = now - job.elapsedMs
            } else if w.state != "transcribing" {
                node.jobID = nil
                node.jobClient = nil
                node.jobStartedAt = nil
            }
            nextWorkers.append(node)
        }
        if nextWorkers.isEmpty && s.workerCount > 0 {
            nextWorkers = (0..<s.workerCount).map { WorkerNode(id: $0, state: "idle", model: s.model) }
        }
        workers = nextWorkers

        // Queue.
        queue = s.queue.map {
            QueueItem(id: $0.id, client: Self.key($0.client), model: $0.model, audioSeconds: $0.audioSeconds, enqueuedAt: now - $0.waitingMs,
                      source: $0.source)
        }

        // Clients: keep existing order, refresh stats, append new ones.
        for c in s.clients {
            touchClient(c.address, at: c.lastSeenMs) {
                $0.requests = c.requests
                $0.completed = c.completed
                $0.failed = c.failed
                $0.lastModel = c.lastModel
            }
        }
        // Streams: the snapshot has no ids, so rebuild per-client counts.
        streams = [:]
        for i in clients.indices { clients[i].activeStreams = 0 }
        for (n, st) in s.streams.enumerated() {
            let key = Self.key(st.client)
            streams[st.id ?? "snapshot-\(n)"] = key
            touchClient(key, at: now) { $0.activeStreams += 1 }
        }

        // Models: the optional catalogue wins; otherwise derive from worker assignments.
        if let list = s.models, !list.isEmpty {
            models = list.map { m in
                let prior = models.first { $0.id == m.id }
                return ModelNode(id: m.id, name: m.name.isEmpty ? Self.prettyModelName(m.id) : m.name, publisher: m.publisher,
                                 sizeBytes: m.sizeBytes, installed: m.installed, assignedWorkers: m.assignedWorkers,
                                 downloadPercentage: prior?.downloadPercentage, downloadStage: prior?.downloadStage,
                                 downloadError: prior?.downloadError)
            }
        } else {
            for w in workers where !w.model.isEmpty { ensureModel(w.model) }
            for i in models.indices {
                models[i].assignedWorkers = workers.filter { $0.model == models[i].id }.map(\.id)
            }
        }
        if let d = s.modelDownload {
            ensureModel(d.model)
            if let i = models.firstIndex(where: { $0.id == d.model }) {
                let active = d.stage != "ready" && d.stage != "error"
                models[i].downloadPercentage = active ? d.percentage : nil
                models[i].downloadStage = active ? d.stage : nil
            }
        }

        // Seed latency history from `recent` so the charts are not empty on connect.
        if completions.isEmpty {
            for r in s.recent.reversed() {
                record(Completion(at: r.completedAtMs, client: Self.key(r.client), model: r.model, worker: nil,
                                  audioSeconds: r.durationSeconds, processingMs: r.processingMs, queueWaitMs: r.queueWaitMs, failed: false,
                                  source: r.source))
            }
        }
    }

    // MARK: Metrics

    public struct Metrics: Equatable, Sendable {
        public var queueDepth: Int
        public var running: Int
        public var activeStreams: Int
        public var jobsPerMinute: Double
        public var audioSecondsPerMinute: Double
        public var latencyP50Ms: Double?
        public var latencyP95Ms: Double?
        public var averageProcessingMs: Double?
        public var speedFactor: Double?
        public var errorRate: Double
    }

    public func metrics(now: Double, window: Double = 60_000) -> Metrics {
        let recent = completions.filter { now - $0.at <= window }
        let ok = recent.filter { !$0.failed }
        let latencies = completions.suffix(60).filter { !$0.failed }.map(\.latencyMs).sorted()
        let processing = ok.map(\.processingMs)
        let audio = ok.reduce(0) { $0 + $1.audioSeconds }
        let procTotal = processing.reduce(0, +)
        let minutes = window / 60_000
        return Metrics(
            queueDepth: queue.count,
            running: workers.filter(\.isBusy).count,
            activeStreams: streams.count,
            jobsPerMinute: Double(ok.count) / minutes,
            audioSecondsPerMinute: audio / minutes,
            latencyP50Ms: Self.percentile(latencies, 0.5),
            latencyP95Ms: Self.percentile(latencies, 0.95),
            averageProcessingMs: processing.isEmpty ? (averageProcessingMs > 0 ? averageProcessingMs : nil) : procTotal / Double(processing.count),
            speedFactor: procTotal > 0 ? audio * 1000 / procTotal : nil,
            errorRate: recent.isEmpty ? 0 : Double(recent.count - ok.count) / Double(recent.count)
        )
    }

    public func estimatedUptime(now: Double) -> Double {
        uptimeSeconds + max(0, now - uptimeMeasuredAt) / 1000
    }

    static func percentile(_ sorted: [Double], _ p: Double) -> Double? {
        guard !sorted.isEmpty else { return nil }
        let rank = p * Double(sorted.count - 1)
        let lo = Int(rank.rounded(.down)), hi = Int(rank.rounded(.up))
        let f = rank - Double(lo)
        return sorted[lo] * (1 - f) + sorted[hi] * f
    }

    // MARK: Helpers

    public static func key(_ client: String?) -> String {
        guard let c = client?.trimmingCharacters(in: .whitespaces), !c.isEmpty else { return unknownClient }
        return c
    }

    /// `alice@example.com` (tailscale-user-login) → `alice`; IPs and names unchanged.
    public static func displayName(forClient key: String) -> String {
        if key == unknownClient { return "Unknown client" }
        if let at = key.firstIndex(of: "@"), at != key.startIndex { return String(key[..<at]) }
        return key
    }

    public static func prettyModelName(_ id: String) -> String {
        switch id {
        case "parakeet-tdt-0.6b-v3", "parakeet": "Parakeet TDT v3"
        case "parakeet-tdt-0.6b-v2": "Parakeet TDT v2"
        case "parakeet-ultra": "Parakeet Ultra"
        case "parakeet-redux": "Parakeet Redux"
        case "mixed": "Mixed"
        default: id
        }
    }

    private mutating func record(_ completion: Completion) {
        completions.append(completion)
        if completions.count > Self.maxCompletions { completions.removeFirst(completions.count - Self.maxCompletions) }
    }

    /// `"mixed"` is a placeholder (`job_queued.model` when workers differ), not a model.
    private mutating func ensureModel(_ id: String) {
        guard !id.isEmpty, id != "mixed", !models.contains(where: { $0.id == id }) else { return }
        models.append(ModelNode(id: id, name: Self.prettyModelName(id), publisher: id.hasPrefix("parakeet") ? "NVIDIA" : "",
                                sizeBytes: 0, installed: true, assignedWorkers: workers.filter { $0.model == id }.map(\.id)))
    }

    private mutating func updateWorker(_ index: Int, model: String?, _ change: (inout WorkerNode) -> Void) {
        if let i = workers.firstIndex(where: { $0.id == index }) {
            if let model, !model.isEmpty { workers[i].model = model }
            change(&workers[i])
        } else {
            var node = WorkerNode(id: index, state: "idle", model: model ?? "")
            change(&node)
            workers.append(node)
            workers.sort { $0.id < $1.id }
        }
        if let model, !model.isEmpty {
            for m in models.indices {
                let assigned = workers.filter { $0.model == models[m].id }.map(\.id)
                if !assigned.isEmpty || models[m].id == model { models[m].assignedWorkers = assigned }
            }
        }
    }

    private mutating func touchClient(_ key: String, at: Double, _ change: (inout ClientNode) -> Void) {
        if let i = clients.firstIndex(where: { $0.id == key }) {
            clients[i].lastSeenMs = max(clients[i].lastSeenMs, at)
            change(&clients[i])
        } else {
            var node = ClientNode(id: key, lastSeenMs: at)
            change(&node)
            clients.append(node)
            if clients.count > Self.maxClients {
                // Evict the least recently seen client that is not streaming.
                if let victim = clients.enumerated().filter({ $0.element.activeStreams == 0 && $0.element.id != key })
                    .min(by: { $0.element.lastSeenMs < $1.element.lastSeenMs })?.offset {
                    clients.remove(at: victim)
                }
            }
        }
    }
}
