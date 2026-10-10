import FairspokenCore
import FluidAudio
import Foundation
import Synchronization
import Testing
@testable import FairspokenSpeech

/// Installs a real model archive through the download-link path into a temporary folder (never
/// FluidAudio's shared cache), checks FluidAudio finds every file, and optionally loads it onto
/// the Neural Engine and transcribes a moment of near-silence. Skipped unless
/// `FAIRSPOKEN_REAL_MODEL_LINK` is set:
///
///     FAIRSPOKEN_REAL_MODEL_LINK=http://127.0.0.1:8765/parakeet-tdt-0.6b-v3-mac.zip \
///     FAIRSPOKEN_REAL_MODEL_LOAD=1 swift test   # in Packages/FairspokenSpeech
///
/// `FAIRSPOKEN_REAL_MODEL_ID` picks the model (default `parakeet-tdt-0.6b-v3`).
@Suite("Real model link (opt-in)")
struct RealModelLinkTests {
    static let environment = ProcessInfo.processInfo.environment
    static let link = environment["FAIRSPOKEN_REAL_MODEL_LINK"]

    @Test(.enabled(if: link != nil, "Set FAIRSPOKEN_REAL_MODEL_LINK to a model archive's link or path"))
    func installsAndLoadsARealArchive() async throws {
        let id = Self.environment["FAIRSPOKEN_REAL_MODEL_ID"] ?? "parakeet-tdt-0.6b-v3"
        let v = try #require(ParakeetModels.version(for: id))
        let required = try #require(ParakeetModels.requiredFiles(for: id))
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("real-model-link-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        // FluidAudio finds a model by its folder name, so keep the cache's name.
        let destination = root.appendingPathComponent(AsrModels.defaultCacheDirectory(for: v).lastPathComponent, isDirectory: true)
        #expect(!destination.path.contains("/FluidAudio/Models/"))
        let link = try ModelLink.parse(try #require(Self.link))
        let stages = Mutex<[String]>([])
        let started = ContinuousClock.now
        try await ParakeetModels.install(id, from: link, into: destination, required: required, progress: { stage in
            let name = switch stage {
            case .downloading: "downloading"
            case .unpacking: "unpacking"
            default: "\(stage)"
            }
            stages.withLock { if $0.last != name { $0.append(name) } }
        }) { dir in
            AsrModels.modelsExist(at: dir, version: v) ? [] : ModelDirectoryInstaller.missingFiles(in: dir, required: required)
        }
        let installed = ContinuousClock.now - started
        #expect(AsrModels.modelsExist(at: destination, version: v))
        #expect(stages.withLock { $0 } == ["downloading", "unpacking"])
        print("Installed \(id) from \(link.display) in \(installed): \(try FileManager.default.contentsOfDirectory(atPath: destination.path).sorted())")

        guard Self.environment["FAIRSPOKEN_REAL_MODEL_LOAD"] == "1" else { return }
        let loadStarted = ContinuousClock.now
        let models = try await AsrModels.load(from: destination, version: v)
        let manager = AsrManager(config: ParakeetModels.managerConfig(v))
        try await manager.loadModels(models)
        print("Loaded onto the Neural Engine in \(ContinuousClock.now - loadStarted)")
        let probe = (0..<16_000).map { i in Float(sin(Double(i) * 0.05)) * 0.0008 }
        var state = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
        let result = try await manager.transcribe(probe, decoderState: &state)
        print("Transcribed 1 s of near-silence: “\(result.text)”")
        await manager.cleanup()
        // Nothing reached the shared cache's folder for this model from here.
        #expect(!FileManager.default.fileExists(atPath: root.appendingPathComponent("FluidAudio").path))
    }
}
