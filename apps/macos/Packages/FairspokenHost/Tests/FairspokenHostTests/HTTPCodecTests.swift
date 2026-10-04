import Foundation
import Testing
@testable import FairspokenHost

@Suite("HTTP codec")
struct HTTPCodecTests {
    @Test func parsesARequestHeadArrivingInPieces() throws {
        var buffer = ByteBuffer()
        let raw = "GET /v1/stats?token=a%2Bb+c HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 100.64.0.7\r\n\r\nBODY"
        let bytes = Array(raw.utf8)
        buffer.append(bytes[0..<20])
        #expect(try HTTPHeadParser.parse(&buffer) == nil)
        buffer.append(bytes[20...])
        let head = try #require(try HTTPHeadParser.parse(&buffer))
        #expect(head.method == "GET")
        #expect(head.path == "/v1/stats")
        #expect(head.queryParameter("token") == "a+b c")
        #expect(head.header("x-forwarded-for") == "100.64.0.7")
        #expect(head.header("HOST") == "x")
        #expect(!head.isHTTP10)
        #expect(head.keepAlive)
        // Bytes after the head stay in the buffer for the body reader.
        #expect(String(decoding: buffer.readAll(), as: UTF8.self) == "BODY")
    }

    @Test func http10AndConnectionHeaders() throws {
        var a = ByteBuffer(Array("GET / HTTP/1.0\r\n\r\n".utf8))
        let h10 = try #require(try HTTPHeadParser.parse(&a))
        #expect(h10.isHTTP10)
        #expect(!h10.keepAlive)
        var b = ByteBuffer(Array("GET / HTTP/1.1\nConnection: close\n\n".utf8))
        let h11 = try #require(try HTTPHeadParser.parse(&b))
        #expect(!h11.keepAlive)
    }

    @Test func rejectsMalformedAndOversizedHeads() {
        var bad = ByteBuffer(Array("NOT A REQUEST\r\n\r\n".utf8))
        #expect(throws: HTTPParseError.self) { try HTTPHeadParser.parse(&bad) }
        var version = ByteBuffer(Array("GET / HTTP/2.0\r\n\r\n".utf8))
        #expect(throws: HTTPParseError.unsupportedVersion) { try HTTPHeadParser.parse(&version) }
        var huge = ByteBuffer(Array(("GET / HTTP/1.1\r\nX: " + String(repeating: "a", count: 20_000)).utf8))
        #expect(throws: HTTPParseError.self) { try HTTPHeadParser.parse(&huge) }
    }

    @Test func bodyFraming() throws {
        func framing(_ headers: String) throws -> HTTPRequestHead.BodyFraming {
            var b = ByteBuffer(Array("POST / HTTP/1.1\r\n\(headers)\r\n".utf8))
            return try #require(try HTTPHeadParser.parse(&b)).bodyFraming()
        }
        #expect(try framing("Content-Length: 5\r\n") == .length(5))
        #expect(try framing("Content-Length: 0\r\n") == HTTPRequestHead.BodyFraming.none)
        #expect(try framing("") == HTTPRequestHead.BodyFraming.none)
        #expect(try framing("Transfer-Encoding: chunked\r\nContent-Length: 9\r\n") == .chunked)
        #expect(throws: HTTPParseError.invalidContentLength) { try framing("Content-Length: -1\r\n") }
        #expect(throws: HTTPParseError.invalidContentLength) { try framing("Content-Length: 4\r\nContent-Length: 5\r\n") }
        #expect(throws: HTTPParseError.unsupportedTransferEncoding) { try framing("Transfer-Encoding: gzip\r\n") }
    }

    @Test func chunkedDecoderHandlesSplitsExtensionsAndTrailers() throws {
        let raw = Array("4;ext=1\r\nWiki\r\n5\r\npedia\r\nE\r\n in\r\n\r\nchunks.\r\n0\r\nTrailer: x\r\n\r\nNEXT".utf8)
        var decoder = ChunkedDecoder()
        var buffer = ByteBuffer()
        var out: [UInt8] = []
        var ended = false
        for byte in raw where !ended {
            buffer.append([byte])
            loop: while true {
                switch try decoder.next(&buffer) {
                case .data(let d): out += d
                case .needMore: break loop
                case .end: ended = true; break loop
                }
            }
        }
        #expect(String(decoding: out, as: UTF8.self) == "Wikipedia in\r\n\r\nchunks.")
        #expect(decoder.isFinished)
    }

    @Test func chunkedDecoderRejectsGarbage() {
        var decoder = ChunkedDecoder()
        var buffer = ByteBuffer(Array("zz\r\n".utf8))
        #expect(throws: HTTPParseError.malformedChunk) { try decoder.next(&buffer) }
        var d2 = ChunkedDecoder()
        var b2 = ByteBuffer(Array("2\r\nabXX".utf8))
        _ = try? d2.next(&b2)
        #expect(throws: HTTPParseError.malformedChunk) { try d2.next(&b2) }
    }

    @Test func percentDecodingMatchesTheRustHost() {
        #expect(HTTPHeadParser.percentDecode("a%20b+c") == "a b c")
        #expect(HTTPHeadParser.percentDecode("100%") == "100%")
        #expect(HTTPHeadParser.percentDecode("%zz") == "%zz")
        #expect(HTTPHeadParser.percentDecode("%E2%9C%93") == "✓")
    }

    @Test func jsonMatchesSerdeFormatting() {
        let v = JSONValue.object([("a", .double(1)), ("b", .double(4.1)), ("c", .null), ("d", .string("q\"\n\u{1}é")),
                                  ("e", .array([.int(3), .bool(false)]))])
        #expect(v.serialized == #"{"a":1.0,"b":4.1,"c":null,"d":"q\"\n\u0001é","e":[3,false]}"#)
        #expect(JSONValue.double(.nan).serialized == "null")
    }

