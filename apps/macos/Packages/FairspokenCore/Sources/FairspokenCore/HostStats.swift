import Foundation

/// `GET /v1/stats` snapshot (also the data of the first `snapshot` SSE event).
/// Shape follows `StatsSnapshot` in `src-tauri/src/host/mod.rs`. Decoding is
/// deliberately lenient: every field has a default so older and newer hosts both
/// decode, and the optional `models` list (being added) is picked up when present.
public struct HostStats: Codable, Equatable, Sendable {
    public var serverVersion: String = ""
    public var bindAddr: String = ""
    public var uptimeSeconds: Double = 0
    public var activeSessions: Int = 0
    public var activeStreams: Int = 0
    public var queuedJobs: Int = 0
    public var runningJobs: Int = 0
    public var workerCount: Int = 0
    public var queueCapacity: Int = 0
    public var maxActiveStreams: Int = 0
    public var maxRecordingSeconds: Int = 0
    public var useGpu: Bool = false
    public var model: String = ""
    public var modelDownload: ModelDownload?
    public var rejectedJobs: Int = 0
    public var failedJobs: Int = 0
    public var totalTranscriptions: Int = 0
    public var totalAudioSeconds: Double = 0
    public var averageQueueMs: Double = 0
    public var averageProcessingMs: Double = 0
    public var workers: [Worker] = []
    public var queue: [QueuedJob] = []
    public var streams: [Stream] = []
    public var clients: [Client] = []
    public var recent: [Record] = []
    public var models: [Model]?

    public init() {}

    public struct ModelDownload: Codable, Equatable, Sendable {
        public var model: String = ""
        public var stage: String = ""
        public var percentage: Double = 0
        public var error: String?
        public init(model: String, stage: String, percentage: Double, error: String? = nil) {
            self.model = model
            self.stage = stage
            self.percentage = percentage
            self.error = error
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            model = c.lenient(String.self, .model) ?? ""
            stage = c.lenient(String.self, .stage) ?? ""
            percentage = c.lenientDouble(.percentage) ?? 0
            error = c.lenient(String.self, .error)
        }
    }

    public struct Worker: Codable, Equatable, Sendable {
        public var index: Int = 0
        public var state: String = "idle"
        public var assignedModel: String = ""
        public var modelAvailable: Bool = true
        public var loadedModel: String?
        public var completedJobs: Int = 0
        public var lastError: String?
        public var job: RunningJob?
        public init(index: Int, state: String, assignedModel: String, modelAvailable: Bool = true, loadedModel: String? = nil, completedJobs: Int = 0, lastError: String? = nil, job: RunningJob? = nil) {
            self.index = index
            self.state = state
            self.assignedModel = assignedModel
            self.modelAvailable = modelAvailable
            self.loadedModel = loadedModel
            self.completedJobs = completedJobs
            self.lastError = lastError
            self.job = job
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            index = c.lenientInt(.index) ?? 0
            state = c.lenient(String.self, .state) ?? "idle"
            assignedModel = c.lenient(String.self, .assignedModel) ?? ""
            modelAvailable = c.lenient(Bool.self, .modelAvailable) ?? true
            loadedModel = c.lenient(String.self, .loadedModel)
            completedJobs = c.lenientInt(.completedJobs) ?? 0
            lastError = c.lenient(String.self, .lastError)
            job = c.lenient(RunningJob.self, .job)
        }
    }

    public struct RunningJob: Codable, Equatable, Sendable {
        /// Job id, matching `jobId` in `/v1/events` (PROTOCOL.md). Absent on older hosts.
        public var id: String?
        public var model: String = ""
        public var source: String = ""
        public var client: String?
        public var audioSeconds: Double = 0
        public var elapsedMs: Double = 0
        public init(id: String? = nil, model: String, source: String, client: String?, audioSeconds: Double, elapsedMs: Double) {
            self.id = id
            self.model = model
            self.source = source
            self.client = client
            self.audioSeconds = audioSeconds
            self.elapsedMs = elapsedMs
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = c.lenientID(.id)
            model = c.lenient(String.self, .model) ?? ""
            source = c.lenient(String.self, .source) ?? ""
            client = c.lenient(String.self, .client)
            audioSeconds = c.lenientDouble(.audioSeconds) ?? 0
            elapsedMs = c.lenientDouble(.elapsedMs) ?? 0
        }
    }

