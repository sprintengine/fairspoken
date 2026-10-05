import Foundation
import Testing
@testable import FairspokenCore

@Suite("Stream frame codec")
struct StreamFrameTests {
    @Test func encodesHeaderAndLittleEndianSamples() {
        let data = StreamFrameCodec.encode(sampleRate: 16_000, samples: [1, -2, 0x1234])
        #expect([UInt8](data) == [
            0x80, 0x3E, 0x00, 0x00,   // 16000 LE
            0x03, 0x00, 0x00, 0x00,   // n = 3
            0x01, 0x00, 0xFE, 0xFF, 0x34, 0x12,
        ])
    }

    @Test func roundTripsLikeTheRustHost() throws {
        let frames = [
            StreamFrameCodec.Frame(sampleRate: 48_000, samples: [1, -2, 3]),
            StreamFrameCodec.Frame(sampleRate: 48_000, samples: [Int16.min, Int16.max]),
        ]
        var stream = Data()
        for f in frames { stream.append(StreamFrameCodec.encode(sampleRate: f.sampleRate, samples: f.samples)) }
        #expect(try StreamFrameCodec.decodeAll(stream) == frames)
    }

    @Test func emptyFramesAreLegal() throws {
        let data = StreamFrameCodec.encode(sampleRate: 16_000, samples: [])
        #expect(data.count == 8)
        #expect(try StreamFrameCodec.decodeAll(data) == [.init(sampleRate: 16_000, samples: [])])
    }

    @Test func splitsOversizedBuffersAtTheFrameLimit() throws {
        let samples = [Int16](repeating: 7, count: 250_000)
        let data = StreamFrameCodec.encodeFrames(sampleRate: 16_000, samples: samples)
        let frames = try StreamFrameCodec.decodeAll(data)
        #expect(frames.map(\.samples.count) == [192_000, 58_000])
        #expect(data.count == 250_000 * 2 + 2 * 8)
    }

    @Test func rejectsZeroOrHugeSampleRate() {
        var bad = Data()
        bad.append(contentsOf: [0, 0, 0, 0, 1, 0, 0, 0, 1, 0])
        #expect(throws: StreamFrameCodec.DecodeError.unsupportedSampleRate(0)) { try StreamFrameCodec.decodeAll(bad) }
        let huge = StreamFrameCodec.encode(sampleRate: 192_001, samples: [1])
        #expect(throws: StreamFrameCodec.DecodeError.unsupportedSampleRate(192_001)) { try StreamFrameCodec.decodeAll(huge) }
    }

    @Test func rejectsTruncatedFrames() {
        let full = StreamFrameCodec.encode(sampleRate: 16_000, samples: [1, 2, 3])
        #expect(throws: StreamFrameCodec.DecodeError.endedMidFrame) { try StreamFrameCodec.decodeAll(full.dropLast()) }
        #expect(throws: StreamFrameCodec.DecodeError.endedMidFrame) { try StreamFrameCodec.decodeAll(full.prefix(5)) }
    }

    @Test func pcmConversionClampsAndRounds() {
        #expect(PCM.int16(from: [0, 1, -1, 2, -2, 0.5]) == [0, 32767, -32767, 32767, -32767, 16384])
    }
}

@Suite("Remote URL policy")
struct RemoteURLPolicyTests {
    func ok(_ s: String) -> Bool { (try? RemoteURLPolicy.validateBaseURL(s)) != nil }

    @Test func allowsHTTPForPrivateHomeNetworkHosts() {
        #expect(ok("http://192.168.0.35:48173"))
        #expect(ok("http://10.0.0.2:48173"))
        #expect(ok("http://172.16.0.2:48173"))
        #expect(ok("http://172.31.255.1:48173"))
        #expect(ok("http://localhost:48173"))
        #expect(ok("http://127.0.0.1:48173"))
        #expect(ok("http://169.254.10.1"))
    }

    @Test func allowsHTTPForTailnetAndLocalNames() {
        #expect(ok("http://100.101.102.103:48173"))
        #expect(ok("http://100.64.0.1:48173"))
        #expect(ok("http://100.127.255.254:48173"))
        #expect(ok("http://studio-box:48173"))
        #expect(ok("http://studio-box.tail1234.ts.net:48173"))
        #expect(ok("http://studio-box.local:48173"))
        #expect(ok("http://STUDIO-BOX.LOCAL."))
        #expect(ok("http://[fd7a:115c:a1e0::1]:48173"))
        #expect(ok("http://[::1]:48173"))
        #expect(ok("http://[fe80::1]:48173"))
    }

