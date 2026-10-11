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
        case unpacking
        case compiling
        case loading
        case warming
        case ready
        case failed(String)

        var isReady: Bool { self == .ready }
        var isBusy: Bool {
            switch self {
            case .downloading, .unpacking, .compiling, .loading, .warming: true
            default: false
            }
        }

        var label: String {
            switch self {
            case .idle: "Not loaded"
            case .downloading(let f): "Downloading \(Int(f * 100))%"
            case .unpacking: "Unpacking…"
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
        case unpacking
        case compiling
        case installed
    }

    private(set) var engineState: EngineState = .idle
    private(set) var activeModelID: String = SpeechModelCatalog.defaultModelID
    private(set) var items: [String: ItemState] = [:]
    private(set) var diskBytes: [String: Int64] = [:]
    private(set) var lastLoadSeconds: Double?
    /// Why the last download of a model failed (says what and where), until the next try.
    private(set) var downloadErrors: [String: String] = [:]

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
            if items[model.id] == .compiling || items[model.id] == .unpacking { continue }
            items[model.id] = FluidAudioEngine.isInstalled(model.id) && !presentedAsAvailable.contains(model.id) ? .installed : .available
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
        downloadErrors[id] = nil
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
                    // A model that never arrived shows why on its card too.
                    if !FluidAudioEngine.isInstalled(id) { self.downloadErrors[id] = error.localizedDescription }
                    if self.items[id] != .installed { self.items[id] = nil }
                    self.refresh()
                }
            }
        }
    }

    private func apply(_ stage: FluidAudioEngine.Stage, for id: String) {
        guard activeModelID == id, engineState != .ready else { return }
        // A late report after the load failed would leave the card looking busy.
        if case .failed = engineState { return }
        switch stage {
        case .downloading(let f):
            engineState = .downloading(f)
            items[id] = .downloading(f)
        case .unpacking:
            engineState = .unpacking
            items[id] = .unpacking
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
        downloadErrors[id] = nil
        let report: @Sendable (FluidAudioEngine.Stage) -> Void = { [weak self] stage in
            Task { @MainActor in
                switch stage {
                case .downloading(let f): self?.items[id] = .downloading(f)
                case .unpacking: self?.items[id] = .unpacking
                case .compiling: self?.items[id] = .compiling
                default: break
                }
            }
        }
        Task { [weak self] in
            var failure: String?
            do {
                try await FluidAudioEngine.download(id, progress: report)
            } catch is CancellationError {
            } catch {
                Self.log.error("Download failed: \(error.localizedDescription, privacy: .public)")
                failure = error.localizedDescription
            }
            await MainActor.run { [weak self] in
                self?.items[id] = nil
                self?.downloadErrors[id] = failure
                self?.refresh()
            }
        }
    }

    func clearDownloadError(_ id: String) { downloadErrors[id] = nil }

    /// Screenshot mode: shows a model as not downloaded, whatever is on disk.
    @ObservationIgnored var presentedAsAvailable: Set<String> = []

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
