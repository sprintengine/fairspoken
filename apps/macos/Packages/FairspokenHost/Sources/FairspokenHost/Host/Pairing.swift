import CryptoKit
import Foundation
import Synchronization
import SystemConfiguration

/// What a client needs to get in: the bearer token and the pairing password that hands it
/// out (PROTOCOL.md "Discovery and pairing"). Both change at run time: `POST /v1/config` may
/// set a password, and setting one on a host without a token generates the token.
public struct HostCredentials: Sendable, Equatable {
    public var token: String?
    public var pairingPassword: String?

    public init(token: String?, pairingPassword: String?) {
        self.token = token
        self.pairingPassword = pairingPassword
    }

    public var pairingEnabled: Bool { pairingPassword != nil }

    /// `/v1/hello` `auth`: `none` (no token), `password` (pair) or `token` (token, no pairing).
    public var authMode: String {
        guard token != nil else { return "none" }
        return pairingEnabled ? "password" : "token"
    }

    /// Compares SHA-256 digests in constant time, so neither the content nor the length of
    /// the password leaks through timing.
    public func passwordMatches(_ candidate: String) -> Bool {
        guard let pairingPassword else { return false }
        return HostRouter.constantTimeEqual(Array(SHA256.hash(data: Data(candidate.utf8))),
                                            Array(SHA256.hash(data: Data(pairingPassword.utf8))))
    }
}

/// One `POST /v1/pair` attempt, kept in memory for the app's Clients page (not in `/v1/stats`).
public struct PairingAttempt: Sendable, Equatable, Identifiable {
    public enum Outcome: String, Sendable {
        case paired
        /// The host has no token, so there was nothing to hand out (still a 200).
        case open
        case wrongPassword
        case disabled
        case limited
    }

    public var id: UInt64
    public var atMs: UInt64
    public var client: String?
    public var clientName: String?
    public var outcome: Outcome
    public var ok: Bool { outcome == .paired || outcome == .open }
}

/// Failed pairing attempts per client address and overall, over a sliding 10-minute window
/// (PROTOCOL.md: at most 5 per address and 20 in total). Once a scope is full, every attempt
/// from it is refused, right password or not, until its oldest failure ages out. In memory
/// only; at most `overall` failures are ever held, as no attempt runs past that.
public final class PairingLimiter: Sendable {
    public static let window: Duration = .seconds(600)
    public static let perAddress = 5
    public static let overall = 20

    public enum Verdict: Equatable, Sendable {
        case matched
        case mismatched
        case limited(retryAfterSeconds: Int)
    }

    private struct State {
        var byAddress: [String: [ContinuousClock.Instant]] = [:]
        var all: [ContinuousClock.Instant] = []
    }

    private let state = Mutex(State())
    private let now: @Sendable () -> ContinuousClock.Instant

    /// `now` is injectable so tests can move through the window without waiting.
    public init(now: @escaping @Sendable () -> ContinuousClock.Instant = { .now }) {
        self.now = now
    }

    /// Checks the limits, runs `matches` and records a failure, as one step: two wrong
    /// attempts racing each other can't both slip under the limit.
    public func attempt(address: String, _ matches: () -> Bool) -> Verdict {
        let now = self.now()
        return state.withLock { s in
            let cutoff = now - Self.window
            s.all.removeAll { $0 <= cutoff }
            for (key, times) in s.byAddress {
                let kept = times.filter { $0 > cutoff }
                s.byAddress[key] = kept.isEmpty ? nil : kept
            }
            let mine = s.byAddress[address] ?? []
            var waits: [Duration] = []
            if mine.count >= Self.perAddress { waits.append(mine[mine.count - Self.perAddress] + Self.window - now) }
            if s.all.count >= Self.overall { waits.append(s.all[s.all.count - Self.overall] + Self.window - now) }
            if let wait = waits.max() {
                return .limited(retryAfterSeconds: max(1, Int((Double(wait.components.seconds) + Double(wait.components.attoseconds) / 1e18).rounded(.up))))
            }
            if matches() { return .matched }
            s.byAddress[address, default: []].append(now)
            s.all.append(now)
            return .mismatched
        }
    }
}

public enum MachineName {
    /// The Rust host's last resort when the machine has no usable name.
    public static let fallback = "Fairspoken host"

    /// This Mac's name as Sharing settings show it ("Studio Mac"), else its Bonjour name, else
    /// the host name. Read once per start: `/v1/hello` must stay cheap.
    public static func current() -> String {
        let host = ProcessInfo.processInfo.hostName
        let candidates = [SCDynamicStoreCopyComputerName(nil, nil) as String?, SCDynamicStoreCopyLocalHostName(nil) as String?,
                          host.hasSuffix(".local") ? String(host.dropLast(6)) : host]
        return candidates.lazy.compactMap { $0.flatMap { HostConfiguration.cleanName($0) } }.first ?? fallback
    }
}
