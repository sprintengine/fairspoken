import Foundation

/// Codec for the host's framed PCM stream (`POST /v1/transcriptions/stream`).
///
/// Each frame is `[u32 LE sampleRate][u32 LE n][n × i16 LE mono]`. Byte-for-byte the
/// same as `write_stream_frame` / `read_stream_frame` in `src-tauri/src/remote_transcription.rs`.
public enum StreamFrameCodec {
    public static let contentType = "application/vnd.fairspoken.pcm-stream"
    public static let maxSampleRate: UInt32 = 192_000
    public static let maxSamplesPerFrame = 192_000
    public static let headerSize = 8

    public struct Frame: Equatable, Sendable {
        public var sampleRate: UInt32
        public var samples: [Int16]
        public init(sampleRate: UInt32, samples: [Int16]) {
            self.sampleRate = sampleRate
            self.samples = samples
        }
    }

    public enum DecodeError: Error, Equatable, CustomStringConvertible {
        case unsupportedSampleRate(UInt32)
        case frameTooLarge(Int)
        case endedMidFrame

        public var description: String {
            switch self {
            case .unsupportedSampleRate: "Remote stream frame has an unsupported sample rate"
            case .frameTooLarge: "Remote stream frame is too large"
            case .endedMidFrame: "Remote stream ended mid-frame"
            }
        }
    }

    /// Encodes one frame. Callers keep `samples.count <= maxSamplesPerFrame`
    /// (see `encodeFrames` for automatic splitting).
    public static func encode(sampleRate: UInt32, samples: [Int16]) -> Data {
        var data = Data(capacity: headerSize + samples.count * 2)
        appendLE(sampleRate, to: &data)
        appendLE(UInt32(samples.count), to: &data)
        samples.withUnsafeBufferPointer { buffer in
            if UInt16(littleEndian: 1) == 1 {
                // Little-endian host (every Apple Silicon Mac): memory layout is the wire layout.
                data.append(UnsafeBufferPointer(
                    start: UnsafeRawPointer(buffer.baseAddress)?.assumingMemoryBound(to: UInt8.self),
                    count: buffer.count * 2))
            } else {
                for sample in buffer { appendLE(UInt16(bitPattern: sample), to: &data) }
            }
        }
        return data
    }

    /// Splits `samples` into as many frames as needed to respect `maxSamplesPerFrame`.
    public static func encodeFrames(sampleRate: UInt32, samples: [Int16], maxPerFrame: Int = maxSamplesPerFrame) -> Data {
        guard !samples.isEmpty else { return Data() }
        var out = Data(capacity: samples.count * 2 + headerSize * (samples.count / maxPerFrame + 1))
        var start = 0
        while start < samples.count {
            let end = min(start + maxPerFrame, samples.count)
            out.append(encode(sampleRate: sampleRate, samples: Array(samples[start..<end])))
            start = end
        }
        return out
    }

    /// Decodes a complete byte stream into frames, mirroring the host's reader:
    /// clean EOF between frames ends the stream; EOF inside a frame is an error.
    public static func decodeAll(_ data: Data) throws -> [Frame] {
        let bytes = [UInt8](data)
        var frames: [Frame] = []
        var offset = 0
        while offset < bytes.count {
            guard bytes.count - offset >= headerSize else { throw DecodeError.endedMidFrame }
            let rate = readLE32(bytes, offset)
            let count = Int(readLE32(bytes, offset + 4))
            offset += headerSize
            guard rate != 0, rate <= maxSampleRate else { throw DecodeError.unsupportedSampleRate(rate) }
            if count == 0 {
                frames.append(Frame(sampleRate: rate, samples: []))
                continue
            }
            guard count <= maxSamplesPerFrame else { throw DecodeError.frameTooLarge(count) }
            guard bytes.count - offset >= count * 2 else { throw DecodeError.endedMidFrame }
            var samples = [Int16](repeating: 0, count: count)
            for i in 0..<count {
                let lo = UInt16(bytes[offset + i * 2])
                let hi = UInt16(bytes[offset + i * 2 + 1])
                samples[i] = Int16(bitPattern: lo | (hi << 8))
            }
            offset += count * 2
            frames.append(Frame(sampleRate: rate, samples: samples))
        }
        return frames
    }

    private static func appendLE<T: FixedWidthInteger>(_ value: T, to data: inout Data) {
        withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) }
    }

    private static func readLE32(_ bytes: [UInt8], _ offset: Int) -> UInt32 {
        UInt32(bytes[offset]) | UInt32(bytes[offset + 1]) << 8 | UInt32(bytes[offset + 2]) << 16 | UInt32(bytes[offset + 3]) << 24
    }
}
