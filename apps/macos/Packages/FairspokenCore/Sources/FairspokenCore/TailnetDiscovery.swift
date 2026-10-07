import Foundation

// Finding Fairspoken hosts on the user's tailnet (PROTOCOL.md › Client discovery):
// `tailscale status --json` lists the machines, and each online one is probed for
// `/v1/hello` on its MagicDNS name over HTTPS and plain HTTP (`tailscale serve`), and over plain HTTP
// on its Tailscale IPv4.
// The CLI and HTTP sit behind `CommandRunning` and `HTTPTransport` so tests need neither.

/// The parts of `tailscale status --json` discovery reads.
public struct TailscaleStatus: Decodable, Sendable, Equatable {
    /// `Running`, `NeedsLogin`, `NeedsMachineAuth`, `Stopped`, `Starting` or `NoState`.
    public var backendState: String
    public var selfNode: Node?
    /// Every peer, sorted by machine name (the JSON keys them by node key, in no order).
    public var peers: [Node]

    public struct Node: Decodable, Sendable, Equatable {
        /// The OS's own name for the machine; on macOS the computer name ("Conal’s Mac mini").
        public var hostName: String
        /// MagicDNS name, fully qualified with a trailing dot (`studio.tail1234.ts.net.`).
        public var dnsName: String
        public var tailscaleIPs: [String]
        public var os: String
        public var online: Bool

        enum CodingKeys: String, CodingKey {
            case hostName = "HostName", dnsName = "DNSName", tailscaleIPs = "TailscaleIPs", os = "OS", online = "Online"
        }

        public init(hostName: String, dnsName: String, tailscaleIPs: [String], os: String, online: Bool) {
            self.hostName = hostName
            self.dnsName = dnsName
            self.tailscaleIPs = tailscaleIPs
            self.os = os
            self.online = online
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            hostName = (try? c.decodeIfPresent(String.self, forKey: .hostName)) ?? ""
            dnsName = (try? c.decodeIfPresent(String.self, forKey: .dnsName)) ?? ""
            tailscaleIPs = (try? c.decodeIfPresent([String].self, forKey: .tailscaleIPs)) ?? []
            os = (try? c.decodeIfPresent(String.self, forKey: .os)) ?? ""
            online = (try? c.decodeIfPresent(Bool.self, forKey: .online)) ?? false
        }

        /// The MagicDNS name without the trailing dot, or `nil` when there is none.
        public var fqdn: String? {
            var name = dnsName.lowercased()
            while name.hasSuffix(".") { name.removeLast() }
            return name.isEmpty ? nil : name
        }

        /// The tailnet machine name (the MagicDNS name's first label), else the host name.
        public var machineName: String {
            fqdn.flatMap { $0.split(separator: ".").first.map(String.init) } ?? hostName
        }

        public var firstIPv4: String? {
            tailscaleIPs.first { RemoteURLPolicy.parseIPv4($0) != nil }
        }
    }

    enum CodingKeys: String, CodingKey {
        case backendState = "BackendState", selfNode = "Self", peer = "Peer"
    }

    public init(backendState: String, selfNode: Node?, peers: [Node]) {
        self.backendState = backendState
        self.selfNode = selfNode
        self.peers = peers
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        backendState = try c.decode(String.self, forKey: .backendState)
        selfNode = try? c.decodeIfPresent(Node.self, forKey: .selfNode)
        let peerMap = (try? c.decodeIfPresent([String: Node].self, forKey: .peer)) ?? [:]
        peers = peerMap.values.sorted { ($0.machineName, $0.dnsName) < ($1.machineName, $1.dnsName) }
    }

    /// Finds the full MagicDNS name for a bare machine name the user typed.
    public func fqdn(forMachine name: String) -> String? {
        let wanted = name.lowercased()
        return ([selfNode].compactMap { $0 } + peers)
            .first { $0.machineName.lowercased() == wanted || $0.hostName.lowercased() == wanted }?
            .fqdn
    }
}

/// A host that answered `/v1/hello`.
public struct DiscoveredHost: Identifiable, Hashable, Sendable {
    /// The host's display name (`name` in its hello).
    public var name: String
    /// The tailnet machine it runs on (`studio-mac`), or what the user typed.
    public var machine: String
    /// Base URL to save in Settings: `https://studio-mac.tail1234.ts.net` or `http://100.101.102.103:48173`.
    public var url: URL
    public var auth: HostAuthMode
    public var serverVersion: String?
    /// True for the Mac doing the scan (Tailscale's `Self`).
    public var isThisMac: Bool

    public var id: String { url.absoluteString }

