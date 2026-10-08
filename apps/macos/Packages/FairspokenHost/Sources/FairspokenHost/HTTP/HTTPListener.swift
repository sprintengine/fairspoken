import Foundation
import Network
import Synchronization

/// `ByteStreamTransport` over a Network.framework TCP connection.
final class NWTransport: ByteStreamTransport, @unchecked Sendable {
    private let connection: NWConnection
    let peerAddress: String?
    private let closed = Mutex(false)

    init(_ connection: NWConnection) {
        self.connection = connection
        if case .hostPort(let host, _) = connection.endpoint {
            peerAddress = NWTransport.format(host)
        } else {
            peerAddress = nil
        }
    }

    /// `IpAddr::to_string()` style: dotted IPv4 (also for IPv4-mapped IPv6), compressed IPv6
    /// without a zone.
    static func format(_ host: NWEndpoint.Host) -> String {
        switch host {
        case .ipv4(let a):
            return a.rawValue.map(String.init).joined(separator: ".")
        case .ipv6(let a):
            let raw = [UInt8](a.rawValue)
            if raw.count == 16, raw[0..<10].allSatisfy({ $0 == 0 }), raw[10] == 0xFF, raw[11] == 0xFF {
                return raw[12..<16].map(String.init).joined(separator: ".")
            }
            var text = "\(a)"
            if let percent = text.firstIndex(of: "%") { text = String(text[..<percent]) }
            return text
        case .name(let name, _):
            return name
        @unknown default:
            return "\(host)"
        }
    }

    func receive() async throws -> [UInt8]? {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<[UInt8]?, Error>) in
            connection.receive(minimumIncompleteLength: 1, maximumLength: HTTPConnection.receiveChunk) { data, _, isComplete, error in
                if let data, !data.isEmpty {
                    cont.resume(returning: [UInt8](data))
                } else if let error {
                    cont.resume(throwing: error)
                } else if isComplete {
                    cont.resume(returning: nil)
                } else {
                    cont.resume(returning: [])
                }
            }
        }
    }

    func send(_ bytes: [UInt8]) async throws {
        if closed.withLock({ $0 }) { throw HTTPTransportError.closed }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            connection.send(content: Data(bytes), completion: .contentProcessed { error in
                if let error { cont.resume(throwing: error) } else { cont.resume() }
            })
        }
    }

    func close() {
        let already = closed.withLock { c in defer { c = true }; return c }
        guard !already else { return }
        // A graceful close (FIN after pending data) so the last response is delivered.
        connection.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .contentProcessed { [connection] _ in
            connection.cancel()
        })
    }
}

/// Accepts TCP connections on the configured address and serves each with an
/// `HTTPConnection` running `handler`.
public final class HTTPListener: @unchecked Sendable {
    public enum ListenError: Error, LocalizedError {
        case invalidAddress(String)
        case failed(String)

        public var errorDescription: String? {
            switch self {
            case .invalidAddress(let a): "Not a valid bind address: \(a)"
            case .failed(let m): m
            }
        }
    }

    private let listener: NWListener
    private let queue = DispatchQueue(label: "ie.fairspoken.host.listener")
    private let connections = Mutex<[ObjectIdentifier: Task<Void, Never>]>([:])
    private let handler: @Sendable (HTTPServerRequest) async -> Void
    public let host: String
    public let port: UInt16

    /// `host` is an IP address (`127.0.0.1`, `100.101.102.103`, `::1`), or `0.0.0.0` / `::`
    /// for every interface.
    public init(host: String, port: UInt16, handler: @escaping @Sendable (HTTPServerRequest) async -> Void) throws {
        self.host = host
        self.port = port
        self.handler = handler
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        let params = NWParameters(tls: nil, tcp: tcp)
        params.allowLocalEndpointReuse = true
        guard let nwPort = NWEndpoint.Port(rawValue: port) else { throw ListenError.invalidAddress("\(host):\(port)") }
        do {
            if host == "0.0.0.0" || host == "::" || host == "*" {
                listener = try NWListener(using: params, on: nwPort)
            } else {
                guard IPv4Address(host) != nil || IPv6Address(host) != nil else { throw ListenError.invalidAddress(host) }
                params.requiredLocalEndpoint = .hostPort(host: NWEndpoint.Host(host), port: nwPort)
                listener = try NWListener(using: params)
            }
        } catch let error as ListenError {
            throw error
        } catch {
            throw ListenError.failed("Failed to start transcription host on \(host):\(port): \(error.localizedDescription)")
        }
    }

    /// Starts listening; returns once the socket is bound (or throws, e.g. port in use).
    public func start() async throws {
        let started = Mutex(false)
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            listener.stateUpdateHandler = { [host, port] state in
                let first = started.withLock { s -> Bool in
                    switch state {
                    case .ready, .failed, .cancelled, .waiting:
                        defer { s = true }
                        return !s
                    default:
                        return false
                    }
                }
                guard first else { return }
                switch state {
                case .ready:
                    cont.resume()
                case .failed(let error), .waiting(let error):
                    let detail: String
                    if case .posix(let code) = error, code == .EADDRINUSE {
                        detail = "port \(port) is already in use"
                    } else if case .posix(let code) = error, code == .EADDRNOTAVAIL {
                        detail = "\(host) is not an address of this Mac"
                    } else {
                        detail = error.localizedDescription
                    }
                    cont.resume(throwing: ListenError.failed("Failed to start transcription host on \(host):\(port): \(detail)"))
                case .cancelled:
                    cont.resume(throwing: ListenError.failed("The listener was cancelled"))
                default:
                    break
                }
            }
            listener.newConnectionHandler = { [weak self] connection in
                self?.accept(connection)
            }
            listener.start(queue: queue)
        }
        // A listener that fails later (e.g. the interface went away) cancels itself.
        listener.stateUpdateHandler = { [weak self] state in
            if case .failed = state { self?.listener.cancel() }
        }
    }

    /// The port actually bound (useful with port 0 in tests).
    public var boundPort: UInt16 { listener.port?.rawValue ?? port }

    private func accept(_ connection: NWConnection) {
        let transport = NWTransport(connection)
        connection.start(queue: DispatchQueue(label: "ie.fairspoken.host.connection"))
        let id = ObjectIdentifier(transport)
        let handler = self.handler
        let task = Task.detached { [weak self] in
            await HTTPConnection(transport: transport).serve(handler)
            _ = self?.connections.withLock { $0.removeValue(forKey: id) }
        }
        connections.withLock { $0[id] = task }
    }

    public func stop() {
        listener.cancel()
        let tasks = connections.withLock { c -> [Task<Void, Never>] in
            defer { c.removeAll() }
            return Array(c.values)
        }
        tasks.forEach { $0.cancel() }
    }
}
