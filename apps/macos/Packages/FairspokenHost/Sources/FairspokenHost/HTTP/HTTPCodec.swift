import Foundation

/// Growable byte buffer with a read cursor, compacted lazily. Shared by the head parser and
/// the body decoders so bytes that arrive together with the head are never lost.
public struct ByteBuffer: Sendable {
    public private(set) var storage: [UInt8] = []
    public private(set) var readIndex = 0

    public init(_ bytes: [UInt8] = []) { storage = bytes }

    public var readableCount: Int { storage.count - readIndex }
    public var isEmpty: Bool { readableCount == 0 }
    public var readable: ArraySlice<UInt8> { storage[readIndex...] }

    public mutating func append<C: Collection>(_ bytes: C) where C.Element == UInt8 {
        if readIndex > 0 && readIndex >= storage.count / 2 {
            storage.removeFirst(readIndex)
            readIndex = 0
        }
        storage.append(contentsOf: bytes)
    }

    public mutating func read(_ count: Int) -> [UInt8] {
        let n = min(count, readableCount)
        let out = Array(storage[readIndex..<readIndex + n])
        readIndex += n
        return out
    }

    public mutating func readAll() -> [UInt8] { read(readableCount) }

    public mutating func skip(_ count: Int) { readIndex += min(count, readableCount) }

    /// Index (relative to the read cursor) of the first LF, if any.
    func firstLineFeed(from offset: Int = 0) -> Int? {
        var i = readIndex + offset
        while i < storage.count {
            if storage[i] == 0x0A { return i - readIndex }
            i += 1
        }
        return nil
    }

    /// Reads one line ending in LF (CR before it stripped), or nil if no full line yet.
    mutating func readLine(maxLength: Int) throws(HTTPParseError) -> String? {
        guard let lf = firstLineFeed() else {
            if readableCount > maxLength { throw .lineTooLong }
            return nil
        }
        if lf > maxLength { throw .lineTooLong }
        var end = readIndex + lf
        if end > readIndex && storage[end - 1] == 0x0D { end -= 1 }
        let line = String(decoding: storage[readIndex..<end], as: UTF8.self)
        readIndex += lf + 1
        return line
    }
}

public enum HTTPParseError: Error, Equatable, Sendable {
    case malformedRequestLine
    case malformedHeader
    case unsupportedVersion
    case headTooLarge
    case lineTooLong
    case invalidContentLength
    case unsupportedTransferEncoding
    case malformedChunk
}

public struct HTTPRequestHead: Sendable, Equatable {
    public var method: String
    /// The request target exactly as sent (`/v1/stats?token=…`).
    public var target: String
    public var path: String
    public var query: String?
    public var versionMinor: Int
    public var headers: [HTTPHeader]

    public struct HTTPHeader: Sendable, Equatable {
        public var name: String
        public var value: String
        public init(name: String, value: String) { self.name = name; self.value = value }
    }

    public enum BodyFraming: Equatable, Sendable {
        case none
        case length(Int)
        case chunked
    }

    public var isHTTP10: Bool { versionMinor == 0 }

    /// First value of a header, case-insensitively (as tiny_http's lookup in the Rust host).
    public func header(_ name: String) -> String? {
        headers.first { $0.name.caseInsensitiveCompare(name) == .orderedSame }?.value
    }

    public var keepAlive: Bool {
        let connection = header("connection")?.lowercased() ?? ""
        let tokens = connection.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
        if isHTTP10 { return tokens.contains("keep-alive") }
        return !tokens.contains("close")
    }

    public var expectsContinue: Bool {
        header("expect")?.lowercased().trimmingCharacters(in: .whitespaces) == "100-continue"
    }

    public func bodyFraming() throws(HTTPParseError) -> BodyFraming {
        if let te = header("transfer-encoding") {
            let codings = te.lowercased().split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
            guard codings.last == "chunked" else { throw .unsupportedTransferEncoding }
            guard codings.allSatisfy({ $0 == "chunked" || $0 == "identity" }) else { throw .unsupportedTransferEncoding }
            return .chunked
        }
        let lengths = headers.filter { $0.name.caseInsensitiveCompare("content-length") == .orderedSame }.map(\.value)
        guard let first = lengths.first else { return .none }
        let trimmed = first.trimmingCharacters(in: .whitespaces)
        guard lengths.allSatisfy({ $0.trimmingCharacters(in: .whitespaces) == trimmed }),
              !trimmed.isEmpty, trimmed.allSatisfy(\.isASCII), trimmed.allSatisfy(\.isNumber),
              let n = Int(trimmed) else { throw .invalidContentLength }
        return n == 0 ? .none : .length(n)
    }

