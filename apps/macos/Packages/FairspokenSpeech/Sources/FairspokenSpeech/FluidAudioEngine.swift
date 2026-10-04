@preconcurrency import CoreML
import FluidAudio
import Foundation
import MultiVoiceCore
import OSLog

let speechLog = Logger(subsystem: "ie.fairspoken.speech", category: "engine")

/// The Parakeet models FluidAudio can run, where they live and how they are installed.
/// Models live in FluidAudio's fixed cache (`~/Library/Application Support/FluidAudio/Models/<repo>`),
/// shared by every app on the Mac. The cache is fixed on purpose: the Neural Engine's compiled
/// cache is keyed by model path and OS build, so moving a model costs a 13–15 s recompile.
public enum ParakeetModels {
    public enum Stage: Sendable, Equatable {
        case downloading(Double)
        case compiling
        case loading
        case warming
    }

    static func version(for id: String) -> AsrModelVersion? {
        switch id {
        case "parakeet-tdt-0.6b-v3": .v3
        case "parakeet-ultra": .ultra
        case "parakeet-tdt-0.6b-v2": .v2
        default: nil
        }
    }

    public static func isSupported(_ id: String) -> Bool { version(for: id) != nil }

    public static func directory(for id: String) -> URL? {
        version(for: id).map { AsrModels.defaultCacheDirectory(for: $0) }
    }

    public static func isInstalled(_ id: String) -> Bool {
        guard let v = version(for: id) else { return false }
        return AsrModels.modelsExist(at: AsrModels.defaultCacheDirectory(for: v), version: v)
    }

    /// Newest modification time of the model folder, so a failed load is retried only after
    /// the files change.
    public static func installStamp(_ id: String) -> Date? {
        guard let dir = directory(for: id) else { return nil }
        return (try? dir.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
    }

    /// Downloads (if needed) without loading. FluidAudio compiles the models for this Mac as
    /// the last step, reported as `.compiling`.
    public static func download(_ id: String, progress: @escaping @Sendable (Stage) -> Void) async throws {
        guard let v = version(for: id) else { throw SpeechEngineError.unknownModel(id) }
        if isInstalled(id) { return }
        try await AsrModels.download(version: v) { p in
            switch p.phase {
            case .compiling: progress(.compiling)
            default: progress(.downloading(p.fractionCompleted))
            }
        }
    }

    /// Removes a model's files. Only ever deletes inside FluidAudio's model cache.
    public static func delete(_ id: String) throws {
        guard let dir = directory(for: id), dir.path.contains("/FluidAudio/Models/") else { throw SpeechEngineError.unknownModel(id) }
        if FileManager.default.fileExists(atPath: dir.path) { try FileManager.default.removeItem(at: dir) }
    }

    /// Allocated size of an installed model folder.
    public static func installedBytes(_ id: String) -> Int64? {
        guard let dir = directory(for: id), FileManager.default.fileExists(atPath: dir.path) else { return nil }
        guard let e = FileManager.default.enumerator(at: dir, includingPropertiesForKeys: [.totalFileAllocatedSizeKey]) else { return nil }
        var total: Int64 = 0
        for case let file as URL in e {
            total += Int64((try? file.resourceValues(forKeys: [.totalFileAllocatedSizeKey]).totalFileAllocatedSize) ?? 0)
        }
        return total
    }

    static func managerConfig(_ v: AsrModelVersion) -> ASRConfig {
        ASRConfig(tdtConfig: TdtConfig(blankId: v.blankId), encoderHiddenSize: v.encoderHiddenSize)
    }

    /// One short, near-silent inference so the first real request never pays CoreML
    /// specialisation.
    static func warmUp(_ manager: AsrManager) async {
        let probe = (0..<16_000).map { i in Float(sin(Double(i) * 0.05)) * 0.0008 }
        var state = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
        _ = try? await manager.transcribe(probe, decoderState: &state)
    }
}

public enum SpeechEngineError: LocalizedError, Sendable {
    case unknownModel(String)
    case notLoaded
    case notInstalled(String)

