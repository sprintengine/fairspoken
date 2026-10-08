import Foundation
import Synchronization

/// A byte stream the HTTP layer runs over: a Network.framework connection in the server,
/// an in-memory pipe in tests.
public protocol ByteStreamTransport: AnyObject, Sendable {
    /// Next bytes from the peer; nil at end of stream.
    func receive() async throws -> [UInt8]?
    func send(_ bytes: [UInt8]) async throws
    func close()
    /// Peer IP address (`127.0.0.1`, `::1`, `100.64.0.7`), if known.
    var peerAddress: String? { get }
}

public enum HTTPTransportError: Error, Sendable {
    case closed
    case timedOut
    /// The peer sent more than the connection buffers without it being consumed.
    case overflow
}

/// What a listener allows its peers. The timeouts cover waiting for a request head only: once
/// a head has arrived, a streamed body (`/v1/transcriptions/stream`) or a long-lived response
/// (`/v1/events`) takes as long as it takes.
public struct HTTPServerLimits: Sendable {
    /// How long a connection may sit with no request started: before the first, and between
    /// requests on a kept-alive connection.
    public var idleTimeout: Duration
    /// How long a request head may take to arrive in full, from its first byte.
    public var headerTimeout: Duration
    /// Connections served at once; more are answered 503 and closed.
    public var maxConnections: Int
    /// How long a graceful close (the last response, then FIN) may take before the connection
    /// is cancelled outright, for a peer that stops reading.
    public var closeGracePeriod: Duration

    public init(idleTimeout: Duration = .seconds(30), headerTimeout: Duration = .seconds(30), maxConnections: Int = 256,
                closeGracePeriod: Duration = .seconds(5)) {
        self.idleTimeout = idleTimeout
        self.headerTimeout = headerTimeout
        self.maxConnections = maxConnections
        self.closeGracePeriod = closeGracePeriod
    }
}

/// One request as seen by a handler: the head, a body reader and a response writer.
/// Used by a single task at a time (the connection's serve loop and the handler it calls).
public final class HTTPServerRequest: @unchecked Sendable {
    public let head: HTTPRequestHead
    public let peerAddress: String?
    let connection: HTTPConnection
    private var framing: HTTPRequestHead.BodyFraming
    private var remaining = 0
    private var chunked = ChunkedDecoder()
    public private(set) var bodyComplete: Bool
    private var sentContinue = false
    public private(set) var responded = false
    /// Set once the response says `Connection: close` (or the connection must not be reused).
    public private(set) var closeAfterResponse: Bool

    init(head: HTTPRequestHead, framing: HTTPRequestHead.BodyFraming, connection: HTTPConnection) {
        self.head = head
        self.peerAddress = connection.transport.peerAddress
        self.connection = connection
        self.framing = framing
        switch framing {
        case .none: bodyComplete = true
        case .length(let n): remaining = n; bodyComplete = false
        case .chunked: bodyComplete = false
        }
        closeAfterResponse = !head.keepAlive
    }

    // MARK: Body

    /// Next piece of the request body as it arrives; nil once the body has ended.
    /// Throws if the peer disconnects mid-body or the chunked framing is malformed.
    public func readBodyChunk() async throws -> [UInt8]? {
        if bodyComplete { return nil }
        if head.expectsContinue && !sentContinue && !responded {
            sentContinue = true
            try await connection.transport.send(HTTPStatus.head(100, headers: []))
        }
        while true {
            switch framing {
            case .none:
                bodyComplete = true
                return nil
            case .length:
                if !connection.buffer.isEmpty {
                    let bytes = connection.buffer.read(remaining)
                    remaining -= bytes.count
                    if remaining == 0 { bodyComplete = true }
                    return bytes
                }
                guard try await connection.fill() else { throw HTTPTransportError.closed }
            case .chunked:
                let step: ChunkedDecoder.Step
                do { step = try chunked.next(&connection.buffer) } catch { throw error }
                switch step {
                case .data(let bytes): return bytes
                case .end:
                    bodyComplete = true
                    return nil
                case .needMore:
                    guard try await connection.fill() else { throw HTTPTransportError.closed }
                }
            }
        }
    }