    public init(name: String, machine: String, url: URL, auth: HostAuthMode, serverVersion: String?, isThisMac: Bool = false) {
        self.name = name
        self.machine = machine
        self.url = url
        self.auth = auth
        self.serverVersion = serverVersion
        self.isThisMac = isThisMac
    }
}

public enum TailnetDiscoveryError: Error, Equatable, LocalizedError {
    case tailscaleNotInstalled
    case tailscaleNotRunning
    case tailscaleLoggedOut
    case tailscaleNeedsApproval
    case tailscaleFailed(String)
    case emptyEntry
    case invalidEntry(String)
    case noHostAnswered(String)

    public var errorDescription: String? {
        switch self {
        case .tailscaleNotInstalled:
            "Tailscale isn't installed on this Mac. Install it from tailscale.com, or type your host's machine name below."
        case .tailscaleNotRunning:
            "Tailscale isn't running. Open Tailscale and connect, then try again."
        case .tailscaleLoggedOut:
            "Tailscale is logged out. Log in to your tailnet, then try again."
        case .tailscaleNeedsApproval:
            "This Mac is waiting to be approved in your tailnet's admin console."
        case .tailscaleFailed(let detail):
            "Couldn't read Tailscale's status: \(detail)"
        case .emptyEntry:
            "Type your host's machine name, or name:port."
        case .invalidEntry(let entry):
            "“\(entry)” isn't a machine name, name:port or URL."
        case .noHostAnswered(let entry):
            "No Fairspoken host answered at \(entry)."
        }
    }
}

// MARK: - Running the CLI

public struct CommandOutput: Sendable, Equatable {
    public var status: Int32
    public var stdout: Data
    public var stderr: String
    public init(status: Int32, stdout: Data, stderr: String) {
        self.status = status
        self.stdout = stdout
        self.stderr = stderr
    }
}

/// The subprocess seam (the real one is `ProcessCommandRunner`).
public protocol CommandRunning: Sendable {
    func run(_ executable: String, arguments: [String], timeout: Duration) async throws -> CommandOutput
}

/// Runs a program with `Process`, collecting both pipes; terminates it after `timeout`.
public struct ProcessCommandRunner: CommandRunning {
    public init() {}

    public func run(_ executable: String, arguments: [String], timeout: Duration) async throws -> CommandOutput {
        let seconds = Double(timeout.components.seconds) + Double(timeout.components.attoseconds) / 1e18
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = Process()
                process.executableURL = URL(fileURLWithPath: executable)
                process.arguments = arguments
                let out = Pipe(), err = Pipe()
                process.standardOutput = out
                process.standardError = err
                process.standardInput = FileHandle.nullDevice
                do {
                    try process.run()
                } catch {
                    continuation.resume(throwing: error)
                    return
                }
                let killer = DispatchWorkItem { if process.isRunning { process.terminate() } }
                DispatchQueue.global().asyncAfter(deadline: .now() + seconds, execute: killer)
                // Read stderr alongside stdout so neither pipe can fill and stall the child.
                let errBox = DataBox()
                let group = DispatchGroup()
                DispatchQueue.global().async(group: group) { errBox.data = err.fileHandleForReading.readDataToEndOfFile() }
                let stdout = out.fileHandleForReading.readDataToEndOfFile()
                group.wait()
                process.waitUntilExit()
                killer.cancel()
                continuation.resume(returning: CommandOutput(status: process.terminationStatus, stdout: stdout,
                                                             stderr: String(decoding: errBox.data, as: UTF8.self)))
            }
        }
    }

    private final class DataBox: @unchecked Sendable { var data = Data() }
}

// MARK: - Discovery

public struct TailnetDiscovery: Sendable {
    /// Where the CLI lives when it isn't on `PATH` (GUI apps get a minimal one): the Mac app's
    /// bundled binary, then the standalone/Homebrew installs.
    public static let fallbackCLIPaths = [
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
    ]
    /// Peers that can't run a host aren't probed.
    static let skippedOSes: Set<String> = ["ios", "android", "tvos"]

    public var runner: any CommandRunning
    public var transport: any HTTPTransport
    public var searchPath: String?
    public var isExecutable: @Sendable (String) -> Bool
    public var probeTimeout: Duration
    public var maxConcurrentProbes: Int
    public var cliTimeout: Duration
    /// Ports probed on `127.0.0.1`, so a host on this Mac turns up whatever address it listens
    /// on and whether or not Tailscale is running.
    public var localPorts: [Int]

