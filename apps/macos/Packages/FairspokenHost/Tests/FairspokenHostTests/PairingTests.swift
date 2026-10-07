import Foundation
import FairspokenCore
import Testing
@testable import FairspokenHost

@Suite("Discovery and pairing", .serialized)
struct PairingTests {
    static let password = "123456"

    func pairBody(_ password: String, clientName: String? = nil) -> [UInt8] {
        var fields: [String: Any] = ["password": password]
        if let clientName { fields["clientName"] = clientName }
        return [UInt8](try! JSONSerialization.data(withJSONObject: fields))
    }

    // MARK: Hello

    @Test func helloNeedsNoTokenAndRevealsOnlyItsIdentity() async {
        let h = Harness(pairingPassword: Self.password, displayName: "Studio Mac")
        let hello = await h.request("GET", "/v1/hello", auth: false)
        #expect(hello.status == 200)
        #expect(hello.body == #"{"service":"fairspoken-host","protocol":1,"name":"Studio Mac","serverVersion":"9.9.9-test","auth":"password"}"#)
        #expect(await Harness().request("GET", "/v1/hello", auth: false).json["auth"] as? String == "token")
        let open = await Harness(token: nil).request("GET", "/v1/hello", auth: false).json
        #expect(open["auth"] as? String == "none")
        // Without an operator-set name, the Mac's own name.
        #expect((open["name"] as? String)?.isEmpty == false)
        // Only GET is exempt from the token.
        #expect(await h.request("POST", "/v1/hello", auth: false).status == 401)
        #expect(await h.request("GET", "/v1/pair", auth: false).status == 401)
    }

    // MARK: Pair

    @Test func pairingAnswersEachHostState() async throws {
        let h = Harness(pairingPassword: Self.password, displayName: "Studio Mac")
        let events = FrameCollector(h.runtime.observe().frames)
        let ok = await h.request("POST", "/v1/pair", headers: ["X-Forwarded-For": "100.64.0.7"],
                                 body: pairBody(Self.password, clientName: "  Conal's MacBook \u{7}"), auth: false)
        #expect(ok.status == 200)
        #expect(ok.body == #"{"token":"secret","name":"Studio Mac"}"#)
        let wrong = await h.request("POST", "/v1/pair", body: pairBody("654321"), auth: false)
        #expect(wrong.status == 401 && wrong.body == #"{"error":"wrong password"}"#)

        #expect(await events.wait(for: "pairing", count: 2))
        let first = events.all[0].json
        #expect(first["client"] as? String == "100.64.0.7")
        #expect(first["clientName"] as? String == "Conal's MacBook")
        #expect(first["ok"] as? Bool == true)
        #expect((first["at"] as? Int ?? 0) > 1_700_000_000_000)
        #expect(Set(first.keys) == ["at", "client", "clientName", "ok"])
        #expect(events.all[1].json["ok"] as? Bool == false && events.all[1].json["clientName"] is NSNull)
        let attempts = h.runtime.pairingAttempts()
        #expect(attempts.map(\.outcome) == [.wrongPassword, .paired])

        let disabled = await Harness().request("POST", "/v1/pair", body: pairBody(Self.password), auth: false)
        #expect(disabled.status == 404 && disabled.body == #"{"error":"pairing disabled"}"#)
        let open = await Harness(token: nil, displayName: "Open Mac").request("POST", "/v1/pair", body: pairBody("anything"), auth: false)
        #expect(open.status == 200 && open.body == #"{"token":null,"name":"Open Mac"}"#)
    }