    /// Reads the whole body, failing with `tooLarge` past `limit` bytes (after reading
    /// `limit + 1`, as the Rust host's `read_limited_body`).
    public func readBody(limit: Int) async throws -> [UInt8] {
        var body: [UInt8] = []
        while let chunk = try await readBodyChunk() {
            body += chunk
            if body.count > limit { throw BodyError.tooLarge }
        }
        return body
    }

    public enum BodyError: Error { case tooLarge }

    // MARK: Response

    public func respond(status: Int, contentType: String? = nil, body: [UInt8] = [], extraHeaders: [(String, String)] = []) async {
        guard !responded else { return }
        responded = true
        if !bodyComplete { closeAfterResponse = true }
        var headers: [(String, String)] = []
        if let contentType { headers.append(("Content-Type", contentType)) }
        headers += extraHeaders
        headers.append(("Content-Length", String(body.count)))
        headers.append(("Server", "Fairspoken Server"))
        if closeAfterResponse { headers.append(("Connection", "close")) }
        try? await connection.transport.send(HTTPStatus.head(status, headers: headers) + body)
    }

    public func respondJSON(_ status: Int, _ value: HostJSON) async {
        await respond(status: status, contentType: "application/json", body: value.bytes)
    }

    public func respondError(_ status: Int, _ message: String) async {
        await respondJSON(status, .error(message))
    }

    /// Starts a streamed response with the given head; the body is then written with
    /// `writeStreamed`. The connection is always closed afterwards.
    public func startStreaming(status: Int, headers: [(String, String)]) async throws {
        responded = true
        closeAfterResponse = true
        try await connection.transport.send(HTTPStatus.head(status, headers: headers))
    }

    public func writeRaw(_ bytes: [UInt8]) async throws {
        try await connection.transport.send(bytes)
    }

    /// Waits until the peer closes its side, so an SSE writer notices a closed tab at once.
    /// The connection is not reused after a streamed response, so anything the peer still
    /// sends is dropped rather than buffered.
    public func waitForPeerClose() async {
        while true {
            do {
                guard try await connection.transport.receive() != nil else { return }
            } catch {
                return
            }
        }
    }
}

/// Serves HTTP/1.x requests on one transport, one at a time (keep-alive supported).
final class HTTPConnection: @unchecked Sendable {
    /// Most bytes one `receive()` returns over TCP (what `NWTransport` asks for).
    static let receiveChunk = 256 * 1024
    /// Unread bytes past which the connection stops receiving. Every reader consumes what it
    /// can before asking for more (a head is at most `maxHeadBytes`, a chunk-size or trailer
    /// line a few KiB, body bytes are handed out as they land), so the buffer never holds more
    /// than this plus one receive unless something stops draining it.
    static let maxBuffered = HTTPHeadParser.maxHeadBytes + receiveChunk

    let transport: any ByteStreamTransport
    let limits: HTTPServerLimits
    var buffer = ByteBuffer()

    init(transport: any ByteStreamTransport, limits: HTTPServerLimits = HTTPServerLimits()) {
        self.transport = transport
        self.limits = limits
    }

    /// Reads more bytes into the buffer; false at end of stream.
    func fill() async throws -> Bool {
        guard buffer.readableCount <= Self.maxBuffered else { throw HTTPTransportError.overflow }
        guard let bytes = try await transport.receive() else { return false }
        buffer.append(bytes)
        return true
    }