    public var errorDescription: String? {
        switch self {
        case .unknownModel(let id): "Unknown speech model \(id)."
        case .notLoaded: "The speech model is still loading."
        case .notInstalled(let id): "Model \(id) is not installed on this host"
        }
    }
}

/// One loaded model for the dictation client. Parakeet runs on the Apple Neural Engine
/// (`.cpuAndNeuralEngine`, FluidAudio's default).
public actor FluidAudioEngine: SpeechEngine {
    public typealias Stage = ParakeetModels.Stage

    private var manager: AsrManager?
    public private(set) var loadedModelID: String?

    public init() {}

    public nonisolated static func directory(for id: String) -> URL? { ParakeetModels.directory(for: id) }
    public nonisolated static func isInstalled(_ id: String) -> Bool { ParakeetModels.isInstalled(id) }

    public static func download(_ id: String, progress: @escaping @Sendable (Stage) -> Void) async throws {
        try await ParakeetModels.download(id, progress: progress)
    }

    /// Download if missing, load onto the Neural Engine and warm up.
    public func load(_ id: String, progress: @escaping @Sendable (Stage) -> Void) async throws {
        if loadedModelID == id, manager != nil { return }
        guard let v = ParakeetModels.version(for: id) else { throw SpeechEngineError.unknownModel(id) }
        let dir = AsrModels.defaultCacheDirectory(for: v)
        if !AsrModels.modelsExist(at: dir, version: v) {
            progress(.downloading(0))
            try await ParakeetModels.download(id, progress: progress)
        }
        progress(.loading)
        let started = Date()
        let models = try await AsrModels.load(from: dir, version: v)
        let next = AsrManager(config: ParakeetModels.managerConfig(v))
        try await next.loadModels(models)
        if let old = manager { await old.cleanup() }
        manager = next
        loadedModelID = id
        speechLog.info("Loaded \(id, privacy: .public) in \(Date().timeIntervalSince(started), format: .fixed(precision: 2)) s")
        progress(.warming)
        await ParakeetModels.warmUp(next)
    }

    public func unload() async {
        if let manager { await manager.cleanup() }
        manager = nil
        loadedModelID = nil
    }

    public func transcribe(_ samples16k: [Float], options: TranscriptionRequestOptions) async throws -> TranscriptionOutput {
        guard let manager, let id = loadedModelID else { throw SpeechEngineError.notLoaded }
        let started = ContinuousClock.now
        var state = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
        let result = try await manager.transcribe(samples16k, decoderState: &state)
        let ms = Int((ContinuousClock.now - started) / .milliseconds(1))
        return TranscriptionOutput(text: result.text, modelMs: ms, model: id, placement: .neuralEngine)
    }
}

/// Parakeet decoders for a pool of server workers. Workers serving the same model share one
/// loaded copy of its weights (`AsrModels`) and each own a decoder (`AsrManager`), which the
/// FluidAudio spike measured as correct at 8 concurrent transcriptions, at ~2.1× the
/// throughput of one decoder.
public actor ParakeetWorkerPool {
    private struct Key: Hashable {
        var model: String
        var accelerated: Bool
    }

    private var shared: [Key: Task<AsrModels, Error>] = [:]
    private var workers: [Int: (key: Key, manager: AsrManager)] = [:]

    public init() {}

    /// Loads `model` for `worker` and warms its decoder. Cheap when already loaded.
    public func prepare(worker: Int, model: String, accelerated: Bool) async throws {
        let key = Key(model: model, accelerated: accelerated)
        if workers[worker]?.key == key { return }
        guard let v = ParakeetModels.version(for: model) else { throw SpeechEngineError.unknownModel(model) }
        guard ParakeetModels.isInstalled(model) else { throw SpeechEngineError.notInstalled(model) }
        let models = try await sharedModels(key, version: v)
        let manager = AsrManager(config: ParakeetModels.managerConfig(v))
        try await manager.loadModels(models)
        await ParakeetModels.warmUp(manager)
        await release(worker: worker)
        workers[worker] = (key, manager)
        speechLog.info("Worker \(worker) serving \(model, privacy: .public)\(accelerated ? "" : " on the CPU", privacy: .public)")
    }

    private func sharedModels(_ key: Key, version v: AsrModelVersion) async throws -> AsrModels {
        if let task = shared[key] {
            do { return try await task.value } catch {
                shared[key] = nil
                throw error
            }
        }
        let dir = AsrModels.defaultCacheDirectory(for: v)
        let units: MLComputeUnits? = key.accelerated ? nil : .cpuOnly
        let task = Task { try await AsrModels.load(from: dir, version: v, encoderComputeUnits: units) }
        shared[key] = task
        do { return try await task.value } catch {
            shared[key] = nil
            throw error
        }
    }

    public func release(worker: Int) async {
        guard let held = workers.removeValue(forKey: worker) else { return }
        await held.manager.cleanup()
        // Drop the shared weights once no worker uses them.
        if !workers.values.contains(where: { $0.key == held.key }) { shared[held.key] = nil }
    }

    /// Drops every cached load of `model` (after its files were deleted or replaced).
    public func forget(model: String) {
        for key in shared.keys where key.model == model && !workers.values.contains(where: { $0.key == key }) {
            shared[key] = nil
        }
    }

    public func transcribe(worker: Int, samples16k: [Float]) async throws -> String {
        guard let manager = workers[worker]?.manager else { throw SpeechEngineError.notLoaded }
        var state = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
        return try await manager.transcribe(samples16k, decoderState: &state).text
    }
}