    public struct QueuedJob: Codable, Equatable, Sendable {
        public var id: String = ""
        public var model: String = ""
        public var source: String = ""
        public var client: String?
        public var audioSeconds: Double = 0
        public var waitingMs: Double = 0
        public init(id: String, model: String, source: String, client: String?, audioSeconds: Double, waitingMs: Double) {
            self.id = id
            self.model = model
            self.source = source
            self.client = client
            self.audioSeconds = audioSeconds
            self.waitingMs = waitingMs
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = c.lenientID(.id) ?? ""
            model = c.lenient(String.self, .model) ?? ""
            source = c.lenient(String.self, .source) ?? ""
            client = c.lenient(String.self, .client)
            audioSeconds = c.lenientDouble(.audioSeconds) ?? 0
            waitingMs = c.lenientDouble(.waitingMs) ?? 0
        }
    }

    public struct Stream: Codable, Equatable, Sendable {
        /// Stream id, matching `streamId` in `/v1/events` (PROTOCOL.md). Absent on older hosts.
        public var id: String?
        public var client: String?
        public var elapsedMs: Double = 0
        public init(id: String? = nil, client: String?, elapsedMs: Double) {
            self.id = id
            self.client = client
            self.elapsedMs = elapsedMs
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = c.lenientID(.id)
            client = c.lenient(String.self, .client)
            elapsedMs = c.lenientDouble(.elapsedMs) ?? 0
        }
    }

    public struct Client: Codable, Equatable, Sendable {
        public var address: String = ""
        public var requests: Int = 0
        public var completed: Int = 0
        public var rejected: Int = 0
        public var failed: Int = 0
        public var totalAudioSeconds: Double = 0
        public var lastSeenMs: Double = 0
        public var lastModel: String?
        public init(address: String, requests: Int = 0, completed: Int = 0, rejected: Int = 0, failed: Int = 0, totalAudioSeconds: Double = 0, lastSeenMs: Double = 0, lastModel: String? = nil) {
            self.address = address
            self.requests = requests
            self.completed = completed
            self.rejected = rejected
            self.failed = failed
            self.totalAudioSeconds = totalAudioSeconds
            self.lastSeenMs = lastSeenMs
            self.lastModel = lastModel
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            address = c.lenient(String.self, .address) ?? ""
            requests = c.lenientInt(.requests) ?? 0
            completed = c.lenientInt(.completed) ?? 0
            rejected = c.lenientInt(.rejected) ?? 0
            failed = c.lenientInt(.failed) ?? 0
            totalAudioSeconds = c.lenientDouble(.totalAudioSeconds) ?? 0
            lastSeenMs = c.lenientDouble(.lastSeenMs) ?? 0
            lastModel = c.lenient(String.self, .lastModel)
        }
    }

    public struct Record: Codable, Equatable, Sendable {
        public var id: String = ""
        public var completedAtMs: Double = 0
        public var durationSeconds: Double = 0
        public var backend: String = ""
        public var model: String = ""
        public var source: String = ""
        public var client: String?
        public var queueWaitMs: Double = 0
        public var processingMs: Double = 0
        public init(id: String, completedAtMs: Double, durationSeconds: Double, backend: String = "parakeet", model: String, source: String, client: String?, queueWaitMs: Double, processingMs: Double) {
            self.id = id
            self.completedAtMs = completedAtMs
            self.durationSeconds = durationSeconds
            self.backend = backend
            self.model = model
            self.source = source
            self.client = client
            self.queueWaitMs = queueWaitMs
            self.processingMs = processingMs
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = c.lenientID(.id) ?? ""
            completedAtMs = c.lenientDouble(.completedAtMs) ?? 0
            durationSeconds = c.lenientDouble(.durationSeconds) ?? 0
            backend = c.lenient(String.self, .backend) ?? ""
            model = c.lenient(String.self, .model) ?? ""
            source = c.lenient(String.self, .source) ?? ""
            client = c.lenient(String.self, .client)
            queueWaitMs = c.lenientDouble(.queueWaitMs) ?? 0
            processingMs = c.lenientDouble(.processingMs) ?? 0
        }
    }

