import FairspokenCore
import FairspokenHost
import Darwin
import Foundation
import SystemConfiguration

/// Where clients can reach this Mac: its Bonjour name, LAN addresses and Tailscale address.
nonisolated struct NetworkAddresses: Equatable, Sendable {
    struct Interface: Equatable, Sendable, Identifiable {
        enum Kind: Sendable { case lan, tailscale }
        var name: String
        var address: String
        var kind: Kind
        var id: String { "\(name)-\(address)" }
    }

    var localHostName: String?
    var interfaces: [Interface]

    var tailscale: Interface? { interfaces.first { $0.kind == .tailscale } }
    var lan: [Interface] { interfaces.filter { $0.kind == .lan } }

    static func current() -> NetworkAddresses {
        var result: [Interface] = []
        var list: UnsafeMutablePointer<ifaddrs>?
        if getifaddrs(&list) == 0, let first = list {
            var cursor: UnsafeMutablePointer<ifaddrs>? = first
            while let entry = cursor {
                defer { cursor = entry.pointee.ifa_next }
                let flags = Int32(entry.pointee.ifa_flags)
                guard let addr = entry.pointee.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET),
                      flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0 else { continue }
                var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                guard getnameinfo(addr, socklen_t(addr.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 else { continue }
                let address = String(cString: host)
                let name = String(cString: entry.pointee.ifa_name)
                if address.hasPrefix("169.254.") { continue }
                result.append(Interface(name: name, address: address, kind: isTailscale(address) ? .tailscale : .lan))
            }
            freeifaddrs(first)
        }
        // en0 (usually the built-in Ethernet or Wi-Fi) first.
        result.sort { ($0.name == "en0" ? 0 : 1, $0.name) < ($1.name == "en0" ? 0 : 1, $1.name) }
        var name: String?
        if let local = SCDynamicStoreCopyLocalHostName(nil) as String? { name = "\(local).local" }
        return NetworkAddresses(localHostName: name, interfaces: result)
    }

    /// Tailscale hands out addresses in 100.64.0.0/10 (CGNAT space).
    static func isTailscale(_ address: String) -> Bool {
        let parts = address.split(separator: ".").compactMap { Int($0) }
        return parts.count == 4 && parts[0] == 100 && (64...127).contains(parts[1])
    }
}
