import AVFoundation
import Foundation

/// Mono PCM16 audio at its own sample rate.
public struct HostRecording: Sendable, Equatable {
    public var samples: [Int16]
    public var sampleRate: Int
    public init(samples: [Int16], sampleRate: Int) {
        self.samples = samples
        self.sampleRate = sampleRate
    }

    public var durationSeconds: Double { sampleRate > 0 ? Double(samples.count) / Double(sampleRate) : 0 }
}

/// Decodes a `POST /v1/transcriptions` upload: RIFF/WAVE, 16-bit integer PCM, any channel
/// count (averaged to mono), as `decode_wav` (hound) in the Rust host.
public enum WAVDecoder {
    public enum DecodeError: Error, Equatable {
        case invalid(String)
        public var message: String { if case .invalid(let m) = self { m } else { "" } }
    }

    public static func decode(_ bytes: [UInt8]) throws(DecodeError) -> HostRecording {
        func u16(_ o: Int) -> Int { Int(bytes[o]) | Int(bytes[o + 1]) << 8 }
        func u32(_ o: Int) -> Int { u16(o) | u16(o + 2) << 16 }
        guard bytes.count >= 12, bytes[0..<4].elementsEqual("RIFF".utf8), bytes[8..<12].elementsEqual("WAVE".utf8) else {
            throw .invalid("Failed to read WAV: no RIFF tag found")
        }
        var offset = 12
        var channels = 0
        var rate = 0
        var bits = 0
        var format = 0
        var sawFormat = false
        while offset + 8 <= bytes.count {
            let id = String(decoding: bytes[offset..<offset + 4], as: UTF8.self)
            let size = u32(offset + 4)
            let body = offset + 8
            if id == "fmt " {
                guard size >= 16, body + 16 <= bytes.count else { throw .invalid("Failed to read WAV: invalid fmt chunk") }
                format = u16(body)
                channels = u16(body + 2)
                rate = u32(body + 4)
                bits = u16(body + 14)
                if format == 0xFFFE, size >= 40, body + 26 <= bytes.count {
                    // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts with the real format tag.
                    format = u16(body + 24)
                }
                sawFormat = true
            } else if id == "data" {
                guard sawFormat else { throw .invalid("Failed to read WAV: data chunk before fmt chunk") }
                guard channels > 0 else { throw .invalid("WAV file has no audio channels") }
                guard format == 1, bits == 16 else { throw .invalid("Only 16-bit PCM WAV audio is supported") }
                guard rate > 0 else { throw .invalid("Failed to read WAV: invalid sample rate") }
                let end = min(bytes.count, body + size)
                let frameBytes = 2 * channels
                let frames = (end - body) / frameBytes
                var samples = [Int16](repeating: 0, count: frames)
                var p = body
                for i in 0..<frames {
                    var sum = 0
                    for _ in 0..<channels {
                        sum += Int(Int16(bitPattern: UInt16(bytes[p]) | UInt16(bytes[p + 1]) << 8))
                        p += 2
                    }
                    samples[i] = Int16(truncatingIfNeeded: sum / channels)
                }
                return HostRecording(samples: samples, sampleRate: rate)
            }
            offset = body + size + (size & 1)
        }
        throw .invalid(sawFormat ? "Failed to read WAV: no data chunk" : "Failed to read WAV: no fmt chunk")
    }
}

/// Converts PCM16 at any rate to the engine's 16 kHz Float32.
public enum Resampler {
    public static func to16k(_ samples: ArraySlice<Int16>, sampleRate: Int) -> [Float] {
        let floats = samples.map { Float($0) / 32768 }
        if sampleRate == 16_000 || floats.isEmpty { return floats }
        return convert(floats, from: Double(sampleRate)) ?? linear(floats, from: Double(sampleRate))
    }

    static func convert(_ input: [Float], from rate: Double) -> [Float]? {
        guard let inFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: 1, interleaved: false),
              let outFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false),
              let converter = AVAudioConverter(from: inFormat, to: outFormat),
              let inBuffer = AVAudioPCMBuffer(pcmFormat: inFormat, frameCapacity: AVAudioFrameCount(input.count)) else { return nil }
        inBuffer.frameLength = AVAudioFrameCount(input.count)
        input.withUnsafeBufferPointer { src in
            inBuffer.floatChannelData![0].update(from: src.baseAddress!, count: input.count)
        }
        let capacity = AVAudioFrameCount(Double(input.count) * 16_000 / rate) + 4096
        guard let outBuffer = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: capacity) else { return nil }
        let feed = OneShotBuffer(inBuffer)
        var error: NSError?
        let status = converter.convert(to: outBuffer, error: &error) { _, outStatus in
            guard let b = feed.take() else {
                outStatus.pointee = .endOfStream
                return nil
            }
            outStatus.pointee = .haveData
            return b
        }
        guard status != .error, error == nil else { return nil }
        return Array(UnsafeBufferPointer(start: outBuffer.floatChannelData![0], count: Int(outBuffer.frameLength)))
    }

    static func linear(_ input: [Float], from rate: Double) -> [Float] {
        let ratio = rate / 16_000
        let count = Int(Double(input.count) / ratio)
        return (0..<count).map { i in
            let x = Double(i) * ratio
            let j = Int(x)
            let f = Float(x - Double(j))
            let a = input[min(j, input.count - 1)], b = input[min(j + 1, input.count - 1)]
            return a + (b - a) * f
        }
    }
}