    @Test func pairingFrameMatchesTheRustWireFormat() {
        #expect(HostWireEvent.pairing(client: "100.64.0.7", clientName: "Conal's MacBook", ok: true).frame(at: 3)
            == "event: pairing\ndata: {\"at\":3,\"client\":\"100.64.0.7\",\"clientName\":\"Conal's MacBook\",\"ok\":true}\n\n")
    }

    @Test func malformedPairRequestsAreRejected() async {
        let h = Harness(pairingPassword: Self.password)
        for body in ["", "nope", "[]", #"{"clientName":"x"}"#, #"{"password":123456}"#, #"{"password":"123456","extra":1}"#,
                     #"{"password":"123456","clientName":5}"#] {
            let r = await h.request("POST", "/v1/pair", body: Array(body.utf8), auth: false)
            #expect(r.status == 400, "\(body)")
            #expect((r.json["error"] as? String)?.isEmpty == false)
        }
        // A malformed request is not an attempt.
        #expect(h.runtime.pairingAttempts().isEmpty)
        #expect(await h.request("POST", "/v1/pair", body: Array(#"{"password":"123456","clientName":null}"#.utf8), auth: false).status == 200)
    }

    @Test func clientNamesAreTidied() throws {
        func name(_ raw: String) throws -> String? {
            let body = [UInt8](try JSONSerialization.data(withJSONObject: ["password": "x", "clientName": raw]))
            return try HostRouter.parsePairRequest(body).get().clientName
        }
        #expect(try name("  Conal's MacBook  ") == "Conal's MacBook")
        #expect(try name(" \n ") == nil)
        #expect(try name(String(repeating: "é", count: 80))?.unicodeScalars.count == 64)
        #expect(try name("a\u{0}b\u{1b}c") == "abc")
    }

    // MARK: Rate limit

    @Test func fiveFailuresBlockAnAddressEvenForTheRightPassword() async {
        let clock = ManualClock()
        let h = Harness(pairingPassword: Self.password, pairingLimiter: PairingLimiter(now: { clock.now }))
        func attempt(_ password: String, from address: String) async -> ParsedResponse {
            await h.request("POST", "/v1/pair", body: pairBody(password), auth: false, peer: address)
        }
        for _ in 0..<5 {
            #expect(await attempt("nope!!", from: "100.64.0.9").status == 401)
            clock.advance(.seconds(10))
        }
        clock.advance(.seconds(10)) // the first failure was 60 s ago
        let limited = await attempt(Self.password, from: "100.64.0.9")
        #expect(limited.status == 429)
        #expect(limited.json["error"] as? String == "too many attempts")
        #expect(limited.json["retryAfterSeconds"] as? Int == 540)
        #expect(limited.headers["retry-after"] == "540")
        // Another address is unaffected.
        #expect(await attempt(Self.password, from: "100.64.0.10").status == 200)
        // Refused attempts don't extend the wait; once the oldest failure ages out there is room.
        clock.advance(.seconds(539))
        #expect(await attempt(Self.password, from: "100.64.0.9").status == 429)
        clock.advance(.seconds(1))
        #expect(await attempt(Self.password, from: "100.64.0.9").status == 200)
        #expect(h.runtime.pairingAttempts().first?.outcome == .paired)
        #expect(h.runtime.pairingAttempts().contains { $0.outcome == .limited })
    }

    @Test func twentyFailuresAcrossAddressesBlockEveryone() {
        let clock = ManualClock()
        let limiter = PairingLimiter(now: { clock.now })
        for i in 0..<20 {
            #expect(limiter.attempt(address: "100.64.1.\(i)", { false }) == .mismatched)
            clock.advance(.seconds(1))
        }
        clock.advance(.seconds(10))
        // The first failure was 30 s ago.
        #expect(limiter.attempt(address: "100.64.2.1", { true }) == .limited(retryAfterSeconds: 570))
        clock.advance(.seconds(570))
        #expect(limiter.attempt(address: "100.64.2.1", { true }) == .matched)
        // Successes never count.
        for _ in 0..<30 { #expect(limiter.attempt(address: "100.64.2.2", { true }) == .matched) }
    }

    @Test func comparisonUsesTheWholePassword() {
        let c = HostCredentials(token: "t", pairingPassword: "123456")
        #expect(c.passwordMatches("123456"))
        #expect(!c.passwordMatches("12345"))
        #expect(!c.passwordMatches("1234567"))
        #expect(!c.passwordMatches(""))
        #expect(!HostCredentials(token: "t", pairingPassword: nil).passwordMatches(""))
    }

    // MARK: Configuration

    @Test func configSetsAndClearsThePasswordWithoutEverEchoingIt() async throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("host-config-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: url) }
        let h = Harness(configURL: url)
        let secret = "correct horse"
        for bad in [#"{"pairingPassword":"12345"}"#, #"{"pairingPassword":5}"#, "{\"pairingPassword\":\"\(String(repeating: "x", count: 129))\"}",
                    "{\"maxActiveStreams\":0,\"pairingPassword\":\"\(secret)\"}"] {
            let r = await h.request("POST", "/v1/config", body: Array(bad.utf8))
            #expect(r.status == 400, "\(bad)")
            #expect(!r.body.contains(secret))
        }
        #expect(!h.runtime.pairingEnabled)
        #expect(h.runtime.liveSettings.maxActiveStreams == 4)

        let set = await h.request("POST", "/v1/config", body: Array("{\"pairingPassword\":\"\(secret)\"}".utf8))
        #expect(set.status == 200)
        #expect(set.json["pairingEnabled"] as? Bool == true)
        #expect(!set.body.contains(secret))
        let stats = await h.request("GET", "/v1/stats")
        #expect(stats.json["pairingEnabled"] as? Bool == true)
        #expect(!stats.body.contains(secret))
        #expect(try HostConfigurationStore.load(url)?.pairingPassword == secret)
        #expect(await h.request("POST", "/v1/pair", body: pairBody(secret), auth: false).status == 200)
        // 128 characters counts Unicode scalars, as the Rust host counts chars.
        #expect(await h.request("POST", "/v1/config", body: Array("{\"pairingPassword\":\"\(String(repeating: "é", count: 128))\"}".utf8)).status == 200)

        for off in [#"{"pairingPassword":null}"#, #"{"pairingPassword":""}"#] {
            let r = await h.request("POST", "/v1/config", body: Array(off.utf8))
            #expect(r.status == 200 && r.json["pairingEnabled"] as? Bool == false, "\(off)")
            #expect(try HostConfigurationStore.load(url)?.pairingPassword == "")
        }
        #expect(await h.request("POST", "/v1/pair", body: pairBody(secret), auth: false).status == 404)
        // An empty password is left out of the file.
        #expect(!(try String(contentsOf: url, encoding: .utf8)).contains("pairingPassword"))
    }

    @Test func aPasswordOnAnOpenHostGeneratesAndEnforcesAToken() async throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("host-config-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: url) }
        let h = Harness(token: nil, configURL: url)
        #expect(await h.request("POST", "/v1/config", body: Array(#"{"pairingPassword":"123456"}"#.utf8), auth: false).status == 200)
        let token = try #require(h.runtime.authToken)
        #expect(token.count == 43 && token.allSatisfy { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" })
        #expect(try HostConfigurationStore.load(url)?.token == token)
        #expect(await h.request("GET", "/v1/hello", auth: false).json["auth"] as? String == "password")
        #expect(await h.request("GET", "/v1/stats", auth: false).status == 401)
        let paired = await h.request("POST", "/v1/pair", body: pairBody("123456"), auth: false)
        #expect(paired.json["token"] as? String == token)
        #expect(await h.request("GET", "/v1/stats", headers: ["Authorization": "Bearer \(token)"], auth: false).status == 200)
    }

    @Test func startupWithAPasswordAndNoTokenSavesOne() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("host-config-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: url) }
        try HostConfigurationStore.save(HostConfiguration(), to: url)
        var config = HostConfiguration()
        config.pairingPassword = "123456"
        let runtime = HostRuntime(configuration: config, configURL: url, backend: StubSpeechBackend(), serverVersion: "t")
        let token = try #require(runtime.authToken)
        let saved = try #require(try HostConfigurationStore.load(url))
        #expect(saved.token == token)
        // Only the token is written: the password came from this run's configuration.
        #expect(saved.pairingPassword == "")
    }

    @Test func environmentPasswordAndNameWinAndAreValidated() throws {
        var file = HostConfiguration()
        file.pairingPassword = "from-file"
        file.displayName = "File name"
        let known: Set<String> = [HostConfiguration.defaultModel]
        let env = ["FAIRSPOKEN_HOST_PAIRING_PASSWORD": "from-env", "FAIRSPOKEN_HOST_NAME": "  Env\u{7} name  "]
        let resolved = try HostConfigurationStore.resolve(file: file, environment: env, knownModels: known)
        #expect(resolved.pairingPassword == "from-env" && resolved.displayName == "Env name")
        let legacy = try HostConfigurationStore.resolve(file: nil, environment: ["MULTIVOICE_HOST_PAIRING_PASSWORD": "legacy1"], knownModels: known)
        #expect(legacy.pairingPassword == "legacy1")
        #expect(try HostConfigurationStore.resolve(file: file, environment: ["FAIRSPOKEN_HOST_PAIRING_PASSWORD": ""], knownModels: known)
            .pairingPassword == "from-file")
        #expect(throws: HostConfigurationStore.StoreError.self) {
            try HostConfigurationStore.resolve(file: nil, environment: ["FAIRSPOKEN_HOST_PAIRING_PASSWORD": "short"], knownModels: known)
        }
        var badFile = HostConfiguration()
        badFile.pairingPassword = "12345"
        #expect(throws: HostConfigurationStore.StoreError.self) {
            try HostConfigurationStore.resolve(file: badFile, environment: [:], knownModels: known)
        }
    }

    @Test func theFileUsesTheRustHostsKeys() throws {
        var config = HostConfiguration()
        config.pairingPassword = "123456"
        config.displayName = "Studio Mac"
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(config)) as? [String: Any]
        #expect(json?["pairingPassword"] as? String == "123456")
        #expect(json?["name"] as? String == "Studio Mac")
        let decoded = try JSONDecoder().decode(HostConfiguration.self, from: Data(#"{"pairingPassword":null,"name":"Den"}"#.utf8))
        #expect(decoded.pairingPassword == "" && decoded.displayName == "Den")
        let token = HostConfiguration.generateToken()
        #expect(token.count == 43 && !token.contains("=") && !token.contains("+") && !token.contains("/"))
        #expect(HostConfiguration.pairingPasswordProblem("123456") == nil)
        #expect(HostConfiguration.pairingPasswordProblem("") == nil)
        #expect(HostConfiguration.pairingPasswordProblem("12345") == "pairingPassword must be 6 to 128 characters")
    }
}
