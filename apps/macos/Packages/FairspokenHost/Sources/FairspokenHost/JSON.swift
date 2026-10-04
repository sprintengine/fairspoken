import Foundation

/// Ordered JSON output. The host's responses are built from this rather than `JSONEncoder`
/// so the wire shape matches the Rust host exactly: keys in a stable order, optional fields
/// present as `null` (serde's default) instead of omitted, and floats printed as `1.0`
/// rather than `1`.
public indirect enum JSONValue: Sendable {
    case null
    case bool(Bool)
    case int(Int64)
    case double(Double)
    case string(String)
    case array([JSONValue])
    case object([(String, JSONValue)])

    public static func int(_ value: Int) -> JSONValue { .int(Int64(value)) }
    public static func optionalString(_ value: String?) -> JSONValue { value.map { .string($0) } ?? .null }
    public static func optionalInt(_ value: Int?) -> JSONValue { value.map { .int(Int64($0)) } ?? .null }

    public var serialized: String {
        var out = ""
        write(into: &out)
        return out
    }

    public var bytes: [UInt8] { Array(serialized.utf8) }

    public func write(into out: inout String) {
        switch self {
        case .null: out += "null"
        case .bool(let b): out += b ? "true" : "false"
        case .int(let i): out += String(i)
        case .double(let d): out += Self.format(d)
        case .string(let s): Self.escape(s, into: &out)
        case .array(let items):
            out += "["
            for (i, item) in items.enumerated() {
                if i > 0 { out += "," }
                item.write(into: &out)
            }
            out += "]"
        case .object(let fields):
            out += "{"
            for (i, (key, value)) in fields.enumerated() {
                if i > 0 { out += "," }
                Self.escape(key, into: &out)
                out += ":"
                value.write(into: &out)
            }
            out += "}"
        }
    }

    /// serde_json style: integral floats keep a `.0`, others use the shortest round-trip form.
    static func format(_ d: Double) -> String {
        guard d.isFinite else { return "null" }
        if d == d.rounded(), abs(d) < 1e15 { return String(Int64(d)) + ".0" }
        return String(d)
    }

    static func escape(_ s: String, into out: inout String) {
        out += "\""
        for scalar in s.unicodeScalars {
            switch scalar {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            case "\u{08}": out += "\\b"
            case "\u{0C}": out += "\\f"
            default:
                if scalar.value < 0x20 {
                    out += String(format: "\\u%04x", scalar.value)
                } else {
                    out.unicodeScalars.append(scalar)
                }
            }
        }
        out += "\""
    }

    public static func error(_ message: String) -> JSONValue { .object([("error", .string(message))]) }
}

/// Helpers for reading request bodies parsed by `JSONSerialization`.
enum JSONInput {
    static func isBool(_ value: Any) -> Bool {
        guard let n = value as? NSNumber else { return false }
        return CFGetTypeID(n) == CFBooleanGetTypeID()
    }

    /// A non-negative integer that fits `max`, or nil for any other value (bools, floats, strings).
    static func unsignedInteger(_ value: Any, max: Int64) -> Int64? {
        guard let n = value as? NSNumber, !isBool(n) else { return nil }
        let d = n.doubleValue
        guard d.isFinite, d == d.rounded(), d >= 0, d <= Double(max) else { return nil }
        return n.int64Value
    }
}