private final class OneShotBuffer: @unchecked Sendable {
    private var buffer: AVAudioPCMBuffer?
    init(_ buffer: AVAudioPCMBuffer) { self.buffer = buffer }
    func take() -> AVAudioPCMBuffer? { defer { buffer = nil }; return buffer }
}

/// Incremental reader for `/v1/transcriptions/stream` frames
/// (`[u32 LE sample_rate][u32 LE n][n × i16 LE]`), mirroring `read_stream_frame`.
public struct StreamFrameReader: Sendable {
    public static let maxSampleRate = 192_000
    public static let maxSamplesPerFrame = 192_000

    public struct Frame: Equatable, Sendable {
        public var sampleRate: Int
        public var samples: [Int16]
    }

    public enum ReadError: Error, Equatable {
        case unsupportedSampleRate
        case frameTooLarge
        case truncated
        public var message: String {
            switch self {
            case .unsupportedSampleRate: "Remote stream frame has an unsupported sample rate"
            case .frameTooLarge: "Remote stream frame is too large"
            case .truncated: "Failed to read remote stream frame: failed to fill whole buffer"
            }
        }
    }

    private var buffer = ByteBuffer()
    public init() {}

    public mutating func feed(_ bytes: [UInt8]) { buffer.append(bytes) }

    /// The next complete frame, or nil if more bytes are needed.
    public mutating func next() throws(ReadError) -> Frame? {
        guard buffer.readableCount >= 8 else { return nil }
        let head = Array(buffer.readable.prefix(8))
        let rate = Int(UInt32(head[0]) | UInt32(head[1]) << 8 | UInt32(head[2]) << 16 | UInt32(head[3]) << 24)
        let count = Int(UInt32(head[4]) | UInt32(head[5]) << 8 | UInt32(head[6]) << 16 | UInt32(head[7]) << 24)
        guard rate != 0, rate <= Self.maxSampleRate else { throw .unsupportedSampleRate }
        if count == 0 {
            buffer.skip(8)
            return Frame(sampleRate: rate, samples: [])
        }
        guard count <= Self.maxSamplesPerFrame else { throw .frameTooLarge }
        guard buffer.readableCount >= 8 + count * 2 else { return nil }
        buffer.skip(8)
        let bytes = buffer.read(count * 2)
        var samples = [Int16](repeating: 0, count: count)
        for i in 0..<count {
            samples[i] = Int16(bitPattern: UInt16(bytes[2 * i]) | UInt16(bytes[2 * i + 1]) << 8)
        }
        return Frame(sampleRate: rate, samples: samples)
    }

    /// At end of body: leftover bytes mean the stream ended mid-frame.
    public func finish() throws(ReadError) {
        if !buffer.isEmpty { throw .truncated }
    }
}

/// Decides where to cut a stream into segments that are decoded while the client is still
/// speaking. Parakeet's encoder sees 15 s windows, and a decode of anything up to that takes
/// 40–80 ms on the Neural Engine, so once 12 s are buffered the audio is cut at the quietest
/// 20 ms between 7 and 12 s (a pause between words) and that segment is decoded at once.
/// Only the tail is left for release, which keeps release-to-text near one short decode no
/// matter how long the dictation.
public struct StreamSegmenter: Sendable {
    public var triggerSeconds = 12.0
    public var earliestCutSeconds = 7.0
    public var windowSeconds = 0.02

    public init() {}

    /// Number of leading samples to cut off as a finished segment, or nil to keep buffering.
    public func cutPoint(_ samples: ArraySlice<Int16>, sampleRate: Int) -> Int? {
        let rate = Double(sampleRate)
        let trigger = Int(triggerSeconds * rate)
        guard samples.count >= trigger else { return nil }
        let window = max(1, Int(windowSeconds * rate))
        let start = Int(earliestCutSeconds * rate)
        var best = trigger
        var bestEnergy = Int64.max
        var i = start
        let base = samples.startIndex
        while i + window <= trigger {
            var energy: Int64 = 0
            var k = base + i
            let end = base + i + window
            while k < end {
                let v = Int64(samples[k])
                energy += v * v
                k += 1
            }
            if energy < bestEnergy {
                bestEnergy = energy
                best = i + window / 2
            }
            i += window
        }
        return best
    }
}
