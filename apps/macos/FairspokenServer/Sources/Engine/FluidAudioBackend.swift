import FairspokenHost
import FairspokenSpeech
import Foundation
import MultiVoiceCore

/// The host's speech engine: Parakeet TDT v3 and Parakeet Ultra on the Apple Neural Engine
/// through FluidAudio. Workers serving the same model share its weights.
nonisolated final class FluidAudioBackend: HostSpeechBackend {
    /// Parakeet Redux is not offered (its encoder runs mostly on the CPU on M4 + macOS 26).
    static let servedModels = ["parakeet-tdt-0.6b-v3", "parakeet-ultra"]

    let pool = ParakeetWorkerPool()

    let catalog: [HostModelDescriptor] = SpeechModelCatalog.all
        .filter { FluidAudioBackend.servedModels.contains($0.id) }
        .map { HostModelDescriptor(id: $0.id, name: $0.name, publisher: $0.publisher, approxBytes: $0.approxBytes) }

    func isInstalled(_ model: String) -> Bool { ParakeetModels.isInstalled(model) }
    func installedBytes(_ model: String) -> Int64? { ParakeetModels.installedBytes(model) }
    func installStamp(_ model: String) -> Date? { ParakeetModels.installStamp(model) }

    func prepare(worker: Int, model: String, accelerated: Bool) async throws {
        try await pool.prepare(worker: worker, model: model, accelerated: accelerated)
    }

    func release(worker: Int) async { await pool.release(worker: worker) }

    func transcribe(worker: Int, samples16k: [Float], language: String) async throws -> String {
        try await pool.transcribe(worker: worker, samples16k: samples16k)
    }

    func download(_ model: String, progress: @escaping @Sendable (ModelDownloadStage, Int) -> Void) async throws {
        try await ParakeetModels.download(model) { stage in
            switch stage {
            case .downloading(let fraction): progress(.downloading, max(1, min(95, Int(fraction * 95))))
            case .compiling: progress(.validating, 96)
            default: break
            }
        }
    }

    func delete(_ model: String) throws {
        try ParakeetModels.delete(model)
        Task { await pool.forget(model: model) }
    }
}