    func serve(_ handler: @Sendable (HTTPServerRequest) async -> Void) async {
        defer { transport.close() }
        while !Task.isCancelled {
            let head: HTTPRequestHead
            do {
                guard let parsed = try await readHead() else { return }
                head = parsed
            } catch let error as HTTPParseError {
                let status = error == .headTooLarge || error == .lineTooLong ? 431 : (error == .unsupportedVersion ? 505 : 400)
                let body = HostJSON.error("Malformed HTTP request").bytes
                try? await transport.send(HTTPStatus.head(status, headers: [
                    ("Content-Type", "application/json"), ("Content-Length", String(body.count)), ("Connection", "close"),
                ]) + body)
                return
            } catch {
                return
            }
            // The listener stopped while this head was arriving: don't serve it with a router
            // (and token) that are going away.
            if Task.isCancelled { return }
            let framing: HTTPRequestHead.BodyFraming
            do { framing = try head.bodyFraming() } catch {
                let status = error == .unsupportedTransferEncoding ? 501 : 400
                let body = HostJSON.error("Unsupported request body framing").bytes
                try? await transport.send(HTTPStatus.head(status, headers: [
                    ("Content-Type", "application/json"), ("Content-Length", String(body.count)), ("Connection", "close"),
                ]) + body)
                return
            }
            let request = HTTPServerRequest(head: head, framing: framing, connection: self)
            await handler(request)
            if !request.responded {
                await request.respondError(500, "No response")
            }
            if !request.bodyComplete {
                // The handler answered before the body ended (an early 4xx, say). Read and drop
                // what the peer is still sending for a moment, so it gets our response instead
                // of a reset, then close: the connection's framing is no longer trustworthy.
                await drain(request, limit: 16 << 20, timeout: .seconds(2))
                return
            }
            if request.closeAfterResponse { return }
        }
    }

    /// The next request head. Waiting for it is bounded by `idleTimeout` until a byte arrives,
    /// then by `headerTimeout`; on expiry the transport is closed (a pending `receive()` can't
    /// be cancelled any other way) and this throws `HTTPTransportError.timedOut`.
    private func readHead() async throws -> HTTPRequestHead? {
        var skipped = 0
        // A pipelined head already buffered needs no wait.
        if let head = try HTTPHeadParser.parse(&buffer, skippedEmptyLineBytes: &skipped) { return head }
        let deadline = HeadDeadline { [transport] in transport.close() }
        var started = !buffer.isEmpty || skipped > 0
        deadline.arm(started ? limits.headerTimeout : limits.idleTimeout)
        do {
            while true {
                guard try await fill() else {
                    if !deadline.disarm() { throw HTTPTransportError.timedOut }
                    return nil
                }
                if !started {
                    started = true
                    deadline.arm(limits.headerTimeout)
                }
                if let head = try HTTPHeadParser.parse(&buffer, skippedEmptyLineBytes: &skipped) {
                    guard deadline.disarm() else { throw HTTPTransportError.timedOut }
                    return head
                }
            }
        } catch {
            throw deadline.disarm() ? error : HTTPTransportError.timedOut
        }
    }

    private func drain(_ request: HTTPServerRequest, limit: Int, timeout: Duration) async {
        let deadline = ContinuousClock.now + timeout
        await withTaskGroup(of: Void.self) { group in
            group.addTask {
                var drained = 0
                while drained < limit, ContinuousClock.now < deadline {
                    guard let chunk = try? await request.readBodyChunk() else { return }
                    drained += chunk.count
                }
            }
            group.addTask { [transport] in
                try? await Task.sleep(until: deadline)
                transport.close()
            }
            await group.next()
            group.cancelAll()
        }
    }
}

/// A re-armable deadline that runs `onExpiry` once if it isn't disarmed in time.
final class HeadDeadline: Sendable {
    private struct State {
        var generation = 0
        var timer: Task<Void, Never>?
        var expired = false
    }

    private let state = Mutex(State())
    private let onExpiry: @Sendable () -> Void

    init(onExpiry: @escaping @Sendable () -> Void) { self.onExpiry = onExpiry }

    /// (Re)starts the clock at `timeout` from now.
    func arm(_ timeout: Duration) {
        state.withLock { s in
            guard !s.expired else { return }
            s.timer?.cancel()
            s.generation += 1
            let generation = s.generation
            s.timer = Task { [weak self] in
                try? await Task.sleep(for: timeout)
                if !Task.isCancelled { self?.expire(generation) }
            }
        }
    }

    /// Stops the clock; false if it had already run out.
    @discardableResult
    func disarm() -> Bool {
        state.withLock { s in
            s.timer?.cancel()
            s.timer = nil
            s.generation += 1
            return !s.expired
        }
    }

    private func expire(_ generation: Int) {
        let fire = state.withLock { s -> Bool in
            guard s.generation == generation, !s.expired else { return false }
            s.expired = true
            s.timer = nil
            return true
        }
        if fire { onExpiry() }
    }
}
