import Foundation

// Discovery and pairing (PROTOCOL.md › Discovery and pairing): the `/v1/hello` and
// `/v1/pair` shapes, the HTTP seam the scanner and the pairing call share, and the
// pairing call itself.

extension RemoteProtocol {
    public static let helloPath = "v1/hello"
    public static let pairPath = "v1/pair"
    /// `service` in a Fairspoken host's hello; clients ignore any other value.
    public static let helloService = "fairspoken-host"
    /// The host's default port, probed on each peer's Tailscale IPv4.
    public static let defaultPort = 48173
    /// `clientName` is at most 64 characters on the wire.
    public static let maxClientNameLength = 64
}

/// How a host wants to be authenticated (`auth` in its hello).
public enum HostAuthMode: String, Codable, Sendable, Equatable {
    /// No token: save the URL and go.
    case open = "none"
    /// Pair with `POST /v1/pair` and the operator's password.
    case password
    /// A token is set and pairing is off: the user pastes the token.
    case token

    /// An unknown mode is treated as `token`, the one that never sends a password.
    public init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = HostAuthMode(rawValue: raw) ?? .token
    }
}

/// `GET /v1/hello`.
public struct HostHello: Codable, Sendable, Equatable {
    public var service: String
    public var `protocol`: Int
    public var name: String
    public var serverVersion: String?
    public var auth: HostAuthMode

    public init(service: String = RemoteProtocol.helloService, protocol: Int = 1, name: String,
                serverVersion: String?, auth: HostAuthMode) {
        self.service = service
        self.protocol = `protocol`
        self.name = name
        self.serverVersion = serverVersion
        self.auth = auth
    }

    /// True when this is a Fairspoken host (and not some other service on the port).
    public var isFairspokenHost: Bool { service == RemoteProtocol.helloService }
}

/// `POST /v1/pair` body.
public struct PairRequest: Codable, Sendable, Equatable {
    public var password: String
    public var clientName: String?

    public init(password: String, clientName: String?) {
        self.password = password
        let name = clientName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.clientName = name.isEmpty ? nil : String(name.prefix(RemoteProtocol.maxClientNameLength))
    }
}

/// `200` from `POST /v1/pair`. `token` is `nil` when the host has no token (nothing to pair).
public struct PairResponse: Codable, Sendable, Equatable {
    public var token: String?
    public var name: String?

    public init(token: String?, name: String?) {
        self.token = token
        self.name = name
    }
}

/// Every non-`200` answer of `POST /v1/pair`, worded for Settings.
public enum PairError: Error, Equatable, LocalizedError {
    case wrongPassword
    case pairingDisabled
    case rateLimited(retryAfterSeconds: Int?)
    case badRequest(String)
    case unexpected(String)

    public var errorDescription: String? {
        switch self {
        case .wrongPassword: "That password isn't right."
        case .pairingDisabled: "This host doesn't accept a pairing password. Enter its token instead."
        case .rateLimited(let seconds?): "Too many attempts. Try again in \(Self.waitDescription(seconds))."
        case .rateLimited(nil): "Too many attempts. Try again in a few minutes."
        case .badRequest(let message): "The host refused the request: \(message)"
        case .unexpected(let message): message
        }
    }

    /// "45 seconds", "1 minute", "4 minutes" (rounded up).
    public static func waitDescription(_ seconds: Int) -> String {
        if seconds < 60 { return seconds == 1 ? "1 second" : "\(max(seconds, 1)) seconds" }
        let minutes = (seconds + 59) / 60
        return minutes == 1 ? "1 minute" : "\(minutes) minutes"
    }
}

/// The HTTP seam for hello probes and pairing, so tests run without a network.
public protocol HTTPTransport: Sendable {
    func data(for request: URLRequest) async throws -> (Data, HTTPURLResponse)
}

/// `URLSession`, ephemeral (no cookies, cache or credential store) and not waiting for connectivity.
public struct URLSessionTransport: HTTPTransport {
    public let session: URLSession

    public init(session: URLSession? = nil) {
        self.session = session ?? {
            let config = URLSessionConfiguration.ephemeral
            config.waitsForConnectivity = false
            config.requestCachePolicy = .reloadIgnoringLocalCacheData
            return URLSession(configuration: config)
        }()
    }

    public func data(for request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        return (data, http)
    }
}

/// `POST /v1/pair`: trades the host's pairing password for its token.
public struct HostPairingClient: Sendable {
    public var transport: any HTTPTransport
    public var timeout: TimeInterval

    public init(transport: any HTTPTransport = URLSessionTransport(), timeout: TimeInterval = 10) {
        self.transport = transport
        self.timeout = timeout
    }

    /// `url` is the host's base URL (as listed by discovery). Throws `PairError` for the
    /// protocol's answers, `RemoteURLPolicy.ValidationError` for a URL the app wouldn't use,
    /// and the transport's error when the host can't be reached.
    public func pair(url: URL, password: String, clientName: String) async throws -> PairResponse {
        let base = try RemoteURLPolicy.validateBaseURL(url.absoluteString)
        var request = URLRequest(url: RemoteURLPolicy.endpoint(base, RemoteProtocol.pairPath), timeoutInterval: timeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(PairRequest(password: password, clientName: clientName))
        let (data, response) = try await transport.data(for: request)
        return try Self.interpret(status: response.statusCode, body: data,
                                  retryAfterHeader: response.value(forHTTPHeaderField: "Retry-After"))
    }

    /// Maps a `/v1/pair` answer to a response or a `PairError` (the status table in PROTOCOL.md).
    public static func interpret(status: Int, body: Data, retryAfterHeader: String?) throws(PairError) -> PairResponse {
        let error = try? JSONDecoder().decode(ErrorBody.self, from: body)
        switch status {
        case 200:
            guard let response = try? JSONDecoder().decode(PairResponse.self, from: body) else {
                throw .unexpected("The host's answer wasn't understood.")
            }
            return response
        case 401: throw .wrongPassword
        case 404: throw .pairingDisabled
        case 429:
            let header = retryAfterHeader.flatMap { Int($0.trimmingCharacters(in: .whitespaces)) }
            throw .rateLimited(retryAfterSeconds: error?.retryAfterSeconds ?? header)
        case 400: throw .badRequest(error?.error ?? RemoteProtocol.statusDisplay(400))
        default: throw .unexpected(RemoteProtocol.errorMessage(statusCode: status, body: body))
        }
    }

    struct ErrorBody: Decodable {
        var error: String?
        var retryAfterSeconds: Int?
    }
}