    /// Optional catalogue of models the host knows about (contract addition).
    public struct Model: Codable, Equatable, Sendable {
        public var id: String = ""
        public var name: String = ""
        public var publisher: String = ""
        public var sizeBytes: Double = 0
        public var installed: Bool = false
        /// Worker indexes serving this model. Accepts an array of indexes or a bare count.
        public var assignedWorkers: [Int] = []
        public init(id: String, name: String, publisher: String, sizeBytes: Double, installed: Bool, assignedWorkers: [Int]) {
            self.id = id
            self.name = name
            self.publisher = publisher
            self.sizeBytes = sizeBytes
            self.installed = installed
            self.assignedWorkers = assignedWorkers
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = c.lenient(String.self, .id) ?? ""
            name = c.lenient(String.self, .name) ?? id
            publisher = c.lenient(String.self, .publisher) ?? ""
            sizeBytes = c.lenientDouble(.sizeBytes) ?? 0
            installed = c.lenient(Bool.self, .installed) ?? false
            if let list = c.lenient([Int].self, .assignedWorkers) {
                assignedWorkers = list
            } else if let count = c.lenientInt(.assignedWorkers) {
                assignedWorkers = Array(0..<max(0, count))
            }
        }
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        serverVersion = c.lenient(String.self, .serverVersion) ?? ""
        bindAddr = c.lenient(String.self, .bindAddr) ?? ""
        uptimeSeconds = c.lenientDouble(.uptimeSeconds) ?? 0
        activeSessions = c.lenientInt(.activeSessions) ?? 0
        activeStreams = c.lenientInt(.activeStreams) ?? 0
        queuedJobs = c.lenientInt(.queuedJobs) ?? 0
        runningJobs = c.lenientInt(.runningJobs) ?? 0
        workerCount = c.lenientInt(.workerCount) ?? 0
        queueCapacity = c.lenientInt(.queueCapacity) ?? 0
        maxActiveStreams = c.lenientInt(.maxActiveStreams) ?? 0
        maxRecordingSeconds = c.lenientInt(.maxRecordingSeconds) ?? 0
        useGpu = c.lenient(Bool.self, .useGpu) ?? false
        model = c.lenient(String.self, .model) ?? ""
        modelDownload = c.lenient(ModelDownload.self, .modelDownload)
        rejectedJobs = c.lenientInt(.rejectedJobs) ?? 0
        failedJobs = c.lenientInt(.failedJobs) ?? 0
        totalTranscriptions = c.lenientInt(.totalTranscriptions) ?? 0
        totalAudioSeconds = c.lenientDouble(.totalAudioSeconds) ?? 0
        averageQueueMs = c.lenientDouble(.averageQueueMs) ?? 0
        averageProcessingMs = c.lenientDouble(.averageProcessingMs) ?? 0
        workers = c.lenient([Worker].self, .workers) ?? []
        queue = c.lenient([QueuedJob].self, .queue) ?? []
        streams = c.lenient([Stream].self, .streams) ?? []
        clients = c.lenient([Client].self, .clients) ?? []
        recent = c.lenient([Record].self, .recent) ?? []
        models = c.lenient([Model].self, .models)
    }
}

// MARK: - Lenient decoding helpers

extension KeyedDecodingContainer {
    func lenient<T: Decodable>(_ type: T.Type, _ key: Key) -> T? {
        (try? decodeIfPresent(type, forKey: key)) ?? nil
    }

    func lenientDouble(_ key: Key) -> Double? {
        if let d = lenient(Double.self, key) { return d }
        if let s = lenient(String.self, key) { return Double(s) }
        return nil
    }

    func lenientInt(_ key: Key) -> Int? {
        if let i = lenient(Int.self, key) { return i }
        if let d = lenient(Double.self, key), d.isFinite { return Int(d) }
        if let s = lenient(String.self, key) { return Int(s) }
        return nil
    }

    /// IDs arrive as numbers from the Rust host; accept strings too.
    func lenientID(_ key: Key) -> String? {
        if let s = lenient(String.self, key) { return s }
        if let i = lenient(Int64.self, key) { return String(i) }
        if let u = lenient(UInt64.self, key) { return String(u) }
        if let d = lenient(Double.self, key), d.isFinite { return String(Int64(d)) }
        return nil
    }
}