    /// Value of a query parameter, percent-decoded with `+` as space (the Rust host's
    /// `query_param`).
    public func queryParameter(_ name: String) -> String? {
        guard let query else { return nil }
        for pair in query.split(separator: "&", omittingEmptySubsequences: false) {
            let parts = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            if parts.first.map(String.init) == name {
                return HTTPHeadParser.percentDecode(parts.count > 1 ? String(parts[1]) : "")
            }
        }
        return nil
    }
}

/// Incremental HTTP/1.x request-head parser.
public enum HTTPHeadParser {
    public static let maxHeadBytes = 16 * 1024
    public static let maxHeaders = 100

    /// Parses a complete head from the front of `buffer`, consuming it; nil if more bytes are
    /// needed. Leading empty lines (allowed by RFC 9112 between pipelined requests) are skipped.
    public static func parse(_ buffer: inout ByteBuffer) throws(HTTPParseError) -> HTTPRequestHead? {
        // Find the blank line that ends the head before consuming anything.
        var scan = buffer
        while let peek = scan.firstLineFeed(), peek <= 1 {
            let line = scan.readable.prefix(peek + 1)
            if line.allSatisfy({ $0 == 0x0D || $0 == 0x0A }) { scan.skip(peek + 1) } else { break }
        }
        var probe = scan
        var lines: [String] = []
        var consumed = 0
        while true {
            guard let line = try probe.readLine(maxLength: maxHeadBytes) else {
                if scan.readableCount > maxHeadBytes { throw .headTooLarge }
                return nil
            }
            consumed += 1
            if line.isEmpty { break }
            lines.append(line)
            if lines.count > maxHeaders + 1 { throw .headTooLarge }
            if probe.readIndex - scan.readIndex > maxHeadBytes { throw .headTooLarge }
        }
        _ = consumed
        guard let requestLine = lines.first else { throw .malformedRequestLine }
        let parts = requestLine.split(separator: " ", omittingEmptySubsequences: true)
        guard parts.count == 3 else { throw .malformedRequestLine }
        let method = String(parts[0])
        guard !method.isEmpty, method.allSatisfy({ $0.isLetter && $0.isASCII }) else { throw .malformedRequestLine }
        let version = parts[2]
        let minor: Int
        switch version {
        case "HTTP/1.1": minor = 1
        case "HTTP/1.0": minor = 0
        default: throw .unsupportedVersion
        }
        var target = String(parts[1])
        // Absolute-form (`http://host/path`), as some proxies send it.
        if let schemeEnd = target.range(of: "://"), target.hasPrefix("http") {
            let rest = target[schemeEnd.upperBound...]
            target = rest.firstIndex(of: "/").map { String(rest[$0...]) } ?? "/"
        }
        let path: String
        let query: String?
        if let q = target.firstIndex(of: "?") {
            path = String(target[..<q])
            query = String(target[target.index(after: q)...])
        } else {
            path = target
            query = nil
        }
        var headers: [HTTPRequestHead.HTTPHeader] = []
        for line in lines.dropFirst() {
            if line.first == " " || line.first == "\t" {
                // Obsolete line folding: append to the previous value.
                guard !headers.isEmpty else { throw .malformedHeader }
                headers[headers.count - 1].value += " " + line.trimmingCharacters(in: .whitespaces)
                continue
            }
            guard let colon = line.firstIndex(of: ":") else { throw .malformedHeader }
            let name = String(line[..<colon])
            guard !name.isEmpty, !name.contains(" "), !name.contains("\t") else { throw .malformedHeader }
            let value = line[line.index(after: colon)...].trimmingCharacters(in: CharacterSet(charactersIn: " \t"))
            headers.append(.init(name: name, value: value))
        }
        buffer = probe
        return HTTPRequestHead(method: method, target: target, path: path, query: query, versionMinor: minor, headers: headers)
    }

