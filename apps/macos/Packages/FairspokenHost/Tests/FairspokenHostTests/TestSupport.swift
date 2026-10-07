import Foundation
import Network
import Synchronization
@testable import FairspokenHost
import FairspokenCore

/// In-memory `HTTPTransport`: the test writes request bytes in, reads response bytes out.
final class PipeTransport: FairspokenHost.HTTPTransport, @unchecked Sendable {
    let peerAddress: String?
    private let input: AsyncStream<[UInt8]>
    private let inputContinuation: AsyncStream<[UInt8]>.Continuation
    private var iterator: AsyncStream<[UInt8]>.Iterator
    private let output = Mutex<[UInt8]>([])
    private let closedFlag = Mutex(false)

    init(peer: String? = "127.0.0.1") {
        peerAddress = peer
        (input, inputContinuation) = AsyncStream<[UInt8]>.makeStream()
        iterator = input.makeAsyncIterator()
    }

    func write(_ bytes: [UInt8]) { inputContinuation.yield(bytes) }
    func write(_ text: String) { write(Array(text.utf8)) }
    func endInput() { inputContinuation.finish() }

    func receive() async throws -> [UInt8]? {
        if closedFlag.withLock({ $0 }) { return nil }
        return await iterator.next()
    }

    func send(_ bytes: [UInt8]) async throws {
        if closedFlag.withLock({ $0 }) { throw HTTPTransportError.closed }
        output.withLock { $0 += bytes }
    }

    func close() {
        closedFlag.withLock { $0 = true }
        inputContinuation.finish()
    }

    var isClosed: Bool { closedFlag.withLock { $0 } }
    var outputText: String { String(decoding: output.withLock { $0 }, as: UTF8.self) }

    /// Waits until the output contains `needle` (or times out).
    func waitForOutput(containing needle: String, timeout: Duration = .seconds(5)) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if outputText.contains(needle) { return true }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return outputText.contains(needle)
    }
}

/// A parsed HTTP response (with a de-chunked body).
struct ParsedResponse {
    var status: Int
    var headers: [String: String]
    var body: String

    var json: [String: Any] { (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any] ?? [:] }

    static func parse(_ text: String) -> ParsedResponse? {
        guard let split = text.range(of: "\r\n\r\n") else { return nil }
        let headLines = text[..<split.lowerBound].components(separatedBy: "\r\n")
        let statusParts = headLines[0].split(separator: " ")
        guard statusParts.count >= 2, let status = Int(statusParts[1]) else { return nil }
        var headers: [String: String] = [:]
        for line in headLines.dropFirst() {
            if let colon = line.firstIndex(of: ":") {
                headers[line[..<colon].lowercased()] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            }
        }
        var body = String(text[split.upperBound...])
        if headers["transfer-encoding"] == "chunked" {
            var out = ""
            var rest = Substring(body)
            while let lineEnd = rest.range(of: "\r\n"), let size = Int(rest[..<lineEnd.lowerBound], radix: 16) {
                if size == 0 { break }
                let start = lineEnd.upperBound
                let utf8 = Array(rest[start...].utf8)
                guard utf8.count >= size else { break }
                out += String(decoding: utf8[0..<size], as: UTF8.self)
                rest = Substring(String(decoding: utf8[size...], as: UTF8.self)).dropFirst(2)
            }
            body = out
        } else if let length = headers["content-length"].flatMap(Int.init) {
            body = String(decoding: Array(body.utf8).prefix(length), as: UTF8.self)
        }
        return ParsedResponse(status: status, headers: headers, body: body)
    }
}

enum TestAudio {
    /// A 16-bit mono WAV of a sine tone.
    static func wav(seconds: Double, rate: Int = 16_000, channels: Int = 1, bits: Int = 16) -> [UInt8] {
        let frames = Int(seconds * Double(rate))
        var data: [UInt8] = []
        for i in 0..<frames {
            let v = Int16(sin(Double(i) * 0.05) * 8000)
            for _ in 0..<channels {
                if bits == 16 {
                    data += [UInt8(truncatingIfNeeded: v), UInt8(truncatingIfNeeded: v >> 8)]
                } else {
                    data.append(UInt8(truncatingIfNeeded: Int(v >> 8) + 128))
                }
            }
        }
        func le32(_ v: Int) -> [UInt8] { (0..<4).map { UInt8(truncatingIfNeeded: v >> (8 * $0)) } }
        func le16(_ v: Int) -> [UInt8] { (0..<2).map { UInt8(truncatingIfNeeded: v >> (8 * $0)) } }
        let blockAlign = channels * bits / 8
        var out = Array("RIFF".utf8) + le32(36 + data.count) + Array("WAVE".utf8)
        out += Array("fmt ".utf8) + le32(16) + le16(1) + le16(channels) + le32(rate) + le32(rate * blockAlign) + le16(blockAlign) + le16(bits)
        out += Array("data".utf8) + le32(data.count) + data
        return out
    }

    static func samples(seconds: Double, rate: Int = 16_000) -> [Int16] {
        (0..<Int(seconds * Double(rate))).map { Int16(sin(Double($0) * 0.05) * 8000) }
    }

    static func chunked(_ bytes: [UInt8]) -> [UInt8] { HTTPStatus.chunk(bytes) }
}

/// A clock the pairing limiter reads, moved by hand.
final class ManualClock: @unchecked Sendable {
    private let lock = NSLock()
    private var current = ContinuousClock.now
    var now: ContinuousClock.Instant { lock.withLock { current } }
    func advance(_ d: Duration) { lock.withLock { current += d } }
}