    @Test func constantTimeCompare() {
        #expect(HostRouter.constantTimeEqual(Array("secret".utf8), Array("secret".utf8)))
        #expect(!HostRouter.constantTimeEqual(Array("secreT".utf8), Array("secret".utf8)))
        #expect(!HostRouter.constantTimeEqual(Array("secret2".utf8), Array("secret".utf8)))
        #expect(!HostRouter.constantTimeEqual([], Array("secret".utf8)))
    }

    @Test func clientAddressTrustsForwardingHeadersOnlyFromLoopback() throws {
        var b = ByteBuffer(Array("GET / HTTP/1.1\r\nX-Forwarded-For: 100.64.0.7, 10.0.0.1\r\nTailscale-User-Login: a@b.c\r\n\r\n".utf8))
        let head = try #require(try HTTPHeadParser.parse(&b))
        #expect(HostRouter.clientAddress(peer: "127.0.0.1", head: head) == "100.64.0.7")
        #expect(HostRouter.clientAddress(peer: "::1", head: head) == "100.64.0.7")
        #expect(HostRouter.clientAddress(peer: "192.168.1.9", head: head) == "192.168.1.9")
        var c = ByteBuffer(Array("GET / HTTP/1.1\r\nTailscale-User-Login: alice@example.com\r\n\r\n".utf8))
        let head2 = try #require(try HTTPHeadParser.parse(&c))
        #expect(HostRouter.clientAddress(peer: "127.0.0.1", head: head2) == "alice@example.com")
    }
}

@Suite("Audio input")
struct AudioInputTests {
    @Test func decodesMonoAndDownmixesStereoWAV() throws {
        let mono = try WAVDecoder.decode(TestAudio.wav(seconds: 0.5))
        #expect(mono.sampleRate == 16_000)
        #expect(mono.samples.count == 8_000)
        let stereo = try WAVDecoder.decode(TestAudio.wav(seconds: 0.25, rate: 48_000, channels: 2))
        #expect(stereo.sampleRate == 48_000)
        #expect(stereo.samples.count == 12_000)
        #expect(abs(stereo.durationSeconds - 0.25) < 0.001)
    }

    @Test func rejectsNon16BitAndGarbage() {
        #expect(throws: WAVDecoder.DecodeError.invalid("Only 16-bit PCM WAV audio is supported")) {
            try WAVDecoder.decode(TestAudio.wav(seconds: 0.1, bits: 8))
        }
        #expect(throws: WAVDecoder.DecodeError.self) { try WAVDecoder.decode(Array("not a wav file at all".utf8)) }
    }

    @Test func resamplesTo16k() {
        let out = Resampler.to16k(TestAudio.samples(seconds: 1, rate: 48_000)[...], sampleRate: 48_000)
        #expect(abs(out.count - 16_000) < 64)
        let same = Resampler.to16k([0, 16384, -32768][...], sampleRate: 16_000)
        #expect(same == [0, 0.5, -1])
    }

    @Test func frameReaderMirrorsTheRustHost() throws {
        var reader = StreamFrameReader()
        var bytes: [UInt8] = []
        func frame(_ rate: UInt32, _ samples: [Int16]) -> [UInt8] {
            var out: [UInt8] = []
            withUnsafeBytes(of: rate.littleEndian) { out += $0 }
            withUnsafeBytes(of: UInt32(samples.count).littleEndian) { out += $0 }
            for s in samples { withUnsafeBytes(of: s.littleEndian) { out += $0 } }
            return out
        }
        bytes += frame(16_000, [1, -2, 3]) + frame(16_000, []) + frame(16_000, [4])
        reader.feed(Array(bytes[0..<7]))
        #expect(try reader.next() == nil)
        reader.feed(Array(bytes[7...]))
        #expect(try reader.next() == .init(sampleRate: 16_000, samples: [1, -2, 3]))
        #expect(try reader.next()?.samples == [])
        #expect(try reader.next()?.samples == [4])
        #expect(try reader.next() == nil)
        try reader.finish()

        var badRate = StreamFrameReader()
        badRate.feed(frame(0, [1]))
        #expect(throws: StreamFrameReader.ReadError.unsupportedSampleRate) { try badRate.next() }
        var tooBig = StreamFrameReader()
        tooBig.feed(frame(192_001, [1]))
        #expect(throws: StreamFrameReader.ReadError.unsupportedSampleRate) { try tooBig.next() }
        var huge = StreamFrameReader()
        huge.feed([0x80, 0x3E, 0, 0, 0x01, 0xEE, 0x02, 0])
        #expect(throws: StreamFrameReader.ReadError.frameTooLarge) { try huge.next() }
        var truncated = StreamFrameReader()
        truncated.feed(Array(frame(16_000, [1, 2]).dropLast()))
        #expect(try truncated.next() == nil)
        #expect(throws: StreamFrameReader.ReadError.truncated) { try truncated.finish() }
    }

    @Test func segmenterCutsAtTheQuietestPoint() {
        let rate = 16_000
        var samples = TestAudio.samples(seconds: 13, rate: rate)
        // Silence around 9.5 s.
        for i in Int(9.4 * Double(rate))..<Int(9.6 * Double(rate)) { samples[i] = 0 }
        let cut = StreamSegmenter().cutPoint(samples[...], sampleRate: rate)
        let seconds = Double(cut ?? 0) / Double(rate)
        #expect(seconds > 9.39 && seconds < 9.61)
        #expect(StreamSegmenter().cutPoint(samples[..<(11 * rate)], sampleRate: rate) == nil)
    }
}
