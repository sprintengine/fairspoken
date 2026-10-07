import Foundation
import Synchronization
import Testing
@testable import FairspokenCore

// MARK: - Fixtures and fakes

/// Trimmed `tailscale status --json` from a real tailnet (keys, IPs and names changed): this Mac,
/// a Linux host, an offline Windows PC, a peer with only an IPv6 address and an Android phone.
/// Every DNSName ends with the trailing dot the CLI prints.
let tailscaleStatusFixture = #"""
{
  "Version": "1.88.1-t8a6f3b2e1-g4d1c5f0a2",
  "TUN": true,
  "BackendState": "Running",
  "HaveNodeKey": true,
  "AuthURL": "",
  "TailscaleIPs": ["100.91.70.66", "fd7a:115c:a1e0::f03b:4643"],
  "Self": {
    "ID": "nSelf1CNTRL",
    "PublicKey": "nodekey:1111111111111111111111111111111111111111111111111111111111111111",
    "HostName": "Conal’s Mac mini",
    "DNSName": "conals-mac-mini.tail1b4c5c.ts.net.",
    "OS": "macOS",
    "UserID": 1234567890,
    "TailscaleIPs": ["100.91.70.66", "fd7a:115c:a1e0::f03b:4643"],
    "Addrs": ["192.168.1.20:41641"],
    "Online": true,
    "ExitNode": false,
    "Active": false,
    "Capabilities": ["https", "funnel"]
  },
  "Health": [],
  "MagicDNSSuffix": "tail1b4c5c.ts.net",
  "CurrentTailnet": {"Name": "conal@example.com", "MagicDNSSuffix": "tail1b4c5c.ts.net", "MagicDNSEnabled": true},
  "CertDomains": ["conals-mac-mini.tail1b4c5c.ts.net"],
  "Peer": {
    "nodekey:2222222222222222222222222222222222222222222222222222222222222222": {
      "ID": "nPeer2CNTRL",
      "HostName": "studio",
      "DNSName": "studio.tail1b4c5c.ts.net.",
      "OS": "linux",
      "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::6565:6667"],
      "Online": true,
      "LastSeen": "0001-01-01T00:00:00Z",
      "Active": true
    },
    "nodekey:3333333333333333333333333333333333333333333333333333333333333333": {
      "ID": "nPeer3CNTRL",
      "HostName": "DESKTOP-FUFM1L7",
      "DNSName": "desktop-fufm1l7.tail1b4c5c.ts.net.",
      "OS": "windows",
      "TailscaleIPs": ["100.86.29.60", "fd7a:115c:a1e0::a03b:1d3d"],
      "Online": false,
      "LastSeen": "2026-09-30T08:12:44Z"
    },
    "nodekey:4444444444444444444444444444444444444444444444444444444444444444": {
      "ID": "nPeer4CNTRL",
      "HostName": "Reception iMac",
      "DNSName": "reception-imac.tail1b4c5c.ts.net.",
      "OS": "macOS",
      "TailscaleIPs": ["fd7a:115c:a1e0::453b:7702"],
      "Online": true
    },
    "nodekey:5555555555555555555555555555555555555555555555555555555555555555": {
      "ID": "nPeer5CNTRL",
      "HostName": "OnePlus 8",
      "DNSName": "oneplus-8.tail1b4c5c.ts.net.",
      "OS": "android",
      "TailscaleIPs": ["100.66.34.126", "fd7a:115c:a1e0::103b:227f"],
      "Online": true
    }
  },
  "User": {"1234567890": {"ID": 1234567890, "LoginName": "conal@example.com", "DisplayName": "Conal"}},
  "ClientVersion": null
}
"""#

struct FakeRunner: CommandRunning {
    var output: CommandOutput
    func run(_ executable: String, arguments: [String], timeout: Duration) async throws -> CommandOutput {
        #expect(arguments == ["status", "--json"])
        return output
    }
}

/// Answers by URL; anything unlisted hangs until cancelled (an unreachable peer). Records the
/// highest number of requests in flight at once.
final class FakeTransport: HTTPTransport {
    enum Answer: Sendable {
        case json(Int, String, headers: [String: String] = [:])
        case refuse
    }

    let answers: [String: Answer]
    let delay: Duration
    private let state = Mutex<(inFlight: Int, peak: Int, requests: [URLRequest])>((0, 0, []))

    init(_ answers: [String: Answer], delay: Duration = .zero) {
        self.answers = answers
        self.delay = delay
    }

    var peak: Int { state.withLock { $0.peak } }
    var requests: [URLRequest] { state.withLock { $0.requests } }

    func data(for request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        state.withLock {
            $0.inFlight += 1
            $0.peak = max($0.peak, $0.inFlight)
            $0.requests.append(request)
        }
        defer { state.withLock { $0.inFlight -= 1 } }
        let url = request.url!.absoluteString
        if delay > .zero { try await Task.sleep(for: delay) }
        switch answers[url] {
        case .json(let status, let body, let headers)?:
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
            return (Data(body.utf8), response)
        case .refuse?:
            throw URLError(.cannotConnectToHost)
        case nil:
            try await Task.sleep(for: .seconds(60))
            throw URLError(.timedOut)
        }
    }
}

func helloJSON(_ name: String, auth: String = "password", service: String = "fairspoken-host") -> FakeTransport.Answer {
    .json(200, #"{"service":"\#(service)","protocol":1,"name":"\#(name)","serverVersion":"0.4.0","auth":"\#(auth)"}"#)
}

func discovery(_ transport: FakeTransport, status: String = tailscaleStatusFixture, stderr: String = "", exit: Int32 = 0,
               installed: Set<String> = ["/Applications/Tailscale.app/Contents/MacOS/Tailscale"], path: String? = "/usr/bin:/bin",
               timeout: Duration = .milliseconds(300), localPorts: [Int] = [RemoteProtocol.defaultPort]) -> TailnetDiscovery {
    TailnetDiscovery(runner: FakeRunner(output: CommandOutput(status: exit, stdout: Data(status.utf8), stderr: stderr)),
                     transport: transport, searchPath: path, isExecutable: { installed.contains($0) },
                     probeTimeout: timeout, localPorts: localPorts)
}

// MARK: - Tests

@Suite("Tailscale status")
struct TailscaleStatusTests {
    @Test func decodesSelfAndPeers() throws {
        let status = try JSONDecoder().decode(TailscaleStatus.self, from: Data(tailscaleStatusFixture.utf8))
        #expect(status.backendState == "Running")
        #expect(status.selfNode?.hostName == "Conal’s Mac mini")
        #expect(status.selfNode?.machineName == "conals-mac-mini")
        #expect(status.selfNode?.fqdn == "conals-mac-mini.tail1b4c5c.ts.net")
        #expect(status.peers.map(\.machineName) == ["desktop-fufm1l7", "oneplus-8", "reception-imac", "studio"])
        let reception = try #require(status.peers.first { $0.machineName == "reception-imac" })
        #expect(reception.firstIPv4 == nil)
        #expect(status.fqdn(forMachine: "studio") == "studio.tail1b4c5c.ts.net")
        #expect(status.fqdn(forMachine: "Reception iMac") == "reception-imac.tail1b4c5c.ts.net")
    }

    @Test func toleratesNullPeerAndMissingFields() throws {
        let json = #"{"BackendState":"Running","Self":{"HostName":"solo","DNSName":"","TailscaleIPs":null},"Peer":null}"#
        let status = try JSONDecoder().decode(TailscaleStatus.self, from: Data(json.utf8))
        #expect(status.peers.isEmpty)
        #expect(status.selfNode?.fqdn == nil)
        #expect(status.selfNode?.machineName == "solo")
        #expect(TailnetDiscovery.candidates(from: status).isEmpty)
    }

    @Test func candidatesSkipOfflinePeersAndPhonesAndPreferHTTPS() throws {
        let status = try JSONDecoder().decode(TailscaleStatus.self, from: Data(tailscaleStatusFixture.utf8))
        let candidates = TailnetDiscovery.candidates(from: status)
        #expect(candidates.map(\.machine) == ["conals-mac-mini", "reception-imac", "studio"])
        #expect(candidates[0].isThisMac)
        #expect(candidates[0].urls.map(\.absoluteString) == ["https://conals-mac-mini.tail1b4c5c.ts.net", "http://conals-mac-mini.tail1b4c5c.ts.net", "http://100.91.70.66:48173"])
        #expect(candidates[1].urls.map(\.absoluteString) == ["https://reception-imac.tail1b4c5c.ts.net", "http://reception-imac.tail1b4c5c.ts.net"])  // no IPv4
        #expect(candidates[2].urls.map(\.absoluteString) == ["https://studio.tail1b4c5c.ts.net", "http://studio.tail1b4c5c.ts.net", "http://100.101.102.103:48173"])
        // Every candidate URL is one the app's URL policy accepts.
        for url in candidates.flatMap(\.urls) { #expect(throws: Never.self) { try RemoteURLPolicy.validateBaseURL(url.absoluteString) } }
    }

    @Test func findsTheCLIOnPathThenInTheApp() {
        let t = FakeTransport([:])
        #expect(discovery(t, installed: ["/opt/homebrew/bin/tailscale", "/usr/local/bin/tailscale"]).locateCLI() == "/usr/local/bin/tailscale")
        #expect(discovery(t, installed: ["/opt/homebrew/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"]).locateCLI()
                == "/Applications/Tailscale.app/Contents/MacOS/Tailscale")
        #expect(discovery(t, installed: ["/Users/me/bin/tailscale", "/opt/homebrew/bin/tailscale"], path: "/Users/me/bin:/usr/bin").locateCLI()
                == "/Users/me/bin/tailscale")
        #expect(discovery(t, installed: [], path: nil).locateCLI() == nil)
    }

    @Test func explainsWhyTailscaleCantBeUsed() async {
        let t = FakeTransport([:])
        await #expect(throws: TailnetDiscoveryError.tailscaleNotInstalled) { try await discovery(t, installed: []).discover() }
        await #expect(throws: TailnetDiscoveryError.tailscaleLoggedOut) {
            try await discovery(t, status: #"{"BackendState":"NeedsLogin","Self":null,"Peer":null}"#).discover()
        }
        await #expect(throws: TailnetDiscoveryError.tailscaleNotRunning) {
            try await discovery(t, status: #"{"BackendState":"Stopped","Peer":null}"#).discover()
        }
        await #expect(throws: TailnetDiscoveryError.tailscaleNeedsApproval) {
            try await discovery(t, status: #"{"BackendState":"NeedsMachineAuth"}"#).discover()
        }
        await #expect(throws: TailnetDiscoveryError.tailscaleNotRunning) {
            try await discovery(t, status: "", stderr: "failed to connect to local tailscaled; it doesn't appear to be running\n", exit: 1).discover()
        }
        await #expect(throws: TailnetDiscoveryError.tailscaleLoggedOut) {
            try await discovery(t, status: "", stderr: "Logged out.\n", exit: 1).discover()
        }
        await #expect(throws: TailnetDiscoveryError.tailscaleFailed("something odd")) {
            try await discovery(t, status: "", stderr: "something odd\nmore", exit: 1).discover()
        }
    }
}

@Suite("Tailnet discovery")
struct TailnetDiscoveryTests {
    @Test func listsEachHostOncePreferringHTTPS() async throws {
        let t = FakeTransport([
            // This Mac answers on both: HTTPS wins.
            "https://conals-mac-mini.tail1b4c5c.ts.net/v1/hello": helloJSON("Conal’s Mac mini", auth: "none"),
            "http://100.91.70.66:48173/v1/hello": helloJSON("Conal’s Mac mini", auth: "none"),
            // studio: HTTPS refused, HTTP answers.
            "https://studio.tail1b4c5c.ts.net/v1/hello": .refuse,
            "http://100.101.102.103:48173/v1/hello": helloJSON("Studio", auth: "password"),
            // reception: something else on 443.
            "https://reception-imac.tail1b4c5c.ts.net/v1/hello": helloJSON("Printer", service: "not-fairspoken"),
        ])
        let start = ContinuousClock.now
        let hosts = try await discovery(t).discover()
        #expect(ContinuousClock.now - start < .seconds(2))  // the silent probes time out
        #expect(hosts == [
            DiscoveredHost(name: "Conal’s Mac mini", machine: "conals-mac-mini", url: URL(string: "https://conals-mac-mini.tail1b4c5c.ts.net")!,
                           auth: .open, serverVersion: "0.4.0", isThisMac: true),
            DiscoveredHost(name: "Studio", machine: "studio", url: URL(string: "http://100.101.102.103:48173")!,
                           auth: .password, serverVersion: "0.4.0"),
        ])
        // Offline and Android peers are never probed.
        let probed = Set(t.requests.compactMap { $0.url?.host() })
        #expect(!probed.contains("100.86.29.60") && !probed.contains("100.66.34.126"))
    }

    /// A server on this Mac turns up on loopback (on its own port too), ahead of the tailnet,
    /// and replaces this Mac's tailnet-IP entry when it is the same host.
    @Test func findsHostsOnThisMacOverLoopback() async throws {
        let t = FakeTransport([
            "http://127.0.0.1:48173/v1/hello": helloJSON("Conal’s Mac mini", auth: "password"),
            "http://100.91.70.66:48173/v1/hello": helloJSON("Conal’s Mac mini", auth: "password"),
            "http://127.0.0.1:48174/v1/hello": helloJSON("Fairspoken Server", auth: "password"),
            "http://100.101.102.103:48173/v1/hello": helloJSON("Studio", auth: "password"),
        ])
        let hosts = try await discovery(t, localPorts: [48174, 48173]).discover()
        #expect(hosts.map(\.url.absoluteString) == ["http://127.0.0.1:48174", "http://127.0.0.1:48173", "http://100.101.102.103:48173"])
        #expect(hosts.map(\.isThisMac) == [true, true, false])
        #expect(hosts[0].machine == "conals-mac-mini")
    }

    @Test func listsThisMacsHostWhenTailscaleIsDown() async throws {
        let t = FakeTransport(["http://127.0.0.1:48173/v1/hello": helloJSON("Here", auth: "none")])
        let hosts = try await discovery(t, installed: []).discover()
        #expect(hosts == [DiscoveredHost(name: "Here", machine: "this-mac", url: URL(string: "http://127.0.0.1:48173")!,
                                         auth: .open, serverVersion: "0.4.0", isThisMac: true)])
    }

    @Test func readsFairspokenServersPort() throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: home) }
        #expect(TailnetDiscovery.localHostPorts(home: home) == [48173])
        let dir = home.appendingPathComponent("Library/Application Support/ie.fairspoken.server", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try Data(#"{"bindAddress":"127.0.0.1","port":48174}"#.utf8).write(to: dir.appendingPathComponent("host-config.json"))
        #expect(TailnetDiscovery.localHostPorts(home: home) == [48174, 48173])
    }

    @Test func ignoresNon200AndUndecodableHellos() async throws {
        let t = FakeTransport([
            "https://studio.tail1b4c5c.ts.net/v1/hello": .json(404, #"{"error":"not found"}"#),
            "http://100.101.102.103:48173/v1/hello": .json(200, "<html>dashboard</html>"),
        ])
        #expect(try await discovery(t).discover().isEmpty)
    }

    @Test func keepsAtMost32ProbesInFlight() async {
        let nodes = (1...40).map { i in
            TailnetDiscovery.Candidate(machine: "m\(i)", urls: [URL(string: "https://m\(i).tail1b4c5c.ts.net")!,
                                                                URL(string: "http://100.100.0.\(i):48173")!], isThisMac: false)
        }
        var answers: [String: FakeTransport.Answer] = [:]
        for i in 1...40 { answers["http://100.100.0.\(i):48173/v1/hello"] = helloJSON("Host \(i)") }
        for i in 1...40 { answers["https://m\(i).tail1b4c5c.ts.net/v1/hello"] = .refuse }
        let t = FakeTransport(answers, delay: .milliseconds(20))
        let hosts = await discovery(t).probe(nodes)
        #expect(hosts.count == 40)
        #expect(hosts.map(\.machine) == nodes.map(\.machine))
        #expect(t.peak <= 32)
        #expect(t.peak > 1)
    }

    @Test func probesATypedNameExpandedThroughMagicDNS() async throws {
        let t = FakeTransport(["https://studio.tail1b4c5c.ts.net/v1/hello": helloJSON("Studio", auth: "token")])
        let host = try await discovery(t).probe(entry: " studio ")
        #expect(host.url.absoluteString == "https://studio.tail1b4c5c.ts.net")
        #expect(host.machine == "studio")
        #expect(host.auth == .token)
    }

    @Test func probesATypedNameWithoutTheCLI() async throws {
        let t = FakeTransport(["http://studio:48200/v1/hello": helloJSON("Studio")])
        let host = try await discovery(t, installed: []).probe(entry: "studio:48200")
        #expect(host.url.absoluteString == "http://studio:48200")
        await #expect(throws: TailnetDiscoveryError.noHostAnswered("nobody")) {
            try await discovery(t, installed: []).probe(entry: "nobody")
        }
    }

    @Test func turnsTypedEntriesIntoURLs() throws {
        func urls(_ s: String, _ expand: @escaping (String) -> String? = { _ in nil }) throws -> [String] {
            try TailnetDiscovery.manualURLs(s, expand: expand).map(\.absoluteString)
        }
        #expect(try urls("studio") == ["https://studio", "http://studio:48173"])
        #expect(try urls("studio", { $0 == "studio" ? "studio.tail1b4c5c.ts.net" : nil })
                == ["https://studio.tail1b4c5c.ts.net", "http://studio.tail1b4c5c.ts.net:48173"])
        #expect(try urls("studio:8443") == ["https://studio:8443", "http://studio:8443"])
        #expect(try urls("100.101.102.103") == ["https://100.101.102.103", "http://100.101.102.103:48173"])
        #expect(try urls("[fd7a:115c:a1e0::1]:48200") == ["https://[fd7a:115c:a1e0::1]:48200", "http://[fd7a:115c:a1e0::1]:48200"])
        #expect(try urls("http://studio:48173/v1/health") == ["http://studio:48173"])
        // Plain HTTP to a public name is dropped (URL policy); HTTPS stays.
        #expect(try urls("host.example.com:8080") == ["https://host.example.com:8080"])
        #expect(throws: TailnetDiscoveryError.emptyEntry) { try TailnetDiscovery.manualURLs("  ") }
        #expect(throws: TailnetDiscoveryError.invalidEntry("studio:99999")) { try TailnetDiscovery.manualURLs("studio:99999") }
        #expect(throws: TailnetDiscoveryError.invalidEntry("stu dio")) { try TailnetDiscovery.manualURLs("stu dio") }
        #expect(throws: TailnetDiscoveryError.invalidEntry("http://example.com")) { try TailnetDiscovery.manualURLs("http://example.com") }
        #expect(TailnetDiscovery.manualMachineName("studio.tail1b4c5c.ts.net:443") == "studio")
        #expect(TailnetDiscovery.manualMachineName("100.101.102.103") == "100.101.102.103")
    }
}

@Suite("Hello and pairing")
struct HostPairingTests {
    @Test func decodesHello() throws {
        let json = #"{"service":"fairspoken-host","protocol":1,"name":"Studio Mac","serverVersion":"0.4.0","auth":"password"}"#
        let hello = try JSONDecoder().decode(HostHello.self, from: Data(json.utf8))
        #expect(hello == HostHello(name: "Studio Mac", serverVersion: "0.4.0", auth: .password))
        #expect(hello.isFairspokenHost)
        let open = try JSONDecoder().decode(HostHello.self, from: Data(#"{"service":"fairspoken-host","protocol":1,"name":"A","auth":"none"}"#.utf8))
        #expect(open.auth == .open)
        #expect(open.serverVersion == nil)
        let future = try JSONDecoder().decode(HostHello.self, from: Data(#"{"service":"fairspoken-host","protocol":1,"name":"A","auth":"passkey"}"#.utf8))
        #expect(future.auth == .token)
    }

    @Test func pairsAndSendsTheBody() async throws {
        let t = FakeTransport(["http://100.101.102.103:48173/v1/pair": .json(200, #"{"token":"abc_DEF-123","name":"Studio"}"#)])
        let longName = String(repeating: "n", count: 80)
        let response = try await HostPairingClient(transport: t).pair(url: URL(string: "http://100.101.102.103:48173")!,
                                                                      password: " 123456 ", clientName: "  \(longName)  ")
        #expect(response == PairResponse(token: "abc_DEF-123", name: "Studio"))
        let request = try #require(t.requests.first)
        #expect(request.httpMethod == "POST")
        #expect(request.value(forHTTPHeaderField: "Content-Type") == "application/json")
        let body = try JSONDecoder().decode(PairRequest.self, from: try #require(request.httpBody))
        #expect(body.password == " 123456 ")  // passwords are sent as typed
        #expect(body.clientName == String(repeating: "n", count: 64))
        // The body has exactly the two fields the host accepts (deny_unknown_fields).
        let keys = try #require(try JSONSerialization.jsonObject(with: request.httpBody!) as? [String: Any]).keys
        #expect(Set(keys) == ["password", "clientName"])
    }

    @Test func omitsAnEmptyClientName() throws {
        let data = try JSONEncoder().encode(PairRequest(password: "secret1", clientName: "   "))
        #expect(String(decoding: data, as: UTF8.self) == #"{"password":"secret1"}"#)
    }

    @Test func interpretsEveryStatus() throws {
        func answer(_ status: Int, _ body: String, retryAfter: String? = nil) throws(PairError) -> PairResponse {
            try HostPairingClient.interpret(status: status, body: Data(body.utf8), retryAfterHeader: retryAfter)
        }
        #expect(try answer(200, #"{"token":null,"name":"Studio"}"#) == PairResponse(token: nil, name: "Studio"))
        #expect(throws: PairError.wrongPassword) { try answer(401, #"{"error":"wrong password"}"#) }
        #expect(throws: PairError.pairingDisabled) { try answer(404, #"{"error":"pairing disabled"}"#) }
        #expect(throws: PairError.rateLimited(retryAfterSeconds: 240)) {
            try answer(429, #"{"error":"too many attempts","retryAfterSeconds":240}"#, retryAfter: "999")
        }
        #expect(throws: PairError.rateLimited(retryAfterSeconds: 30)) { try answer(429, "", retryAfter: "30") }
        #expect(throws: PairError.badRequest("unknown field `pin`")) { try answer(400, #"{"error":"unknown field `pin`"}"#) }
        #expect(throws: PairError.unexpected("Remote transcription host returned an error (500 Internal Server Error): boom")) {
            try answer(500, #"{"error":"boom"}"#)
        }
        #expect(throws: PairError.unexpected("The host's answer wasn't understood.")) { try answer(200, "ok") }
    }

    @Test func wordsTheRetryTime() {
        #expect(PairError.rateLimited(retryAfterSeconds: 240).errorDescription == "Too many attempts. Try again in 4 minutes.")
        #expect(PairError.rateLimited(retryAfterSeconds: 61).errorDescription == "Too many attempts. Try again in 2 minutes.")
        #expect(PairError.rateLimited(retryAfterSeconds: 60).errorDescription == "Too many attempts. Try again in 1 minute.")
        #expect(PairError.rateLimited(retryAfterSeconds: 45).errorDescription == "Too many attempts. Try again in 45 seconds.")
        #expect(PairError.rateLimited(retryAfterSeconds: nil).errorDescription == "Too many attempts. Try again in a few minutes.")
    }

    @Test func refusesURLsThePolicyRejects() async {
        let t = FakeTransport([:])
        await #expect(throws: RemoteURLPolicy.ValidationError.requiresHTTPS) {
            try await HostPairingClient(transport: t).pair(url: URL(string: "http://example.com")!, password: "secret1", clientName: "Mac")
        }
        #expect(t.requests.isEmpty)
    }
}
