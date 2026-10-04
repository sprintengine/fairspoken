import Foundation

/// A model the host can serve, as listed in `/v1/stats` `models[]`.
public struct HostModelDescriptor: Sendable, Equatable, Identifiable {
    public var id: String
    public var name: String
    public var publisher: String
    /// Download size, reported until the model is installed.
    public var approxBytes: Int64

    public init(id: String, name: String, publisher: String, approxBytes: Int64) {
        self.id = id
        self.name = name
        self.publisher = publisher
        self.approxBytes = approxBytes
    }
}

/// Stage of a model download, the `model_download.stage` values.
public enum ModelDownloadStage: String, Sendable {
    case starting, downloading, validating, ready, error
}

/// The speech engine behind the host. Fairspoken Server plugs in FluidAudio (Parakeet on the
/// Neural Engine); tests use `StubSpeechBackend`. Every worker owns a decoder for its model,
/// and workers serving the same model share one loaded copy of its weights.
public protocol HostSpeechBackend: Sendable {
    var catalog: [HostModelDescriptor] { get }
    func isInstalled(_ model: String) -> Bool
    /// On-disk size of an installed model (cached; refreshed by `refreshSizes`).
    func installedBytes(_ model: String) -> Int64?
    /// Changes when the installed files change, so a failed load is retried only then.
    func installStamp(_ model: String) -> Date?
    /// Loads `model` for `worker` (sharing weights with other workers) and warms it up.
    func prepare(worker: Int, model: String, accelerated: Bool) async throws
    /// Frees whatever `worker` holds.
    func release(worker: Int) async
    /// Transcribes 16 kHz mono samples on `worker`'s loaded model.
    func transcribe(worker: Int, samples16k: [Float], language: String) async throws -> String
    /// Downloads (and validates) a model, reporting progress 0–100.
    func download(_ model: String, progress: @escaping @Sendable (ModelDownloadStage, Int) -> Void) async throws
    func delete(_ model: String) throws
}

public enum HostBackendError: Error, LocalizedError, Sendable {
    case notInstalled(String)
    case notLoaded(String)
    case unknownModel(String)

    public var errorDescription: String? {
        switch self {
        case .notInstalled(let id): "Model \(id) is not installed on this host"
        case .notLoaded(let id): "Model \(id) is not loaded"
        case .unknownModel(let id): "Unsupported model: \(id)"
        }
    }
}

/// Deterministic backend for tests and for the conformance suite without a Neural Engine:
/// "transcribes" by reporting the audio length, after a configurable delay.
public final class StubSpeechBackend: HostSpeechBackend, @unchecked Sendable {
    public let catalog: [HostModelDescriptor]
    private let lock = NSLock()
    private var installed: Set<String>
    private var loaded: [Int: String] = [:]
    public var transcribeDelay: Duration
    public var prepareDelay: Duration
    public var downloadStepDelay: Duration
    public var failNextTranscription = false
    public private(set) var transcribeCalls: [(worker: Int, samples: Int)] = []

    public init(installed: Set<String> = ["parakeet-tdt-0.6b-v3", "parakeet-ultra"],
                transcribeDelay: Duration = .milliseconds(20), prepareDelay: Duration = .zero,
                downloadStepDelay: Duration = .milliseconds(5)) {
        catalog = [
            HostModelDescriptor(id: "parakeet-tdt-0.6b-v3", name: "Parakeet TDT 0.6B v3", publisher: "NVIDIA", approxBytes: 470_000_000),
            HostModelDescriptor(id: "parakeet-ultra", name: "Parakeet Ultra", publisher: "Moondream", approxBytes: 613_000_000),
        ]
        self.installed = installed
        self.transcribeDelay = transcribeDelay
        self.prepareDelay = prepareDelay
        self.downloadStepDelay = downloadStepDelay
    }

    public func isInstalled(_ model: String) -> Bool { lock.withLock { installed.contains(model) } }
    public func installedBytes(_ model: String) -> Int64? { isInstalled(model) ? 480_000_000 : nil }
    public func installStamp(_ model: String) -> Date? { nil }

    public func prepare(worker: Int, model: String, accelerated: Bool) async throws {
        guard isInstalled(model) else { throw HostBackendError.notInstalled(model) }
        if prepareDelay > .zero { try await Task.sleep(for: prepareDelay) }
        lock.withLock { loaded[worker] = model }
    }

    public func release(worker: Int) async { _ = lock.withLock { loaded.removeValue(forKey: worker) } }

    public func transcribe(worker: Int, samples16k: [Float], language: String) async throws -> String {
        let model = lock.withLock { loaded[worker] }
        guard model != nil else { throw HostBackendError.notLoaded("worker \(worker)") }
        lock.withLock { transcribeCalls.append((worker, samples16k.count)) }
        if transcribeDelay > .zero { try await Task.sleep(for: transcribeDelay) }
        let fail = lock.withLock { () -> Bool in defer { failNextTranscription = false }; return failNextTranscription }
        if fail { throw NSError(domain: "stub", code: 1, userInfo: [NSLocalizedDescriptionKey: "Stub transcription failed"]) }
        return String(format: "Stub transcript of %.2f seconds.", Double(samples16k.count) / 16_000)
    }

    public func download(_ model: String, progress: @escaping @Sendable (ModelDownloadStage, Int) -> Void) async throws {
        guard catalog.contains(where: { $0.id == model }) else { throw HostBackendError.unknownModel(model) }
        for pct in stride(from: 10, through: 90, by: 20) {
            try await Task.sleep(for: downloadStepDelay)
            progress(.downloading, pct)
        }
        progress(.validating, 96)
        lock.withLock { _ = installed.insert(model) }
    }

    public func delete(_ model: String) throws { lock.withLock { _ = installed.remove(model) } }

    public func setInstalled(_ model: String, _ value: Bool) {
        lock.withLock { if value { installed.insert(model) } else { installed.remove(model) } }
    }
}
