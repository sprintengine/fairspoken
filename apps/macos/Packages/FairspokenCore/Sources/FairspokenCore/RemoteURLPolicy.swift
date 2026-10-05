import Foundation

/// Validation of the user's transcription-host URL. Same rules and messages as
/// `validate_remote_base_url` in `src-tauri/src/remote_transcription.rs`:
/// HTTPS everywhere, plain HTTP only for loopback, RFC 1918, link-local,
/// Tailscale CGNAT (100.64.0.0/10), IPv6 ULA/link-local, single-label names,
/// `*.ts.net` (MagicDNS) and `*.local` (mDNS).
public enum RemoteURLPolicy {
    public enum ValidationError: Error, Equatable, LocalizedError {
        case notConfigured
        case invalid(String)
        case hasCredentials
        case missingHost
        case requiresHTTPS

        public var errorDescription: String? {
            switch self {
            case .notConfigured: "Remote transcription host URL is not configured"
            case .invalid(let detail): "Invalid remote host URL: \(detail)"
            case .hasCredentials: "Remote host URL must not contain username or password"
            case .missingHost: "Remote host URL must include a host"
            case .requiresHTTPS: "Remote host URL must use HTTPS unless it is localhost or a private network address"
            }
        }
    }

    public static func validateBaseURL(_ raw: String) throws(ValidationError) -> URL {
        var trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix("/") { trimmed.removeLast() }
        guard !trimmed.isEmpty else { throw .notConfigured }
        guard let components = URLComponents(string: trimmed), let scheme = components.scheme?.lowercased(), !scheme.isEmpty else {
            throw .invalid("relative URL without a base")
        }
        guard let url = components.url else { throw .invalid("malformed URL") }
        if !(components.user ?? "").isEmpty || components.password != nil { throw .hasCredentials }
        guard var host = components.host, !host.isEmpty else { throw .missingHost }
        if host.hasPrefix("[") && host.hasSuffix("]") { host = String(host.dropFirst().dropLast()) }
        if scheme != "https" && !(scheme == "http" && hostAllowsPlainHTTP(host)) {
            throw .requiresHTTPS
        }
        return url
    }

    /// Resolves an API path against the base the way `Url::join` does (RFC 3986 relative reference).
    public static func endpoint(_ base: URL, _ path: String) -> URL {
        URL(string: path, relativeTo: base)?.absoluteURL ?? base.appendingPathComponent(path)
    }

    public static func hostAllowsPlainHTTP(_ rawHost: String) -> Bool {
        var host = rawHost
        if host.hasPrefix("[") && host.hasSuffix("]") { host = String(host.dropFirst().dropLast()) }
        if host == "localhost" || host == "localhost." { return true }
        if let v4 = parseIPv4(host) {
            let (a, b) = (v4[0], v4[1])
            return a == 127                                  // loopback
                || a == 10                                   // 10/8
                || (a == 172 && (16...31).contains(b))       // 172.16/12
                || (a == 192 && b == 168)                    // 192.168/16
                || a == 169                                  // link-local (Rust also accepts all of 169/8)
                || (a == 100 && (64...127).contains(b))      // Tailscale CGNAT 100.64/10
        }
        if let v6 = parseIPv6(host) {
            let isLoopback = v6.dropLast().allSatisfy { $0 == 0 } && v6[15] == 1
            let isUniqueLocal = (v6[0] & 0xFE) == 0xFC        // fc00::/7
            let isLinkLocal = v6[0] == 0xFE && (v6[1] & 0xC0) == 0x80 // fe80::/10
            return isLoopback || isUniqueLocal || isLinkLocal
        }
        return hostIsPrivateName(host)
    }

    /// Names that only resolve inside a private network: single-label MagicDNS
    /// names, `*.ts.net` and mDNS `*.local`.
    public static func hostIsPrivateName(_ host: String) -> Bool {
        var name = host.lowercased()
        while name.hasSuffix(".") { name.removeLast() }
        guard !name.isEmpty else { return false }
        return !name.contains(".") || name.hasSuffix(".ts.net") || name.hasSuffix(".local")
    }

    static func parseIPv4(_ s: String) -> [UInt8]? {
        var addr = in_addr()
        guard s.withCString({ inet_pton(AF_INET, $0, &addr) }) == 1 else { return nil }
        return withUnsafeBytes(of: addr.s_addr) { Array($0) }
    }

    static func parseIPv6(_ s: String) -> [UInt8]? {
        var addr = in6_addr()
        guard s.withCString({ inet_pton(AF_INET6, $0, &addr) }) == 1 else { return nil }
        return withUnsafeBytes(of: addr) { Array($0) }
    }
}
