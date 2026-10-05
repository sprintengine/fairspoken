import Foundation

public enum ComputePlacement: String, Sendable, Codable {
    case neuralEngine
    case gpu
    case cpu
    case remoteHost

    public var label: String {
        switch self {
        case .neuralEngine: "Neural Engine"
        case .gpu: "GPU"
        case .cpu: "CPU"
        case .remoteHost: "Your host"
        }
    }

    public var symbol: String {
        switch self {
        case .neuralEngine: "brain"
        case .gpu: "cpu"
        case .cpu: "memorychip"
        case .remoteHost: "server.rack"
        }
    }
}

/// One local speech model the Mac app can run through FluidAudio (CoreML).
/// Accuracy/speed figures come from FluidAudio's published benchmarks
/// (Documentation/ASR/ParakeetUltra.md; full LibriSpeech, ANE), which the FluidAudio
/// spike reproduced on an M4 (v3 ≈ Ultra latency: 38–77 ms for clips up to 11 s).
/// Parakeet Redux is deliberately absent: on M4 + macOS 26 ~98% of its encoder runs on
/// the CPU, so it is slower and less accurate than v3 (spike finding).
public struct SpeechModelInfo: Identifiable, Equatable, Sendable {
    public var id: String
    public var name: String
    public var shortName: String
    public var publisher: String
    public var repository: String
    /// Download size on disk.
    public var approxBytes: Int64
    /// Resident cost while loaded: encoder weights live in Neural Engine (kernel wired)
    /// memory, not the app's footprint. Measured by the FluidAudio spike on an M4.
    public var memoryBytes: Int64
    public var languages: String
    public var summary: String
    /// 1…5 relative hints for the gallery.
    public var accuracy: Int
    public var speed: Int
    public var werClean: Double?
    public var werOther: Double?
    public var realTimeFactor: Double?
    public var placement: ComputePlacement
    public var badge: String?
    public var isExperimental: Bool
}

public enum SpeechModelCatalog {
    public static let defaultModelID = "parakeet-tdt-0.6b-v3"

    public static let all: [SpeechModelInfo] = [
        SpeechModelInfo(
            id: "parakeet-tdt-0.6b-v3", name: "Parakeet TDT 0.6B v3", shortName: "Parakeet v3", publisher: "NVIDIA",
            repository: "FluidInference/parakeet-tdt-0.6b-v3-coreml", approxBytes: 470_000_000, memoryBytes: 450_000_000,
            languages: "25 European languages",
            summary: "The default. Fast, accurate dictation in English and 24 other European languages.",
            accuracy: 4, speed: 5, werClean: 2.27, werOther: 4.12, realTimeFactor: 128.6,
            placement: .neuralEngine, badge: "Default", isExperimental: false),
        SpeechModelInfo(
            id: "parakeet-ultra", name: "Parakeet Ultra", shortName: "Parakeet Ultra", publisher: "Moondream",
            repository: "FluidInference/parakeet-ultra-coreml", approxBytes: 613_000_000, memoryBytes: 600_000_000,
            languages: "25 European languages",
            summary: "Post-trained v3. Lowest error rate in every language at the same speed; a larger download.",
            accuracy: 5, speed: 5, werClean: 2.13, werOther: 3.81, realTimeFactor: 126.7,
            placement: .neuralEngine, badge: "Most accurate", isExperimental: false),
        SpeechModelInfo(
            id: "parakeet-tdt-0.6b-v2", name: "Parakeet TDT 0.6B v2", shortName: "Parakeet v2", publisher: "NVIDIA",
            repository: "FluidInference/parakeet-tdt-0.6b-v2-coreml", approxBytes: 470_000_000, memoryBytes: 450_000_000,
            languages: "English only",
            summary: "The English-only predecessor. Kept for parity with the Windows and Linux app.",
            accuracy: 4, speed: 5, werClean: nil, werOther: nil, realTimeFactor: nil,
            placement: .neuralEngine, badge: nil, isExperimental: false),
    ]

    public static func model(id: String) -> SpeechModelInfo? {
        all.first { $0.id == id }
    }
}
