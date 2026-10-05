import Foundation

/// Deterministic stand-in for a busy practice host (a Mac mini serving a GP surgery).
/// Emits exactly the `/v1/events` contract in `src-tauri/src/host/PROTOCOL.md`:
/// a stream job is queued (`audioSeconds: 0`, configured model) as soon as the stream
/// opens, a worker picks it up and decodes while the client speaks, and the job
/// completes shortly after the stream closes. Every `job_queued` gets exactly one
/// terminal event, which precedes the worker's `worker_state` back to idle.
/// Time is compressed so activity is visible in the Server view.
public struct HostSimulator: Sendable {
    public struct Config: Sendable {
        public var seed: UInt64 = 0x4D56_2026
        /// Mean seconds between new dictations across all clients.
        public var meanArrivalSeconds: Double = 1.0
        /// Wall seconds a stream stays open per second of audio (time compression).
        public var streamTimeScale: Double = 0.14
        public var failureRate: Double = 0.02
        public var queueCapacity = 8
        public var clients: [String] = [
            "dr.byrne@practice.example", "dr.okeeffe@practice.example", "nurse-station", "reception-2",
            "100.88.14.22", "dr.walsh@practice.example",
        ]
        public init() {}
    }

    private struct Pending: Sendable {
        var at: Double
        var event: HostEvent
    }

    private struct Job: Sendable {
        var id: String
        var streamID: String
        var client: String
        var audio: Double
        var streamEnd: Double
        var queuedAt: Double
    }

    private struct Worker: Sendable {
        var index: Int
        var model: String
        var speed: Double // RTFx
        var job: Job?
        var completed = 0
    }

    public private(set) var config: Config
    private var rng: SplitMix64
    private var now: Double
    private let startedAt: Double
    private var nextArrival: Double
    private var pending: [Pending] = []
    private var queue: [Job] = []
    private var workers: [Worker]
    private var activeStreams: [(id: String, client: String)] = []
    private var streamSerial = 0
    private var jobSerial = 1_300
    private var totals = (transcriptions: 0, audio: 0.0, failed: 0, rejected: 0, queueMs: 0.0, procMs: 0.0)
    private var recent: [HostStats.Record] = []
    private var clientStats: [String: HostStats.Client] = [:]

    public init(config: Config = Config(), now: Double = Date().timeIntervalSince1970 * 1000) {
        self.config = config
        self.rng = SplitMix64(seed: config.seed)
        self.now = now
        self.startedAt = now - 3 * 3600 * 1000 - 1_234_000
        self.nextArrival = now + 250
        self.workers = [
            Worker(index: 0, model: "parakeet-tdt-0.6b-v3", speed: 128),
            Worker(index: 1, model: "parakeet-tdt-0.6b-v3", speed: 128),
            Worker(index: 2, model: "parakeet-ultra", speed: 120),
        ]
        // Plausible history so totals and charts are populated on first paint.
        totals = (1_284, 1_284 * 21.5, 3, 0, 1_284 * 38, 1_284 * 190)
        for (i, client) in config.clients.enumerated() {
            clientStats[client] = HostStats.Client(address: client, requests: 180 + i * 37, completed: 178 + i * 37,
                                                   rejected: 0, failed: i == 3 ? 1 : 0, totalAudioSeconds: Double(3_100 + i * 640),
                                                   lastSeenMs: now - Double(i) * 41_000, lastModel: "parakeet-tdt-0.6b-v3")
        }
        for i in 0..<24 {
            let audio = 6 + Double(i * 7 % 31)
            let model = i % 3 == 2 ? "parakeet-ultra" : "parakeet-tdt-0.6b-v3"
            recent.append(HostStats.Record(id: "\(1_284 - i)", completedAtMs: now - Double(i) * 9_500, durationSeconds: audio,
                                           model: model, source: "stream", client: config.clients[i % config.clients.count],
                                           queueWaitMs: Double((i * 13) % 70), processingMs: audio * 1000 / 125 + Double(i % 5) * 9))
        }
    }

    public var currentTime: Double { now }

