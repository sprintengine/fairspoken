import Foundation

/// Typed `GET /v1/events` messages (contract agreed with the Rust host). Fields are
/// camelCase, `at` is epoch milliseconds, no transcript text ever appears.
public enum HostEvent: Equatable, Sendable {
    case snapshot(HostStats)
    case streamStarted(StreamStarted)
    case streamFinished(StreamFinished)
    case jobQueued(JobQueued)
    case jobStarted(JobStarted)
    case jobCompleted(JobCompleted)
    case jobFailed(JobFailed)
    case workerState(WorkerState)
    case modelDownload(ModelDownload)
    case unknown(type: String)

    public struct StreamStarted: Codable, Equatable, Sendable {
        public var streamId: String
        public var client: String?
        public var at: Double
        public init(streamId: String, client: String?, at: Double) {
            self.streamId = streamId; self.client = client; self.at = at
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            streamId = c.lenientID(.streamId) ?? ""
            client = c.lenient(String.self, .client)
            at = c.lenientDouble(.at) ?? 0
        }
    }

    public struct StreamFinished: Codable, Equatable, Sendable {
        public var streamId: String
        public var client: String?
        public var audioSeconds: Double
        public var at: Double
        public init(streamId: String, client: String?, audioSeconds: Double, at: Double) {
            self.streamId = streamId; self.client = client; self.audioSeconds = audioSeconds; self.at = at
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            streamId = c.lenientID(.streamId) ?? ""
            client = c.lenient(String.self, .client)
            audioSeconds = c.lenientDouble(.audioSeconds) ?? 0
            at = c.lenientDouble(.at) ?? 0
        }
    }

    public struct JobQueued: Codable, Equatable, Sendable {
        public var jobId: String
        public var client: String?
        public var source: String
        public var model: String
        public var audioSeconds: Double
        public var at: Double
        public init(jobId: String, client: String?, source: String, model: String, audioSeconds: Double, at: Double) {
            self.jobId = jobId; self.client = client; self.source = source; self.model = model
            self.audioSeconds = audioSeconds; self.at = at
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            jobId = c.lenientID(.jobId) ?? ""
            client = c.lenient(String.self, .client)
            source = c.lenient(String.self, .source) ?? ""
            model = c.lenient(String.self, .model) ?? ""
            audioSeconds = c.lenientDouble(.audioSeconds) ?? 0
            at = c.lenientDouble(.at) ?? 0
        }
    }

    public struct JobStarted: Codable, Equatable, Sendable {
        public var jobId: String
        public var worker: Int
        public var model: String
        public var client: String?
        public var queueWaitMs: Double
        public var at: Double
        public init(jobId: String, worker: Int, model: String, client: String?, queueWaitMs: Double, at: Double) {
            self.jobId = jobId; self.worker = worker; self.model = model; self.client = client
            self.queueWaitMs = queueWaitMs; self.at = at
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            jobId = c.lenientID(.jobId) ?? ""
            worker = c.lenientInt(.worker) ?? 0
            model = c.lenient(String.self, .model) ?? ""
            client = c.lenient(String.self, .client)
            queueWaitMs = c.lenientDouble(.queueWaitMs) ?? 0
            at = c.lenientDouble(.at) ?? 0
        }
    }

    public struct JobCompleted: Codable, Equatable, Sendable {
        public var jobId: String
        public var worker: Int
        public var model: String
        public var client: String?
        public var audioSeconds: Double
        public var processingMs: Double
        public var at: Double
        public init(jobId: String, worker: Int, model: String, client: String?, audioSeconds: Double, processingMs: Double, at: Double) {
            self.jobId = jobId; self.worker = worker; self.model = model; self.client = client
            self.audioSeconds = audioSeconds; self.processingMs = processingMs; self.at = at
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            jobId = c.lenientID(.jobId) ?? ""
            worker = c.lenientInt(.worker) ?? 0
            model = c.lenient(String.self, .model) ?? ""
            client = c.lenient(String.self, .client)
            audioSeconds = c.lenientDouble(.audioSeconds) ?? 0
            processingMs = c.lenientDouble(.processingMs) ?? 0
            at = c.lenientDouble(.at) ?? 0
        }
    }