    public init(runner: any CommandRunning = ProcessCommandRunner(),
                transport: any HTTPTransport = URLSessionTransport(),
                searchPath: String? = ProcessInfo.processInfo.environment["PATH"],
                isExecutable: @escaping @Sendable (String) -> Bool = { FileManager.default.isExecutableFile(atPath: $0) },
                probeTimeout: Duration = .milliseconds(1500),
                maxConcurrentProbes: Int = 32,
                cliTimeout: Duration = .seconds(5),
                localPorts: [Int] = TailnetDiscovery.localHostPorts()) {
        self.runner = runner
        self.transport = transport
        self.searchPath = searchPath
        self.isExecutable = isExecutable
        self.probeTimeout = probeTimeout
        self.maxConcurrentProbes = max(1, maxConcurrentProbes)
        self.cliTimeout = cliTimeout
        self.localPorts = localPorts
    }

    /// Fairspoken Server's configured port (release and debug builds), then the default port.
    public static func localHostPorts(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> [Int] {
        let support = home.appendingPathComponent("Library/Application Support", isDirectory: true)
        let configured = ["ie.fairspoken.server", "ie.fairspoken.server.dev"].compactMap { bundleID -> Int? in
            let url = support.appendingPathComponent(bundleID, isDirectory: true).appendingPathComponent("host-config.json")
            guard let data = try? Data(contentsOf: url),
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let port = object["port"] as? Int, (1...65_535).contains(port) else { return nil }
            return port
        }
        var seen = Set<Int>()
        return (configured + [RemoteProtocol.defaultPort]).filter { seen.insert($0).inserted }
    }

    /// `tailscale` on `PATH`, else the first of `fallbackCLIPaths` that exists.
    public func locateCLI() -> String? {
        let onPath = (searchPath ?? "").split(separator: ":").map { "\($0)/tailscale" }
        return (onPath + Self.fallbackCLIPaths).first(where: isExecutable)
    }

    /// `tailscale status --json`, or the reason it can't be used.
    public func status() async throws(TailnetDiscoveryError) -> TailscaleStatus {
        guard let cli = locateCLI() else { throw .tailscaleNotInstalled }
        let output: CommandOutput
        do {
            output = try await runner.run(cli, arguments: ["status", "--json"], timeout: cliTimeout)
        } catch {
            throw .tailscaleFailed(error.localizedDescription)
        }
        return try Self.interpretStatus(output)
    }

    /// Reads the CLI's answer. The JSON (when there is any) says whether Tailscale is up;
    /// without it, stderr says why.
    public static func interpretStatus(_ output: CommandOutput) throws(TailnetDiscoveryError) -> TailscaleStatus {
        if let status = try? JSONDecoder().decode(TailscaleStatus.self, from: output.stdout) {
            switch status.backendState {
            case "Running": return status
            case "NeedsLogin": throw .tailscaleLoggedOut
            case "NeedsMachineAuth": throw .tailscaleNeedsApproval
            default: throw .tailscaleNotRunning
            }
        }
        let message = output.stderr.trimmingCharacters(in: .whitespacesAndNewlines)
        let lower = message.lowercased()
        if ["logged out", "needslogin", "log in at", "not logged in"].contains(where: lower.contains) {
            throw .tailscaleLoggedOut
        }
        if ["failed to connect", "doesn't appear to be running", "is stopped", "not running",
            "connection refused", "failed to start"].contains(where: lower.contains) {
            throw .tailscaleNotRunning
        }
        let firstLine = message.split(separator: "\n").first.map(String.init)
        throw .tailscaleFailed(firstLine ?? "tailscale exited with status \(output.status)")
    }

    /// One machine and the base URLs to try, best first.
    public struct Candidate: Sendable, Equatable {
        public var machine: String
        public var urls: [URL]
        public var isThisMac: Bool
    }

    /// `Self` and every online peer (minus phones and TVs): HTTPS on the MagicDNS name, then plain
    /// HTTP on it (`tailscale serve` on port 80), then HTTP on the first Tailscale IPv4 and the default port.
    public static func candidates(from status: TailscaleStatus) -> [Candidate] {
        var nodes: [(TailscaleStatus.Node, Bool)] = []
        if let me = status.selfNode { nodes.append((me, true)) }
        nodes += status.peers.filter { $0.online && !skippedOSes.contains($0.os.lowercased()) }.map { ($0, false) }
        return nodes.compactMap { node, isThisMac in
            var urls: [URL] = []
            if let fqdn = node.fqdn, let url = URL(string: "https://\(fqdn)") { urls.append(url) }
            if let fqdn = node.fqdn, let url = URL(string: "http://\(fqdn)") { urls.append(url) }
            if let ip = node.firstIPv4, let url = URL(string: "http://\(ip):\(RemoteProtocol.defaultPort)") { urls.append(url) }
            return urls.isEmpty ? nil : Candidate(machine: node.machineName, urls: urls, isThisMac: isThisMac)
        }
    }

    /// One loopback candidate per port, for hosts on this Mac.
    public static func loopbackCandidates(ports: [Int], machine: String) -> [Candidate] {
        ports.compactMap { port in
            URL(string: "http://127.0.0.1:\(port)").map { Candidate(machine: machine, urls: [$0], isThisMac: true) }
        }
    }

    /// Scans this Mac (on loopback) and the tailnet. Hosts come back once each (HTTPS preferred),
    /// this Mac first, then by machine name. When Tailscale can't be used, hosts on this Mac are
    /// still listed; with none, the Tailscale problem is the error.
    public func discover() async throws(TailnetDiscoveryError) -> [DiscoveredHost] {
        let tailnet: Result<TailscaleStatus, TailnetDiscoveryError>
        do { tailnet = .success(try await status()) } catch { tailnet = .failure(error) }
        let status = try? tailnet.get()
        let local = Self.loopbackCandidates(ports: localPorts, machine: status?.selfNode?.machineName ?? "this-mac")
        let hosts = Self.preferLoopback(await probe(local + (status.map(Self.candidates(from:)) ?? [])))
        if hosts.isEmpty, case .failure(let error) = tailnet { throw error }
        return hosts
    }

    /// Drops this Mac's tailnet-IP entry when the same host (same name, same port) answered on
    /// loopback: apps on this Mac should save `127.0.0.1`, which works with Tailscale down too.
    static func preferLoopback(_ hosts: [DiscoveredHost]) -> [DiscoveredHost] {
        let loopback = hosts.filter { $0.url.host() == "127.0.0.1" }
        return hosts.filter { host in
            guard host.isThisMac, host.url.host() != "127.0.0.1" else { return true }
            return !loopback.contains { $0.name == host.name && $0.url.port == host.url.port }
        }
    }

    /// Probes every candidate URL in parallel (at most `maxConcurrentProbes` at once) and keeps,
    /// per machine, the best URL that answered.
    public func probe(_ candidates: [Candidate]) async -> [DiscoveredHost] {
        struct Job: Sendable { var candidate: Int; var rank: Int; var url: URL }
        let jobs = candidates.enumerated().flatMap { index, candidate in
            candidate.urls.enumerated().map { Job(candidate: index, rank: $0.offset, url: $0.element) }
        }
        var best: [Int: (rank: Int, url: URL, hello: HostHello)] = [:]
        await withTaskGroup(of: (Job, HostHello?).self) { group in
            var pending = jobs.makeIterator()
            for _ in 0..<maxConcurrentProbes {
                guard let job = pending.next() else { break }
                group.addTask { (job, await self.hello(at: job.url)) }
            }
            for await (job, answer) in group {
                if let answer, best[job.candidate].map({ job.rank < $0.rank }) ?? true {
                    best[job.candidate] = (job.rank, job.url, answer)
                }
                if let next = pending.next() {
                    group.addTask { (next, await self.hello(at: next.url)) }
                }
            }
        }
        var seen = Set<String>()
        return candidates.indices.compactMap { index in
            guard let found = best[index], seen.insert(found.url.absoluteString).inserted else { return nil }
            let candidate = candidates[index]
            return DiscoveredHost(name: found.hello.name, machine: candidate.machine, url: found.url,
                                  auth: found.hello.auth, serverVersion: found.hello.serverVersion,
                                  isThisMac: candidate.isThisMac)
        }
    }

    /// Probes what the user typed: a machine name, `name:port`, an IP (`[v6]:port` too) or a full URL.
    /// A bare machine name is expanded to its MagicDNS name when the CLI is available.
    public func probe(entry: String) async throws(TailnetDiscoveryError) -> DiscoveredHost {
        let trimmed = entry.trimmingCharacters(in: .whitespacesAndNewlines)
        let tailnet = locateCLI() == nil ? nil : try? await status()
        let urls = try Self.manualURLs(trimmed) { tailnet?.fqdn(forMachine: $0) }
        let machine = Self.manualMachineName(trimmed)
        let found = await probe([Candidate(machine: machine, urls: urls, isThisMac: false)])
        guard let host = found.first else { throw .noHostAnswered(trimmed) }
        return host
    }

    /// The base URLs to try for a typed entry, best first, keeping only what `RemoteURLPolicy` allows.
    public static func manualURLs(_ entry: String, expand: (String) -> String? = { _ in nil }) throws(TailnetDiscoveryError) -> [URL] {
        let entry = entry.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !entry.isEmpty else { throw .emptyEntry }

        if entry.contains("://") {
            guard var components = URLComponents(string: entry), let scheme = components.scheme?.lowercased(),
                  ["http", "https"].contains(scheme), components.host?.isEmpty == false else { throw .invalidEntry(entry) }
            components.path = ""
            components.query = nil
            components.fragment = nil
            guard let url = components.url, let valid = try? RemoteURLPolicy.validateBaseURL(url.absoluteString) else {
                throw .invalidEntry(entry)
            }
            return [valid]
        }

        let (rawHost, port) = try splitHostPort(entry)
        var host = rawHost
        let isIP = RemoteURLPolicy.parseIPv4(host) != nil || RemoteURLPolicy.parseIPv6(host) != nil
        if !isIP && !host.contains("."), let fqdn = expand(host) { host = fqdn }
        let hostPart = RemoteURLPolicy.parseIPv6(host) != nil ? "[\(host)]" : host
        let strings = port.map { ["https://\(hostPart):\($0)", "http://\(hostPart):\($0)"] }
            ?? ["https://\(hostPart)", "http://\(hostPart):\(RemoteProtocol.defaultPort)"]
        let urls = strings.compactMap { try? RemoteURLPolicy.validateBaseURL($0) }
        guard !urls.isEmpty else { throw .invalidEntry(entry) }
        return urls
    }

    static func splitHostPort(_ entry: String) throws(TailnetDiscoveryError) -> (String, Int?) {
        var host = entry
        var portText: Substring?
        if entry.hasPrefix("[") {
            guard let close = entry.firstIndex(of: "]") else { throw .invalidEntry(entry) }
            host = String(entry[entry.index(after: entry.startIndex)..<close])
            let rest = entry[entry.index(after: close)...]
            if !rest.isEmpty {
                guard rest.hasPrefix(":") else { throw .invalidEntry(entry) }
                portText = rest.dropFirst()
            }
        } else if entry.filter({ $0 == ":" }).count == 1, let colon = entry.firstIndex(of: ":") {
            host = String(entry[..<colon])
            portText = entry[entry.index(after: colon)...]
        }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._:"))
        guard !host.isEmpty, host.unicodeScalars.allSatisfy(allowed.contains) else { throw .invalidEntry(entry) }
        guard let portText else { return (host, nil) }
        guard let port = Int(portText), (1...65_535).contains(port) else { throw .invalidEntry(entry) }
        return (host, port)
    }

    /// "studio-mac" for `studio-mac:48200`, `studio-mac.tail1234.ts.net` or `https://studio-mac.tail1234.ts.net`.
    static func manualMachineName(_ entry: String) -> String {
        let host = URLComponents(string: entry.contains("://") ? entry : "x://\(entry)")?.host ?? entry
        if RemoteURLPolicy.parseIPv4(host) != nil || RemoteURLPolicy.parseIPv6(host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))) != nil {
            return host
        }
        return host.split(separator: ".").first.map(String.init) ?? host
    }

    /// `GET <base>/v1/hello` within `probeTimeout`; `nil` unless a Fairspoken host answered.
    public func hello(at base: URL) async -> HostHello? {
        let request = URLRequest(url: RemoteURLPolicy.endpoint(base, RemoteProtocol.helloPath),
                                 cachePolicy: .reloadIgnoringLocalCacheData,
                                 timeoutInterval: Self.seconds(probeTimeout))
        let transport = self.transport
        do {
            let (data, response) = try await Self.withTimeout(probeTimeout) { try await transport.data(for: request) }
            guard response.statusCode == 200, let hello = try? JSONDecoder().decode(HostHello.self, from: data),
                  hello.isFairspokenHost else { return nil }
            return hello
        } catch {
            return nil
        }
    }

    static func seconds(_ d: Duration) -> TimeInterval {
        Double(d.components.seconds) + Double(d.components.attoseconds) / 1e18
    }

    /// Runs `operation`, cancelling it once `limit` passes (URL timeouts are idle timeouts, not totals).
    static func withTimeout<T: Sendable>(_ limit: Duration, _ operation: @escaping @Sendable () async throws -> T) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await operation() }
            group.addTask {
                try await Task.sleep(for: limit)
                throw URLError(.timedOut)
            }
            defer { group.cancelAll() }
            guard let first = try await group.next() else { throw URLError(.timedOut) }
            return first
        }
    }
}