    /// The `snapshot` event body (same shape as `GET /v1/stats`).
    public func snapshot() -> HostStats {
        var s = HostStats()
        s.serverVersion = "0.9.0-demo"
        s.bindAddr = "127.0.0.1:48173"
        s.uptimeSeconds = ((now - startedAt) / 1000).rounded()
        s.activeStreams = activeStreams.count
        s.queuedJobs = queue.count
        s.runningJobs = workers.filter { $0.job != nil }.count
        s.activeSessions = s.activeStreams + s.runningJobs
        s.workerCount = workers.count
        s.queueCapacity = config.queueCapacity
        s.maxActiveStreams = 8
        s.maxRecordingSeconds = 600
        s.useGpu = true
        s.model = "mixed"
        s.totalTranscriptions = totals.transcriptions
        s.totalAudioSeconds = totals.audio
        s.failedJobs = totals.failed
        s.rejectedJobs = totals.rejected
        s.averageQueueMs = (totals.queueMs / Double(max(1, totals.transcriptions))).rounded()
        s.averageProcessingMs = (totals.procMs / Double(max(1, totals.transcriptions))).rounded()
        s.workers = workers.map { w in
            HostStats.Worker(index: w.index, state: w.job != nil ? "transcribing" : "idle", assignedModel: w.model,
                             loadedModel: w.model, completedJobs: 420 + w.completed + w.index * 31,
                             job: w.job.map { HostStats.RunningJob(id: $0.id, model: w.model, source: "stream", client: $0.client,
                                                                   audioSeconds: 0, elapsedMs: now - $0.queuedAt) })
        }
        s.queue = queue.map {
            HostStats.QueuedJob(id: $0.id, model: "mixed", source: "stream", client: $0.client, audioSeconds: 0, waitingMs: now - $0.queuedAt)
        }
        s.streams = activeStreams.map { HostStats.Stream(id: $0.id, client: $0.client, elapsedMs: 0) }
        s.clients = clientStats.values.sorted { $0.lastSeenMs > $1.lastSeenMs }
        s.recent = recent
        s.models = [
            HostStats.Model(id: "parakeet-tdt-0.6b-v3", name: "Parakeet TDT 0.6B v3", publisher: "NVIDIA", sizeBytes: 482_000_000,
                            installed: true, assignedWorkers: [0, 1]),
            HostStats.Model(id: "parakeet-ultra", name: "Parakeet Ultra", publisher: "Moondream", sizeBytes: 643_000_000,
                            installed: true, assignedWorkers: [2]),
            HostStats.Model(id: "parakeet-tdt-0.6b-v2", name: "Parakeet TDT 0.6B v2", publisher: "NVIDIA", sizeBytes: 473_000_000,
                            installed: false, assignedWorkers: []),
        ]
        return s
    }

    /// Advances simulated time to `time` (epoch ms) and returns the events that happened, in order.
    public mutating func advance(to time: Double) -> [HostEvent] {
        var out: [HostEvent] = []
        // Cap catch-up after the app slept or the view was hidden.
        if time - now > 5_000 {
            let skip = time - 5_000 - now
            now += skip
            nextArrival = max(nextArrival, now)
        }
        while true {
            let nextPending = pending.min { $0.at < $1.at }?.at ?? .infinity
            let t = min(nextArrival, nextPending)
            guard t <= time else { break }
            now = max(now, t)
            if t == nextArrival {
                out += startDictation()
                nextArrival = now + exponential(mean: config.meanArrivalSeconds * 1000)
            } else if let i = pending.firstIndex(where: { $0.at == t }) {
                let item = pending.remove(at: i)
                out += handle(item.event)
            }
        }
        now = max(now, time)
        return out
    }

    // MARK: Internals

    private mutating func startDictation() -> [HostEvent] {
        guard activeStreams.count < 8 else { return [] }
        let busy = Set(activeStreams.map(\.client))
        let free = config.clients.filter { !busy.contains($0) }
        guard !free.isEmpty else { return [] }
        let client = free[Int(rng.next() % UInt64(free.count))]
        streamSerial += 1
        let streamID = "\(streamSerial)"
        activeStreams.append((streamID, client))
        clientStats[client, default: HostStats.Client(address: client)].requests += 1
        clientStats[client]?.lastSeenMs = now
        // Audio length: mostly short notes, some long letters.
        let audio = rng.unit() < 0.8 ? 3 + rng.unit() * 22 : 25 + rng.unit() * 70
        let end = now + max(600, audio * 1000 * config.streamTimeScale)
        var out: [HostEvent] = [.streamStarted(.init(streamId: streamID, client: client, at: now))]
        pending.append(Pending(at: end, event: .streamFinished(.init(streamId: streamID, client: client, audioSeconds: audio, at: end))))

        jobSerial += 1
        let job = Job(id: "\(jobSerial)", streamID: streamID, client: client, audio: audio, streamEnd: end, queuedAt: now)
        guard queue.count < config.queueCapacity else {
            totals.rejected += 1
            out.append(.jobQueued(.init(jobId: job.id, client: client, source: "stream", model: "mixed", audioSeconds: 0, at: now)))
            out.append(.jobFailed(.init(jobId: job.id, worker: nil, client: client, error: "Transcription queue is full", at: now)))
            return out
        }
        queue.append(job)
        out.append(.jobQueued(.init(jobId: job.id, client: client, source: "stream", model: "mixed", audioSeconds: 0, at: now)))
        return out + dispatch()
    }