    /// The Rust host's `percent_decode`: `+` is a space, `%XX` a byte, anything malformed kept.
    public static func percentDecode(_ input: String) -> String {
        let bytes = Array(input.utf8)
        var out: [UInt8] = []
        out.reserveCapacity(bytes.count)
        var i = 0
        func hex(_ b: UInt8) -> UInt8? {
            switch b {
            case 0x30...0x39: b - 0x30
            case 0x41...0x46: b - 0x41 + 10
            case 0x61...0x66: b - 0x61 + 10
            default: nil
            }
        }
        while i < bytes.count {
            let b = bytes[i]
            if b == UInt8(ascii: "+") {
                out.append(0x20)
                i += 1
            } else if b == UInt8(ascii: "%"), i + 2 < bytes.count, let hi = hex(bytes[i + 1]), let lo = hex(bytes[i + 2]) {
                out.append(hi << 4 | lo)
                i += 3
            } else {
                out.append(b)
                i += 1
            }
        }
        return String(decoding: out, as: UTF8.self)
    }
}

/// `Transfer-Encoding: chunked` request-body decoder (RFC 9112 §7.1). Chunk extensions and
/// trailer fields are read and ignored.
public struct ChunkedDecoder: Sendable {
    enum State: Sendable {
        case size
        case data(remaining: Int)
        case dataEnd
        case trailers
        case done
    }

    public enum Step: Equatable, Sendable {
        case data([UInt8])
        case needMore
        case end
    }

    private var state: State = .size
    public init() {}

    public var isFinished: Bool { if case .done = state { true } else { false } }

    public mutating func next(_ buffer: inout ByteBuffer) throws(HTTPParseError) -> Step {
        while true {
            switch state {
            case .size:
                guard let line = try buffer.readLine(maxLength: 4096) else { return .needMore }
                let sizeText = line.split(separator: ";", maxSplits: 1).first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
                guard !sizeText.isEmpty, sizeText.count <= 15, let size = Int(sizeText, radix: 16), size >= 0 else {
                    throw .malformedChunk
                }
                state = size == 0 ? .trailers : .data(remaining: size)
            case .data(let remaining):
                guard !buffer.isEmpty else { return .needMore }
                let bytes = buffer.read(remaining)
                let left = remaining - bytes.count
                state = left == 0 ? .dataEnd : .data(remaining: left)
                return .data(bytes)
            case .dataEnd:
                let pending = buffer.readable.prefix(2)
                guard let first = pending.first else { return .needMore }
                if first == 0x0A {
                    buffer.skip(1)
                } else if first == 0x0D {
                    guard pending.count == 2 else { return .needMore }
                    guard pending.last == 0x0A else { throw .malformedChunk }
                    buffer.skip(2)
                } else {
                    throw .malformedChunk
                }
                state = .size
            case .trailers:
                guard let line = try buffer.readLine(maxLength: 8192) else { return .needMore }
                if line.isEmpty {
                    state = .done
                    return .end
                }
            case .done:
                return .end
            }
        }
    }
}

public enum HTTPStatus {
    public static func reason(_ code: Int) -> String {
        switch code {
        case 100: "Continue"
        case 200: "OK"
        case 202: "Accepted"
        case 204: "No Content"
        case 400: "Bad Request"
        case 401: "Unauthorized"
        case 404: "Not Found"
        case 405: "Method Not Allowed"
        case 409: "Conflict"
        case 413: "Payload Too Large"
        case 429: "Too Many Requests"
        case 431: "Request Header Fields Too Large"
        case 500: "Internal Server Error"
        case 503: "Service Unavailable"
        case 505: "HTTP Version Not Supported"
        default: "Status \(code)"
        }
    }

    /// Serialises a response head. `Content-Length` is added by the caller when there is a body.
    public static func head(_ code: Int, headers: [(String, String)]) -> [UInt8] {
        var s = "HTTP/1.1 \(code) \(reason(code))\r\n"
        for (k, v) in headers { s += "\(k): \(v)\r\n" }
        s += "\r\n"
        return Array(s.utf8)
    }

    /// One chunk of a chunked response body.
    public static func chunk(_ bytes: [UInt8]) -> [UInt8] {
        Array(String(bytes.count, radix: 16).utf8) + [0x0D, 0x0A] + bytes + [0x0D, 0x0A]
    }
}
