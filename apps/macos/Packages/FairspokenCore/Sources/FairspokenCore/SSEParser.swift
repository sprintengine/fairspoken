import Foundation

/// One dispatched Server-Sent Event.
public struct SSEEvent: Equatable, Sendable {
    public var type: String
    public var data: String
    public var id: String?
    public init(type: String = "message", data: String, id: String? = nil) {
        self.type = type
        self.data = data
        self.id = id
    }
}

public enum SSEMessage: Equatable, Sendable {
    case event(SSEEvent)
    /// A comment line (`: ping` heartbeats). Text excludes the leading colon and one space.
    case comment(String)
}

/// Incremental parser for `text/event-stream`, following the WHATWG HTML
/// "event stream interpretation" rules: CRLF / LF / CR line endings (a CR at a
/// chunk boundary followed by LF counts once), optional leading BOM, comment
/// lines, `event` / `data` / `id` / `retry` fields with one optional space after
/// the colon, multi-line data joined with `\n`, events with an empty data buffer
/// are not dispatched, and an unterminated event at EOF is discarded.
public struct SSEParser: Sendable {
    public private(set) var lastEventID: String?
    public private(set) var retryMilliseconds: Int?

    private var line: [UInt8] = []
    private var dataBuffer = ""
    private var hasData = false
    private var eventType = ""
    private var sawCR = false
    private var atStreamStart = true
    private var bomCheck: [UInt8] = []

    public init() {}

    /// Feeds a chunk of bytes, returning any completed messages in order.
    public mutating func feed<S: Sequence>(_ bytes: S) -> [SSEMessage] where S.Element == UInt8 {
        var out: [SSEMessage] = []
        for byte in bytes {
            if let message = push(byte) { out.append(message) }
        }
        return out
    }

    public mutating func feed(_ string: String) -> [SSEMessage] { feed(Array(string.utf8)) }

    /// Feeds one byte. A single byte can complete at most one line, so at most one message.
    public mutating func push(_ byte: UInt8) -> SSEMessage? {
        if atStreamStart {
            // Strip a UTF-8 BOM (EF BB BF) if the stream starts with one.
            let bom: [UInt8] = [0xEF, 0xBB, 0xBF]
            if bomCheck.count < 3 && byte == bom[bomCheck.count] {
                bomCheck.append(byte)
                if bomCheck.count == 3 { atStreamStart = false; bomCheck = [] }
                return nil
            }
            atStreamStart = false
            let pending = bomCheck
            bomCheck = []
            var result: SSEMessage?
            for b in pending { if let m = pushByte(b) { result = m } }
            if let m = pushByte(byte) { result = m }
            return result
        }
        return pushByte(byte)
    }

    private mutating func pushByte(_ byte: UInt8) -> SSEMessage? {
        switch byte {
        case 0x0A: // LF
            if sawCR { sawCR = false; return nil } // second half of CRLF
            return endLine()
        case 0x0D: // CR
            sawCR = true
            return endLine()
        default:
            sawCR = false
            line.append(byte)
            return nil
        }
    }

    private mutating func endLine() -> SSEMessage? {
        defer { line.removeAll(keepingCapacity: true) }
        if line.isEmpty { return dispatch() }
        let text = String(decoding: line, as: UTF8.self)
        if text.hasPrefix(":") {
            var comment = text.dropFirst()
            if comment.hasPrefix(" ") { comment = comment.dropFirst() }
            return .comment(String(comment))
        }
        let field: Substring
        var value: Substring
        if let colon = text.firstIndex(of: ":") {
            field = text[..<colon]
            value = text[text.index(after: colon)...]
            if value.hasPrefix(" ") { value = value.dropFirst() }
        } else {
            field = Substring(text)
            value = ""
        }
        switch field {
        case "event": eventType = String(value)
        case "data":
            dataBuffer += value
            dataBuffer += "\n"
            hasData = true
        case "id":
            if !value.contains("\0") { lastEventID = String(value) }
        case "retry":
            if !value.isEmpty, value.allSatisfy(\.isASCII), value.allSatisfy(\.isNumber) { retryMilliseconds = Int(value) }
        default: break
        }
        return nil
    }

    private mutating func dispatch() -> SSEMessage? {
        defer {
            dataBuffer = ""
            hasData = false
            eventType = ""
        }
        guard hasData else { return nil }
        var data = dataBuffer
        if data.hasSuffix("\n") { data.removeLast() }
        return .event(SSEEvent(type: eventType.isEmpty ? "message" : eventType, data: data, id: lastEventID))
    }
}