    private mutating func handle(_ event: HostEvent) -> [HostEvent] {
        switch event {
        case .streamFinished(let e):
            activeStreams.removeAll { $0.id == e.streamId }
            return [event]
        case .jobCompleted(let e):
            totals.transcriptions += 1
            totals.audio += e.audioSeconds
            totals.procMs += e.processingMs
            if let client = e.client {
                clientStats[client]?.completed += 1
                clientStats[client]?.totalAudioSeconds += e.audioSeconds
                clientStats[client]?.lastSeenMs = now
                clientStats[client]?.lastModel = e.model
            }
            recent.insert(HostStats.Record(id: e.jobId, completedAtMs: now, durationSeconds: e.audioSeconds, model: e.model,
                                           source: "stream", client: e.client, queueWaitMs: 0, processingMs: e.processingMs), at: 0)
            if recent.count > 24 { recent.removeLast() }
            return [event] + release(worker: e.worker)
        case .jobFailed(let e):
            totals.failed += 1
            if let client = e.client { clientStats[client]?.failed += 1 }
            return [event] + release(worker: e.worker)
        default:
            return [event]
        }
    }

    /// Frees a worker after its terminal event: picks up the next job, or reports idle.
    private mutating func release(worker index: Int?) -> [HostEvent] {
        guard let index, let w = workers.firstIndex(where: { $0.index == index }) else { return dispatch() }
        workers[w].job = nil
        workers[w].completed += 1
        let next = dispatch()
        if workers[w].job == nil {
            return next + [.workerState(.init(worker: index, state: "idle", model: workers[w].model, at: now))]
        }
        return next
    }

    /// Hands queued jobs (FIFO) to free workers.
    private mutating func dispatch() -> [HostEvent] {
        var out: [HostEvent] = []
        while !queue.isEmpty, let w = workers.indices.filter({ workers[$0].job == nil }).randomElement(using: &rng) {
            let job = queue.removeFirst()
            let wait = now - job.queuedAt
            totals.queueMs += wait
            workers[w].job = job
            out.append(.workerState(.init(worker: workers[w].index, state: "transcribing", model: workers[w].model, at: now)))
            out.append(.jobStarted(.init(jobId: job.id, worker: workers[w].index, model: workers[w].model, client: job.client,
                                         queueWaitMs: wait.rounded(), at: now)))
            // The worker decoded while the client spoke; the tail after release is short.
            let processing = (job.audio * 1000 / workers[w].speed + 25 + rng.unit() * 40).rounded()
            let doneAt = max(now, job.streamEnd) + max(380, processing * 2.5)
            let done: HostEvent = rng.unit() < config.failureRate
                ? .jobFailed(.init(jobId: job.id, worker: workers[w].index, client: job.client, error: "Stream upload aborted", at: doneAt))
                : .jobCompleted(.init(jobId: job.id, worker: workers[w].index, model: workers[w].model, client: job.client,
                                      audioSeconds: job.audio, processingMs: processing, at: doneAt))
            pending.append(Pending(at: doneAt, event: done))
        }
        return out
    }

    private mutating func exponential(mean: Double) -> Double {
        -log(max(1e-6, 1 - rng.unit())) * mean
    }
}

/// Small, fast, seedable PRNG (SplitMix64) so demos and tests are reproducible.
public struct SplitMix64: RandomNumberGenerator, Sendable {
    private var state: UInt64
    public init(seed: UInt64) { state = seed }
    public mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
    public mutating func unit() -> Double { Double(next() >> 11) / Double(1 << 53) }
}
