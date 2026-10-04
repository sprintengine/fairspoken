import Foundation

public struct AudioStats: Equatable, Sendable {
    public var durationSeconds: Double
    public var rms: Float
    public var peak: Float

    public init(samples: [Float], sampleRate: Double) {
        durationSeconds = sampleRate > 0 ? Double(samples.count) / sampleRate : 0
        var sum: Float = 0
        var peak: Float = 0
        for s in samples {
            sum += s * s
            peak = max(peak, abs(s))
        }
        rms = samples.isEmpty ? 0 : (sum / Float(samples.count)).squareRoot()
        self.peak = peak
    }
}

/// Rejects recordings that cannot contain a dictation, with the Rust app's thresholds
/// (`MIN_RECORDING_SECONDS`, `SILENCE_RMS_THRESHOLD`, `SILENCE_PEAK_THRESHOLD` in lib.rs).
public enum RecordingGate {
    public static let minimumSeconds = 0.35
    public static let silenceRMS: Float = 0.001
    public static let silencePeak: Float = 0.008

    public enum Verdict: Equatable, Sendable {
        case accept
        case tooShort
        case silent
    }

    public static func evaluate(_ stats: AudioStats) -> Verdict {
        if stats.durationSeconds < minimumSeconds { return .tooShort }
        if stats.rms < silenceRMS && stats.peak < silencePeak { return .silent }
        return .accept
    }
}

public enum PCM {
    /// Float [-1, 1] → Int16, clamped (wire format for the host).
    public static func int16(from samples: [Float], gain: Float = 1) -> [Int16] {
        samples.map { s in
            let v = max(-1, min(1, s * gain))
            return Int16((v * 32767).rounded())
        }
    }

    /// Perceptual 0…1 meter value from an RMS amplitude (-55 dBFS floor).
    public static func meterLevel(rms: Float) -> Float {
        guard rms > 0 else { return 0 }
        let db = 20 * log10(rms)
        return max(0, min(1, (db + 55) / 50))
    }
}
