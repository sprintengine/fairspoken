@preconcurrency import CoreML
import FluidAudio
import Foundation
import FairspokenCore
import OSLog
import Synchronization

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

    /// Downloads (if needed) without loading, from the current `source`. From Hugging Face or a
    /// mirror, FluidAudio compiles the models for this Mac as the last step, reported as
    /// `.compiling`; a folder install is a copy (compiled on first load).
    public static func download(_ id: String, progress: @escaping @Sendable (Stage) -> Void) async throws {
        guard let v = version(for: id), let files = sourceFiles(for: id) else { throw SpeechEngineError.unknownModel(id) }
        if isInstalled(id) { return }
        if let problem = currentProblem.withLock({ $0 }) { throw problem }
        let source = self.source
        if case .folder = source {
            try await installFromFolder(source, version: v, files: files, progress: progress)
            return
        }
        do {
            try await AsrModels.download(version: v) { p in
                switch p.phase {
                case .compiling: progress(.compiling)
                default: progress(.downloading(p.fractionCompleted))
                }
            }
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            throw ModelSourceError.downloadFailed(location: source.location(of: files.repo), reason: ModelSourceProbe.describe(error))
        }
    }

    // MARK: Source

    /// The FluidAudio repository of a model and the files `AsrModels.modelsExist` checks for.
    public struct SourceFiles: Sendable, Equatable {
        /// `FluidInference/parakeet-tdt-0.6b-v3-coreml`
        public var repo: String
        public var revision: String
        public var required: [String]
    }

    public static func sourceFiles(for id: String) -> SourceFiles? {
        guard let v = version(for: id) else { return nil }
        let names = ModelNames.ASR.self
        let repo: Repo
        let models: Set<String>
        switch v {
        case .v3: repo = .parakeetV3; models = names.requiredModelsV3(precision: .int8)
        case .ultra: repo = .parakeetUltra; models = names.requiredModelsV3()
        case .v2: repo = .parakeetV2; models = names.requiredModels
        default: return nil
        }
        return SourceFiles(repo: repo.remotePath, revision: repo.revision, required: models.sorted() + [names.vocabularyFile])
    }

    private static let currentSource = Mutex<ModelSource>(.huggingFace)
    /// Why the configured text isn't a source (a hand-edited file); downloads fail with it.
    private static let currentProblem = Mutex<ModelSourceError?>(nil)

    /// Where downloads come from. Set it before the first download and whenever the setting
    /// changes; a download in progress keeps the source it started with.
    public static var source: ModelSource { currentSource.withLock { $0 } }

    /// Points FluidAudio's registry at Hugging Face or the mirror (for a folder, downloads
    /// don't use it).
    public static func setSource(_ source: ModelSource) {
        currentSource.withLock { $0 = source }
        if case .mirror(let url) = source {
            ModelRegistry.baseURL = url.absoluteString
        } else {
            // FluidAudio's own default, which honours REGISTRY_URL / MODEL_REGISTRY_URL.
            let env = ProcessInfo.processInfo.environment
            ModelRegistry.baseURL = env["REGISTRY_URL"] ?? env["MODEL_REGISTRY_URL"] ?? ModelSource.huggingFaceURL.absoluteString
        }
        speechLog.info("Model source: \(source.displayName, privacy: .public)")
    }

    /// The settings value. An invalid one (settings keep it so the error shows) makes downloads
    /// fail with the reason rather than quietly use Hugging Face.
    public static func setSource(text: String) {
        do {
            setSource(try ModelSource.parse(text))
            currentProblem.withLock { $0 = nil }
        } catch {
            setSource(.huggingFace)
            currentProblem.withLock { $0 = error }
            speechLog.error("Model source: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// The Test button: can `text` serve each of `ids`? Nothing is downloaded.
    public static func check(_ text: String, models ids: [String]) async -> ModelSourceCheck {
        let source: ModelSource
        do { source = try ModelSource.parse(text) } catch { return ModelSourceCheck(ok: false, message: error.localizedDescription) }
        var found: [String] = []
        for id in ids {
            guard let files = sourceFiles(for: id) else { continue }
            let result = await ModelSourceProbe.check(source, repo: files.repo, revision: files.revision, requiredFiles: files.required)
            guard result.ok else { return result }
            found.append(result.message)
        }
        return ModelSourceCheck(ok: true, message: found.joined(separator: " "))
    }

    /// Copies `{folder}/{repo}` into FluidAudio's cache off the calling thread, reporting the copy
    /// as `.downloading`, and keeps it only if FluidAudio finds every file it loads.
    private static func installFromFolder(_ source: ModelSource, version v: AsrModelVersion, files: SourceFiles,
                                          progress: @escaping @Sendable (Stage) -> Void) async throws {
        guard let from = source.repoDirectory(files.repo) else { throw ModelSourceError.folderMissing(path: source.displayName) }
        let destination = AsrModels.defaultCacheDirectory(for: v)
        progress(.downloading(0))
        try await Task.detached(priority: .userInitiated) {
            try ModelFolderInstaller.install(from: from, to: destination, required: files.required, progress: { progress(.downloading($0)) }) { dir in
                AsrModels.modelsExist(at: dir, version: v) ? [] : ModelFolderInstaller.missingFiles(in: dir, required: files.required)
            }
        }.value
        speechLog.info("Installed \(files.repo, privacy: .public) from \(from.path(percentEncoded: false), privacy: .public)")
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
