import Foundation
import Synchronization

/// A byte stream the HTTP layer runs over: a Network.framework connection in the server,
/// an in-memory pipe in tests.
public protocol HTTPTransport: AnyObject, Sendable {
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

    public func respondJSON(_ status: Int, _ value: JSONValue) async {
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

    /// Waits until the peer closes its side or sends anything (after a streamed response a
    /// well-behaved client sends nothing more), so an SSE writer notices a closed tab at once.
    public func waitForPeerClose() async {
        while true {
            do {
                guard let bytes = try await connection.transport.receive() else { return }
                if bytes.isEmpty { continue }
                connection.buffer.append(bytes)
            } catch {
                return
            }
        }
    }
}

/// Serves HTTP/1.x requests on one transport, one at a time (keep-alive supported).
final class HTTPConnection: @unchecked Sendable {
    let transport: any HTTPTransport
    var buffer = ByteBuffer()

    init(transport: any HTTPTransport) {
        self.transport = transport
    }

    /// Reads more bytes into the buffer; false at end of stream.
    func fill() async throws -> Bool {
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
                let body = JSONValue.error("Malformed HTTP request").bytes
                try? await transport.send(HTTPStatus.head(status, headers: [
                    ("Content-Type", "application/json"), ("Content-Length", String(body.count)), ("Connection", "close"),
                ]) + body)
                return
            } catch {
                return
            }
            let framing: HTTPRequestHead.BodyFraming
            do { framing = try head.bodyFraming() } catch {
                let status = error == .unsupportedTransferEncoding ? 501 : 400
                let body = JSONValue.error("Unsupported request body framing").bytes
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

    private func readHead() async throws -> HTTPRequestHead? {
        while true {
            if let head = try HTTPHeadParser.parse(&buffer) { return head }
            guard try await fill() else { return nil }
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
