import FairspokenSpeech
import Foundation
import FairspokenCore
import Observation
import OSLog

/// State of the local speech engine and the model gallery.
@Observable
final class ModelLibrary {
    enum EngineState: Equatable {
        case idle
        case downloading(Double)
        case compiling
        case loading
        case warming
        case ready
        case failed(String)

        var isReady: Bool { self == .ready }
        var isBusy: Bool {
            switch self {
            case .downloading, .compiling, .loading, .warming: true
            default: false
            }
        }

        var label: String {
            switch self {
            case .idle: "Not loaded"
            case .downloading(let f): "Downloading \(Int(f * 100))%"
            case .compiling: "Optimising for this Mac…"
            case .loading: "Loading onto the Neural Engine…"
            case .warming: "Warming up…"
            case .ready: "Ready"
            case .failed(let m): m
            }
        }
    }

    enum ItemState: Equatable {
        case available
        case downloading(Double)
        case compiling
        case installed
    }

    private(set) var engineState: EngineState = .idle
    private(set) var activeModelID: String = SpeechModelCatalog.defaultModelID
    private(set) var items: [String: ItemState] = [:]
    private(set) var diskBytes: [String: Int64] = [:]
    private(set) var lastLoadSeconds: Double?

    let engine = FluidAudioEngine()
    @ObservationIgnored private var loadTask: Task<Void, Never>?
    private static let log = Logger(subsystem: AppInfo.bundleID, category: "models")

    var activeModel: SpeechModelInfo {
        SpeechModelCatalog.model(id: activeModelID) ?? SpeechModelCatalog.all[0]
    }

    init() {
        refresh()
    }

    func state(of id: String) -> ItemState { items[id] ?? .available }

    func refresh() {
        for model in SpeechModelCatalog.all {
            if case .downloading = items[model.id] { continue }
            if items[model.id] == .compiling { continue }
            items[model.id] = FluidAudioEngine.isInstalled(model.id) ? .installed : .available
        }
        Task.detached(priority: .utility) {
            var sizes: [String: Int64] = [:]
            for model in SpeechModelCatalog.all {
                if let dir = FluidAudioEngine.directory(for: model.id), FileManager.default.fileExists(atPath: dir.path) {
                    sizes[model.id] = Self.directorySize(dir)
                }
            }
            await MainActor.run { self.diskBytes = sizes }
        }
    }

    /// Download if needed, load onto the ANE and warm up. Safe to call repeatedly.
    func prepare(_ id: String) {
        guard !(activeModelID == id && (engineState.isReady || engineState.isBusy)) else { return }
        activeModelID = id
        loadTask?.cancel()
        engineState = .loading
        let started = Date()
        let report: @Sendable (FluidAudioEngine.Stage) -> Void = { [weak self] stage in
            Task { @MainActor in self?.apply(stage, for: id) }
        }
        loadTask = Task { [weak self, engine] in
            do {
                try await engine.load(id, progress: report)
                await MainActor.run {
                    guard let self, self.activeModelID == id else { return }
                    self.engineState = .ready
                    self.lastLoadSeconds = Date().timeIntervalSince(started)
                    self.refresh()
                }
            } catch {
                await MainActor.run {
                    guard let self, self.activeModelID == id else { return }
                    Self.log.error("Model load failed: \(error.localizedDescription, privacy: .public)")
                    self.engineState = .failed("Couldn't load the model: \(error.localizedDescription)")
                    self.refresh()
                }
            }
        }
    }

    private func apply(_ stage: FluidAudioEngine.Stage, for id: String) {
        guard activeModelID == id, engineState != .ready else { return }
        switch stage {
        case .downloading(let f):
            engineState = .downloading(f)
            items[id] = .downloading(f)
        case .compiling:
            engineState = .compiling
            items[id] = .compiling
        case .loading:
            engineState = .loading
            items[id] = .installed
        case .warming:
            engineState = .warming
        }
    }

    /// Gallery download (not activated).
    func download(_ id: String) {
        guard state(of: id) == .available else { return }
        items[id] = .downloading(0)
        let report: @Sendable (FluidAudioEngine.Stage) -> Void = { [weak self] stage in
            Task { @MainActor in
                switch stage {
                case .downloading(let f): self?.items[id] = .downloading(f)
                case .compiling: self?.items[id] = .compiling
                default: break
                }
            }
        }
        Task { [weak self] in
            do {
                try await FluidAudioEngine.download(id, progress: report)
            } catch {
                Self.log.error("Download failed: \(error.localizedDescription, privacy: .public)")
            }
            await MainActor.run { [weak self] in
                self?.items[id] = nil
                self?.refresh()
            }
        }
    }

    /// Removes a model's files. The active model is never deleted.
    func delete(_ id: String) {
        guard id != activeModelID, let dir = FluidAudioEngine.directory(for: id) else { return }
        // Only ever delete inside FluidAudio's model cache.
        guard dir.path.contains("/FluidAudio/Models/") else { return }
        try? FileManager.default.removeItem(at: dir)
        items[id] = .available
        diskBytes[id] = nil
        refresh()
    }

    nonisolated private static func directorySize(_ url: URL) -> Int64 {
        guard let e = FileManager.default.enumerator(at: url, includingPropertiesForKeys: [.totalFileAllocatedSizeKey]) else { return 0 }
        var total: Int64 = 0
        for case let file as URL in e {
            total += Int64((try? file.resourceValues(forKeys: [.totalFileAllocatedSizeKey]).totalFileAllocatedSize) ?? 0)
        }
        return total
    }
}