    public struct JobFailed: Codable, Equatable, Sendable {
        public var jobId: String
        public var worker: Int?
        public var client: String?
        public var error: String
        public var at: Double
        public init(jobId: String, worker: Int?, client: String?, error: String, at: Double) {
            self.jobId = jobId; self.worker = worker; self.client = client; self.error = error; self.at = at
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            jobId = c.lenientID(.jobId) ?? ""
            worker = c.lenientInt(.worker)
            client = c.lenient(String.self, .client)
            error = c.lenient(String.self, .error) ?? "failed"
            at = c.lenientDouble(.at) ?? 0
        }
    }

    public struct WorkerState: Codable, Equatable, Sendable {
        public var worker: Int
        public var state: String
        public var model: String?
        public var at: Double
        public init(worker: Int, state: String, model: String?, at: Double) {
            self.worker = worker; self.state = state; self.model = model; self.at = at
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            worker = c.lenientInt(.worker) ?? 0
            state = c.lenient(String.self, .state) ?? "idle"
            model = c.lenient(String.self, .model)
            at = c.lenientDouble(.at) ?? 0
        }
    }

    public struct ModelDownload: Codable, Equatable, Sendable {
        public var model: String
        public var stage: String
        public var percentage: Double
        /// Present only when `stage == "error"`.
        public var error: String?
        public var at: Double
        public init(model: String, stage: String, percentage: Double, error: String? = nil, at: Double) {
            self.model = model; self.stage = stage; self.percentage = percentage; self.error = error; self.at = at
        }
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            model = c.lenient(String.self, .model) ?? ""
            stage = c.lenient(String.self, .stage) ?? ""
            percentage = c.lenientDouble(.percentage) ?? 0
            error = c.lenient(String.self, .error)
            at = c.lenientDouble(.at) ?? 0
        }
    }

    public enum DecodeError: Error, Equatable {
        case invalidJSON(type: String)
    }

    /// Decodes one SSE event. Unknown event types become `.unknown` (forward compatible);
    /// a known type with undecodable JSON throws.
    public static func decode(_ event: SSEEvent) throws -> HostEvent {
        let data = Data(event.data.utf8)
        let decoder = JSONDecoder()
        func decode<T: Decodable>(_ type: T.Type) throws -> T {
            do { return try decoder.decode(type, from: data) } catch { throw DecodeError.invalidJSON(type: event.type) }
        }
        switch event.type {
        case "snapshot": return .snapshot(try decode(HostStats.self))
        case "stream_started": return .streamStarted(try decode(StreamStarted.self))
        case "stream_finished": return .streamFinished(try decode(StreamFinished.self))
        case "job_queued": return .jobQueued(try decode(JobQueued.self))
        case "job_started": return .jobStarted(try decode(JobStarted.self))
        case "job_completed": return .jobCompleted(try decode(JobCompleted.self))
        case "job_failed": return .jobFailed(try decode(JobFailed.self))
        case "worker_state": return .workerState(try decode(WorkerState.self))
        case "model_download": return .modelDownload(try decode(ModelDownload.self))
        default: return .unknown(type: event.type)
        }
    }

    /// Wire name, used by the simulator's SSE encoder and in logs.
    public var typeName: String {
        switch self {
        case .snapshot: "snapshot"
        case .streamStarted: "stream_started"
        case .streamFinished: "stream_finished"
        case .jobQueued: "job_queued"
        case .jobStarted: "job_started"
        case .jobCompleted: "job_completed"
        case .jobFailed: "job_failed"
        case .workerState: "worker_state"
        case .modelDownload: "model_download"
        case .unknown(let type): type
        }
    }
}
