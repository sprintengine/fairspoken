import Foundation

/// Request/response shapes and header rules of the transcription-host protocol (v1),
/// as spoken by the Rust client in `remote_transcription.rs`.
public enum RemoteProtocol {
    /// Identifies this client in host stats (`x-multivoice-client`).
    public static let clientID = "multivoice-macos"
    /// Engine identifier on the wire. The host chooses the model; `model` is a hint it ignores.
    public static let backendID = "parakeet"

    public static let healthPath = "v1/health"
    public static let statsPath = "v1/stats"
    public static let eventsPath = "v1/events"
    public static let streamPath = "v1/transcriptions/stream"
    public static let batchPath = "v1/transcriptions"

    public struct Options: Sendable, Equatable {
        public var token: String
        public var model: String
        public var language: String
        public var vocabularyHints: [String]
        public init(token: String, model: String, language: String, vocabularyHints: [String]) {
            self.token = token
            self.model = model
            self.language = language
            self.vocabularyHints = vocabularyHints
        }
    }

    public static func authHeaders(token: String) -> [String: String] {
        let token = token.trimmingCharacters(in: .whitespacesAndNewlines)
        return token.isEmpty ? [:] : ["Authorization": "Bearer \(token)"]
    }

    /// Every header the stream/batch upload carries (minus Content-Type).
    public static func transcriptionHeaders(_ options: Options) -> [String: String] {
        var headers = authHeaders(token: options.token)
        headers["x-multivoice-client"] = clientID
        headers["x-multivoice-backend"] = backendID
        headers["x-multivoice-model"] = options.model
        headers["x-multivoice-language"] = options.language
        if let hints = vocabularyHintsHeader(options.vocabularyHints) {
            headers["x-multivoice-vocabulary-hints"] = hints
        }
        return headers
    }

    /// Percent-encoded compact JSON array, byte-identical to `serde_json::to_string` + `percent_encode`.
    public static func vocabularyHintsHeader(_ hints: [String]) -> String? {
        guard !hints.isEmpty else { return nil }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        guard let data = try? encoder.encode(hints), let json = String(data: data, encoding: .utf8) else { return nil }
        return percentEncode(json)
    }

    /// RFC 3986 unreserved characters pass through; every other byte becomes `%XX` (upper-case hex).
    public static func percentEncode(_ input: String) -> String {
        var out = ""
        out.reserveCapacity(input.utf8.count * 3)
        for byte in input.utf8 {
            switch byte {
            case UInt8(ascii: "A")...UInt8(ascii: "Z"), UInt8(ascii: "a")...UInt8(ascii: "z"),
                 UInt8(ascii: "0")...UInt8(ascii: "9"), UInt8(ascii: "-"), UInt8(ascii: "_"),
                 UInt8(ascii: "."), UInt8(ascii: "~"):
                out.unicodeScalars.append(UnicodeScalar(byte))
            default:
                out += String(format: "%%%02X", byte)
            }
        }
        return out
    }

    /// Builds the user-facing error for a non-2xx response, surfacing the host's `{"error": …}` body.
    public static func errorMessage(statusCode: Int, body: Data?) -> String {
        let display = statusDisplay(statusCode)
        var serverMessage: String?
        if let body, let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
           let message = object["error"] as? String,
           !message.trimmingCharacters(in: .whitespaces).isEmpty {
            serverMessage = message
        }
        if let serverMessage {
            return "Remote transcription host returned an error (\(display)): \(serverMessage)"
        }
        return "Remote transcription host returned an error: \(display)"
    }

    /// `"429 Too Many Requests"`, matching reqwest's `StatusCode` display.
    public static func statusDisplay(_ code: Int) -> String {
        let reasons: [Int: String] = [
            400: "Bad Request", 401: "Unauthorized", 403: "Forbidden", 404: "Not Found",
            408: "Request Timeout", 409: "Conflict", 413: "Payload Too Large", 429: "Too Many Requests",
            500: "Internal Server Error", 502: "Bad Gateway", 503: "Service Unavailable", 504: "Gateway Timeout",
        ]
        if let reason = reasons[code] { return "\(code) \(reason)" }
        return "\(code) \(HTTPURLResponse.localizedString(forStatusCode: code).capitalized)"
    }
}

public struct RemoteHealth: Codable, Sendable, Equatable {
    public var ok: Bool
    public var mode: String
    public var backend: String
    public var serverVersion: String?
}

public struct RemoteTranscriptionResponse: Codable, Sendable, Equatable {
    public var text: String
    public var durationSeconds: Double
    public var backend: String
    public var model: String
    public var serverVersion: String?

    public init(text: String, durationSeconds: Double, backend: String, model: String, serverVersion: String?) {
        self.text = text
        self.durationSeconds = durationSeconds
        self.backend = backend
        self.model = model
        self.serverVersion = serverVersion
    }
}