    @Test func requiresHTTPSForPublicHosts() {
        #expect(throws: RemoteURLPolicy.ValidationError.requiresHTTPS) { try RemoteURLPolicy.validateBaseURL("http://example.com:48173") }
        #expect(!ok("http://100.128.0.1:48173"))            // outside the CGNAT /10
        #expect(!ok("http://100.63.255.255:48173"))
        #expect(!ok("http://172.32.0.1:48173"))
        #expect(!ok("http://evil.ts.net.example.com:48173"))
        #expect(!ok("http://8.8.8.8"))
        #expect(!ok("http://[2001:db8::1]:48173"))
        #expect(!ok("ftp://192.168.0.2"))
        #expect(ok("https://example.com"))
        #expect(ok("https://host.example.com:8443/"))
    }

    @Test func rejectsEmptyCredentialsAndRelativeURLs() {
        #expect(throws: RemoteURLPolicy.ValidationError.notConfigured) { try RemoteURLPolicy.validateBaseURL("   ") }
        #expect(throws: RemoteURLPolicy.ValidationError.hasCredentials) { try RemoteURLPolicy.validateBaseURL("https://u:p@example.com") }
        #expect(!ok("example.com"))
        #expect(!ok("192.168.0.2:48173/"))
    }

    @Test func messagesMatchTheRustClient() {
        #expect(RemoteURLPolicy.ValidationError.requiresHTTPS.errorDescription
                == "Remote host URL must use HTTPS unless it is localhost or a private network address")
        #expect(RemoteURLPolicy.ValidationError.notConfigured.errorDescription == "Remote transcription host URL is not configured")
    }

    @Test func stripsTrailingSlashesAndJoinsEndpoints() throws {
        let base = try RemoteURLPolicy.validateBaseURL("http://192.168.0.35:48173///")
        #expect(base.absoluteString == "http://192.168.0.35:48173")
        #expect(RemoteURLPolicy.endpoint(base, RemoteProtocol.streamPath).absoluteString == "http://192.168.0.35:48173/v1/transcriptions/stream")
        #expect(RemoteURLPolicy.endpoint(base, RemoteProtocol.eventsPath).absoluteString == "http://192.168.0.35:48173/v1/events")
    }
}

@Suite("Remote protocol headers")
struct RemoteProtocolTests {
    @Test func transcriptionHeadersCarryAuthAndHints() {
        let h = RemoteProtocol.transcriptionHeaders(.init(token: "  secret  ", model: "parakeet-tdt-0.6b-v3", language: "en",
                                                           vocabularyHints: ["Amoxicillin", "Dr O'Keeffe", "a/b"]))
        #expect(h["Authorization"] == "Bearer secret")
        #expect(h["x-fairspoken-backend"] == "parakeet")
        #expect(h["x-fairspoken-client"] == "fairspoken-macos")
        #expect(h["x-fairspoken-model"] == "parakeet-tdt-0.6b-v3")
        #expect(h["x-fairspoken-language"] == "en")
        #expect(h["x-fairspoken-vocabulary-hints"] == "%5B%22Amoxicillin%22%2C%22Dr%20O%27Keeffe%22%2C%22a%2Fb%22%5D")
    }

    @Test func omitsEmptyTokenAndHints() {
        let h = RemoteProtocol.transcriptionHeaders(.init(token: " ", model: "m", language: "en", vocabularyHints: []))
        #expect(h["Authorization"] == nil)
        #expect(h["x-fairspoken-vocabulary-hints"] == nil)
    }

    @Test func percentEncodesUTF8BytesUpperCase() {
        #expect(RemoteProtocol.percentEncode("Ó-é_~.") == "%C3%93-%C3%A9_~.")
    }

    @Test func decodesTheResponseShape() throws {
        let json = #"{"text":"Hello there.","durationSeconds":1.5,"backend":"parakeet","model":"parakeet-tdt-0.6b-v3","serverVersion":"0.9.0"}"#
        let r = try JSONDecoder().decode(RemoteTranscriptionResponse.self, from: Data(json.utf8))
        #expect(r == RemoteTranscriptionResponse(text: "Hello there.", durationSeconds: 1.5, backend: "parakeet",
                                                 model: "parakeet-tdt-0.6b-v3", serverVersion: "0.9.0"))
    }

    @Test func errorsSurfaceTheServerMessage() {
        #expect(RemoteProtocol.errorMessage(statusCode: 429, body: Data(#"{"error":"queue full"}"#.utf8))
                == "Remote transcription host returned an error (429 Too Many Requests): queue full")
        #expect(RemoteProtocol.errorMessage(statusCode: 500, body: Data("oops".utf8))
                == "Remote transcription host returned an error: 500 Internal Server Error")
    }
}