/// A runtime + router over a stub backend, driven through pipes.
struct Harness {
    let runtime: HostRuntime
    let router: HostRouter
    let backend: StubSpeechBackend

    init(token: String? = "secret", workers: Int = 1, queue: Int = 4, maxStreams: Int = 4, transcribeDelay: Duration = .milliseconds(20),
         heartbeat: Duration = .seconds(15), configURL: URL? = nil, pairingPassword: String = "", displayName: String = "",
         pairingLimiter: PairingLimiter = PairingLimiter()) {
        var config = HostConfiguration()
        config.token = token ?? ""
        config.pairingPassword = pairingPassword
        config.displayName = displayName
        config.workerCount = workers
        config.workerModels = Array(repeating: HostConfiguration.defaultModel, count: workers)
        config.queueCapacity = queue
        config.maxActiveStreams = maxStreams
        backend = StubSpeechBackend(transcribeDelay: transcribeDelay)
        runtime = HostRuntime(configuration: config, configURL: configURL, backend: backend, serverVersion: "9.9.9-test",
                              pairingLimiter: pairingLimiter)
        router = HostRouter(runtime: runtime, dashboardHTML: Array("<html>dashboard</html>".utf8), heartbeat: heartbeat)
    }

    /// Starts workers and waits until each has preloaded its model.
    func startWorkers() async {
        runtime.startWorkers()
        for _ in 0..<200 {
            let ready = runtime.metrics.locked { s in s.workers.allSatisfy { $0.loadedModel != nil } }
            if ready { return }
            try? await Task.sleep(for: .milliseconds(5))
        }
    }

    /// Opens a connection whose bytes the test controls.
    func connect(peer: String? = "127.0.0.1") -> (PipeTransport, Task<Void, Never>) {
        let pipe = PipeTransport(peer: peer)
        let router = self.router
        let task = Task { await HTTPConnection(transport: pipe).serve { await router.handle($0) } }
        return (pipe, task)
    }

    /// One complete request; returns the parsed response.
    func request(_ method: String, _ target: String, headers: [String: String] = [:], body: [UInt8] = [], auth: Bool = true,
                 peer: String? = "127.0.0.1") async -> ParsedResponse {
        let (pipe, task) = connect(peer: peer)
        var head = "\(method) \(target) HTTP/1.1\r\nHost: test\r\nConnection: close\r\n"
        if auth, let token = runtime.authToken { head += "Authorization: Bearer \(token)\r\n" }
        for (k, v) in headers { head += "\(k): \(v)\r\n" }
        if !body.isEmpty { head += "Content-Length: \(body.count)\r\n" }
        head += "\r\n"
        pipe.write(Array(head.utf8) + body)
        await task.value
        return ParsedResponse.parse(pipe.outputText) ?? ParsedResponse(status: 0, headers: [:], body: pipe.outputText)
    }

    func shutdown() async { await runtime.stop() }
}

/// Collects SSE frames from an in-process observer.
final class FrameCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var frames: [(type: String, json: [String: Any])] = []
    private var task: Task<Void, Never>?

    init(_ stream: AsyncStream<String>) {
        task = Task { [weak self] in
            for await frame in stream {
                let lines = frame.split(separator: "\n")
                guard lines.count >= 2, lines[0].hasPrefix("event: "), lines[1].hasPrefix("data: ") else { continue }
                let json = (try? JSONSerialization.jsonObject(with: Data(lines[1].dropFirst(6).utf8))) as? [String: Any] ?? [:]
                self?.append((String(lines[0].dropFirst(7)), json))
            }
        }
    }

    private func append(_ item: (String, [String: Any])) { lock.withLock { frames.append(item) } }

    var all: [(type: String, json: [String: Any])] { lock.withLock { frames } }
    var types: [String] { all.map(\.type) }

    func wait(for type: String, count: Int = 1, timeout: Duration = .seconds(5)) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if types.filter({ $0 == type }).count >= count { return true }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return false
    }

    deinit { task?.cancel() }
}

/// Minimal raw TCP client for tests against the real listener.
final class RawClient: @unchecked Sendable {
    private let connection: NWConnection
    private let received = Mutex<[UInt8]>([])
    private let ended = Mutex(false)

    init(port: UInt16) async throws {
        connection = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        let ready = Mutex(false)
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    if !ready.withLock({ r in defer { r = true }; return r }) { cont.resume() }
                case .failed(let e):
                    if !ready.withLock({ r in defer { r = true }; return r }) { cont.resume(throwing: e) }
                default: break
                }
            }
            connection.start(queue: .global())
        }
        receiveLoop()
    }

    private func receiveLoop() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, complete, error in
            guard let self else { return }
            if let data { self.received.withLock { $0 += [UInt8](data) } }
            if complete || error != nil {
                self.ended.withLock { $0 = true }
                return
            }
            self.receiveLoop()
        }
    }

    func send(_ text: String) { send(Array(text.utf8)) }
    func send(_ bytes: [UInt8]) { connection.send(content: Data(bytes), completion: .idempotent) }

    var text: String { String(decoding: received.withLock { $0 }, as: UTF8.self) }
    var isEnded: Bool { ended.withLock { $0 } }

    func wait(until predicate: (String) -> Bool, timeout: Duration = .seconds(5)) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if predicate(text) { return true }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return predicate(text)
    }

    func close() { connection.cancel() }
}
