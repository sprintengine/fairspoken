import Foundation
import FairspokenCore
import Testing
@testable import FairspokenHost

@Suite("Host routes", .serialized)
struct RouterTests {
    @Test func authRules() async {
        let h = Harness()
        #expect(await h.request("GET", "/v1/health", auth: false).status == 401)
        #expect(await h.request("GET", "/v1/health", auth: false).body == #"{"error":"unauthorized"}"#)
        #expect(await h.request("GET", "/v1/health", headers: ["Authorization": "Bearer secreT"], auth: false).status == 401)
        #expect(await h.request("GET", "/v1/health?token=secret", auth: false).status == 200)
        #expect(await h.request("GET", "/v1/stats?token=nope", auth: false).status == 401)
        // ?token= is for GET only: mutating routes need the header.
        #expect(await h.request("POST", "/v1/config?token=secret", body: Array("{}".utf8), auth: false).status == 401)
        #expect(await h.request("GET", "/", auth: false).status == 200)
        #expect(await h.request("GET", "/favicon.ico", auth: false).status == 204)
        #expect(await h.request("GET", "/nope", auth: false).status == 401)
        let missing = await h.request("GET", "/nope")
        #expect(missing.status == 404 && missing.body == #"{"error":"not_found"}"#)
        #expect(await h.request("POST", "/v1/health").status == 404)
        let open = Harness(token: nil)
        #expect(await open.request("GET", "/v1/health", auth: false).status == 200)
    }

    @Test func healthAndStatsShape() async throws {
        let h = Harness(workers: 2)
        await h.startWorkers()
        let health = await h.request("GET", "/v1/health")
        #expect(health.body == #"{"ok":true,"mode":"standalone-host","backend":"parakeet","serverVersion":"9.9.9-test"}"#)
        #expect(health.headers["content-type"] == "application/json")
        let stats = await h.request("GET", "/v1/stats").json
        #expect(stats["workerCount"] as? Int == 2)
        #expect(stats["model"] as? String == "parakeet-tdt-0.6b-v3")
        #expect(stats["modelDownload"] is NSNull)
        let models = try #require(stats["models"] as? [[String: Any]])
        #expect(models.map { $0["id"] as? String } == ["parakeet-tdt-0.6b-v3", "parakeet-ultra"])
        #expect(models[0]["assignedWorkers"] as? [Int] == [0, 1])
        let workers = try #require(stats["workers"] as? [[String: Any]])
        #expect(workers.map { $0["state"] as? String } == ["idle", "idle"])
        #expect(workers[0]["loadedModel"] as? String == "parakeet-tdt-0.6b-v3")
        #expect(workers[0]["job"] is NSNull)
        // Decodes with the client's lenient model too.
        let decoded = try JSONDecoder().decode(HostStats.self, from: Data(await h.request("GET", "/v1/stats").body.utf8))
        #expect(decoded.workers.count == 2)
        await h.shutdown()
    }

    @Test func configUpdatesAreAllOrNothingAndPersist() async throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("host-config-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: url) }
        let h = Harness(workers: 2, configURL: url)
        let bad = await h.request("POST", "/v1/config", body: Array(#"{"maxActiveStreams":8,"maxRecordingSeconds":5}"#.utf8))
        #expect(bad.status == 400)
        #expect(h.runtime.liveSettings.maxActiveStreams == 4)
        #expect(await h.request("POST", "/v1/config", body: Array(#"{"what":1}"#.utf8)).status == 400)
        #expect(await h.request("POST", "/v1/config", body: Array("nope".utf8)).status == 400)
        let ok = await h.request("POST", "/v1/config", body: Array(#"{"maxActiveStreams":8,"workerModels":["parakeet-ultra","parakeet-tdt-0.6b-v3"]}"#.utf8))
        #expect(ok.status == 200)
        #expect(ok.body == #"{"maxActiveStreams":8,"maxRecordingSeconds":600,"useGpu":true,"model":"mixed","workerModels":["parakeet-ultra","parakeet-tdt-0.6b-v3"],"pairingEnabled":false}"#)
        let saved = try #require(try HostConfigurationStore.load(url))
        #expect(saved.maxActiveStreams == 8 && saved.workerModels == ["parakeet-ultra", "parakeet-tdt-0.6b-v3"])
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        #expect((attributes[.posixPermissions] as? NSNumber)?.intValue == 0o600)
    }

    @Test func batchTranscriptionAndEventOrder() async throws {
        let h = Harness()
        await h.startWorkers()
        let events = FrameCollector(h.runtime.observe().frames)
        let response = await h.request("POST", "/v1/transcriptions", headers: ["x-fairspoken-model": "whatever", "X-Forwarded-For": "100.64.0.7"],
                                       body: TestAudio.wav(seconds: 1.5))
        #expect(response.status == 200)
        let json = response.json
        #expect(json["text"] as? String == "Stub transcript of 1.50 seconds.")
        #expect(json["durationSeconds"] as? Double == 1.5)
        #expect(json["model"] as? String == "parakeet-tdt-0.6b-v3")
        #expect(json["backend"] as? String == "parakeet")
        #expect(await events.wait(for: "worker_state", count: 2))
        #expect(events.types == ["job_queued", "job_started", "worker_state", "job_completed", "worker_state"])
        let queued = events.all[0].json
        #expect(queued["source"] as? String == "batch" && queued["audioSeconds"] as? Double == 1.5 && queued["client"] as? String == "100.64.0.7")
        #expect(events.all[4].json["state"] as? String == "idle")
        #expect(await h.request("POST", "/v1/transcriptions", headers: ["x-fairspoken-backend": "nonsense"], body: TestAudio.wav(seconds: 0.2)).status == 400)
        // The legacy x-multivoice-* headers are still read when the new one is absent.
        #expect(await h.request("POST", "/v1/transcriptions", headers: ["x-multivoice-backend": "nonsense"], body: TestAudio.wav(seconds: 0.2)).status == 400)
        #expect(await h.request("POST", "/v1/transcriptions", headers: ["x-fairspoken-backend": "whisper", "x-multivoice-backend": "nonsense"],
                                body: TestAudio.wav(seconds: 0.2)).status == 200)
        #expect(await h.request("POST", "/v1/transcriptions", body: TestAudio.wav(seconds: 0.2, bits: 8)).status == 400)
        #expect(await h.request("POST", "/v1/transcriptions", body: Array("garbage".utf8)).status == 400)
        _ = try await h.runtime.applyConfigUpdate(HostConfigUpdate(maxRecordingSeconds: 10))
        #expect(await h.request("POST", "/v1/transcriptions", body: TestAudio.wav(seconds: 10.5)).status == 413)
        let stats = await h.request("GET", "/v1/stats").json
        let client = (stats["clients"] as? [[String: Any]])?.first { $0["address"] as? String == "100.64.0.7" }
        #expect(client?["completed"] as? Int == 1)
        await h.shutdown()
    }

    /// Frames over a chunked body, as the clients stream them.
    func streamRequest(_ h: Harness, frames: [[UInt8]], pace: Duration = .milliseconds(2), extraHeaders: String = "",
                       peer: String = "127.0.0.1") async -> (ParsedResponse?, PipeTransport) {
        let (pipe, task) = h.connect(peer: peer)
        pipe.write("POST /v1/transcriptions/stream HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer secret\r\nTransfer-Encoding: chunked\r\nContent-Type: \(StreamFrameCodec.contentType)\r\nConnection: close\r\n\(extraHeaders)\r\n")
        for frame in frames {
            pipe.write(TestAudio.chunked(frame))
            try? await Task.sleep(for: pace)
        }
        pipe.write("0\r\n\r\n")
        await task.value
        return (ParsedResponse.parse(pipe.outputText), pipe)
    }

    func frames(seconds: Double, rate: Int = 16_000, per: Int = 1_600) -> [[UInt8]] {
        let samples = TestAudio.samples(seconds: seconds, rate: rate)
        return stride(from: 0, to: samples.count, by: per).map {
            [UInt8](StreamFrameCodec.encode(sampleRate: UInt32(rate), samples: Array(samples[$0..<min($0 + per, samples.count)])))
        }
    }

    @Test func streamTranscriptionDecodesWhileTheClientSpeaks() async throws {
        let h = Harness()
        await h.startWorkers()
        let events = FrameCollector(h.runtime.observe().frames)
        var input = frames(seconds: 2)
        input.insert([UInt8](StreamFrameCodec.encode(sampleRate: 16_000, samples: [])), at: 3) // a no-op frame
        let (response, _) = await streamRequest(h, frames: input)
        let r = try #require(response)
        #expect(r.status == 200)
        #expect(r.json["durationSeconds"] as? Double == 2.0)
        #expect(await events.wait(for: "worker_state", count: 2))
        #expect(events.types == ["stream_started", "job_queued", "job_started", "worker_state", "stream_finished", "job_completed", "worker_state"])
        #expect(events.all[1].json["audioSeconds"] as? Double == 0 && events.all[1].json["source"] as? String == "stream")
        #expect(events.all[4].json["audioSeconds"] as? Double == 2.0)
        #expect(events.all[5].json["audioSeconds"] as? Double == 2.0)
        await h.shutdown()
    }

    @Test func longStreamsAreDecodedInSegmentsAsTheyArrive() async throws {
        let h = Harness()
        await h.startWorkers()
        let (response, _) = await streamRequest(h, frames: frames(seconds: 30, per: 16_000), pace: .milliseconds(1))
        #expect(response?.status == 200)
        let calls = h.backend.transcribeCalls
        #expect(calls.count == 3) // two 7–12 s segments while streaming, then the tail
        #expect(calls.allSatisfy { $0.samples <= 12 * 16_000 })
        #expect(calls.reduce(0) { $0 + $1.samples } == 30 * 16_000)
        // A 48 kHz stream is resampled.
        let (r48, _) = await streamRequest(h, frames: frames(seconds: 1, rate: 48_000, per: 4_800))
        #expect(r48?.json["durationSeconds"] as? Double == 1.0)
        await h.shutdown()
    }

    @Test func streamErrors() async throws {
        let h = Harness()
        await h.startWorkers()
        let events = FrameCollector(h.runtime.observe().frames)
        // Sample-rate change mid-stream.
        let mixed = [[UInt8](StreamFrameCodec.encode(sampleRate: 16_000, samples: [1, 2, 3])),
                     [UInt8](StreamFrameCodec.encode(sampleRate: 8_000, samples: [1, 2, 3]))]
        let (changed, _) = await streamRequest(h, frames: mixed)
        #expect(changed?.status == 413)
        #expect(changed?.json["error"] as? String == "Remote stream sample rate changed during recording")
        // Only zero-count frames.
        let (empty, _) = await streamRequest(h, frames: [[UInt8](StreamFrameCodec.encode(sampleRate: 16_000, samples: []))])
        #expect(empty?.status == 400)
        #expect(empty?.json["error"] as? String == "No audio samples were captured")
        // Unsupported sample rate.
        let (badRate, _) = await streamRequest(h, frames: [[0, 0, 0, 0, 1, 0, 0, 0, 1, 0]])
        #expect(badRate?.status == 413)
        // Over the maximum length.
        _ = try h.runtime.applyConfigUpdate(HostConfigUpdate(maxRecordingSeconds: 10))
        let (long, _) = await streamRequest(h, frames: frames(seconds: 11, per: 16_000), pace: .zero)
        #expect(long?.status == 413)
        #expect(long?.json["error"] as? String == "Recording exceeds host maximum of 10 seconds")
        // Every job_queued got exactly one terminal event; aborted streams end with "Stream upload aborted".
        try await Task.sleep(for: .milliseconds(200))
        let queued = events.all.filter { $0.type == "job_queued" }.compactMap { $0.json["jobId"] as? Int }
        let terminal = events.all.filter { $0.type == "job_completed" || $0.type == "job_failed" }.compactMap { $0.json["jobId"] as? Int }
        #expect(queued.sorted() == terminal.sorted())
        #expect(events.all.filter { $0.type == "job_failed" }.allSatisfy { $0.json["error"] as? String == "Stream upload aborted" })
        #expect(events.all.filter { $0.type == "stream_finished" }.allSatisfy { $0.json["audioSeconds"] as? Double == 0 })
        await h.shutdown()
    }

    @Test func capacityLimitsAnswer429() async throws {
        let h = Harness(workers: 1, queue: 1, maxStreams: 2, transcribeDelay: .milliseconds(10))
        await h.startWorkers()
        let events = FrameCollector(h.runtime.observe().frames)
        // Two open streams: one holds the worker, one waits in the queue (capacity 1).
        var open: [(PipeTransport, Task<Void, Never>)] = []
        for _ in 0..<2 {
            let (pipe, task) = h.connect()
            pipe.write("POST /v1/transcriptions/stream HTTP/1.1\r\nAuthorization: Bearer secret\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
            pipe.write(TestAudio.chunked([UInt8](StreamFrameCodec.encode(sampleRate: 16_000, samples: TestAudio.samples(seconds: 0.2)))))
            open.append((pipe, task))
        }
        #expect(await events.wait(for: "job_queued", count: 2))
        let full = await h.request("POST", "/v1/transcriptions", body: TestAudio.wav(seconds: 0.5))
        #expect(full.status == 429)
        #expect(full.json["error"] as? String == "Transcription queue is full")
        #expect(await events.wait(for: "job_failed"))
        let failed = try #require(events.all.first { $0.type == "job_failed" })
        #expect(failed.json["worker"] is NSNull)
        // Stream capacity (2): a third stream is refused before it is queued.
        let (third, _) = await streamRequest(h, frames: frames(seconds: 0.2))
        #expect(third?.status == 429)
        #expect(third?.json["error"] as? String == "Server is at active stream capacity")
        // End both uploads before waiting on either: whichever stream claimed the
        // worker must finish first, and that may be open[1], not open[0].
        for (pipe, _) in open {
            pipe.write("0\r\n\r\n")
        }
        for (pipe, task) in open {
            await task.value
            #expect(ParsedResponse.parse(pipe.outputText)?.status == 200)
        }
        let stats = await h.request("GET", "/v1/stats").json
        #expect((stats["rejectedJobs"] as? Int ?? 0) >= 2)
        await h.shutdown()
    }

    @Test func aStreamThatWaitedForAWorkerReportsReleaseToTextFromItsUploadEnd() async throws {
        let h = Harness(workers: 1, queue: 2, transcribeDelay: .milliseconds(5))
        await h.startWorkers()
        let events = FrameCollector(h.runtime.observe().frames)
        // A holds the only worker; B uploads everything and ends while still queued.
        let (a, aTask) = h.connect()
        a.write("POST /v1/transcriptions/stream HTTP/1.1\r\nAuthorization: Bearer secret\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
        a.write(TestAudio.chunked([UInt8](StreamFrameCodec.encode(sampleRate: 16_000, samples: TestAudio.samples(seconds: 0.2)))))
        #expect(await events.wait(for: "job_started"))
        let bTask = Task { await streamRequest(h, frames: frames(seconds: 0.5)) }
        #expect(await events.wait(for: "stream_finished"))
        try await Task.sleep(for: .milliseconds(300))
        a.write("0\r\n\r\n")
        await aTask.value
        _ = await bTask.value
        #expect(await events.wait(for: "job_completed", count: 2))
        let completed = events.all.filter { $0.type == "job_completed" }
        let b = try #require(completed.last)
        // B's release-to-text includes the ~300 ms it waited for the worker after its client let go.
        #expect((b.json["processingMs"] as? Int ?? 0) >= 250)
        var live = HostLiveState()
        live.apply(.snapshot(HostStats()))
        for e in events.all { if let event = try? HostEvent.decode(SSEEvent(type: e.type, data: String(decoding: try JSONSerialization.data(withJSONObject: e.json), as: UTF8.self))) { live.apply(event) } }
        let last = try #require(live.completions.last)
        #expect(last.source == "stream" && last.latencyMs == last.processingMs)
        await h.shutdown()
    }

    @Test func modelDownloadsReportProgressAndRejectConcurrentStarts() async throws {
        let h = Harness()
        h.backend.setInstalled("parakeet-ultra", false)
        h.backend.downloadStepDelay = .milliseconds(30)
        let events = FrameCollector(h.runtime.observe().frames)
        #expect(await h.request("POST", "/v1/models/download", body: Array(#"{"model":"tiny"}"#.utf8)).status == 400)
        #expect(await h.request("POST", "/v1/models/download", body: Array(#"{"model":"parakeet-ultra","x":1}"#.utf8)).status == 400)
        let started = await h.request("POST", "/v1/models/download", body: Array(#"{"model":"parakeet-ultra"}"#.utf8))
        #expect(started.status == 202)
        #expect(started.body == #"{"model":"parakeet-ultra","status":"downloading"}"#)
        #expect(await h.request("POST", "/v1/models/download", body: Array(#"{"model":"parakeet-ultra"}"#.utf8)).status == 409)
        #expect(await events.wait(for: "model_download", count: 7))
        let stages = events.all.filter { $0.type == "model_download" }.compactMap { $0.json["stage"] as? String }
        #expect(stages.first == "starting" && stages.last == "ready" && stages.contains("downloading") && stages.contains("validating"))
        #expect(h.backend.isInstalled("parakeet-ultra"))
    }

    @Test func workersReportAMissingModelAndFailJobsForIt() async throws {
        let h = Harness()
        h.backend.setInstalled("parakeet-tdt-0.6b-v3", false)
        h.runtime.startWorkers()
        try await Task.sleep(for: .milliseconds(100))
        let stats = await h.request("GET", "/v1/stats").json
        let worker = try #require((stats["workers"] as? [[String: Any]])?.first)
        #expect(worker["state"] as? String == "model-unavailable")
        #expect(worker["modelAvailable"] as? Bool == false)
        #expect((worker["lastError"] as? String)?.contains("not installed") == true)
        let r = await h.request("POST", "/v1/transcriptions", body: TestAudio.wav(seconds: 0.3))
        #expect(r.status == 500)
        await h.shutdown()
    }

    @Test func keepAliveAndExpectContinue() async throws {
        let h = Harness()
        await h.startWorkers()
        let (pipe, task) = h.connect()
        pipe.write("GET /v1/health HTTP/1.1\r\nAuthorization: Bearer secret\r\n\r\n")
        #expect(await pipe.waitForOutput(containing: "standalone-host"))
        let wav = TestAudio.wav(seconds: 0.4)
        pipe.write("POST /v1/transcriptions HTTP/1.1\r\nAuthorization: Bearer secret\r\nExpect: 100-continue\r\nContent-Length: \(wav.count)\r\n\r\n")
        #expect(await pipe.waitForOutput(containing: "100 Continue"))
        pipe.write(wav)
        #expect(await pipe.waitForOutput(containing: "Stub transcript"))
        pipe.endInput()
        await task.value
        await h.shutdown()
    }

    @Test func emptyLinesBeforeARequestAreSkippedUpToTheHeadLimit() async throws {
        let h = Harness()
        let (pipe, task) = h.connect()
        for _ in 0..<100 { pipe.write("\r\n\r\n") }
        pipe.write("GET /v1/health HTTP/1.1\r\nAuthorization: Bearer secret\r\n\r\n")
        #expect(await pipe.waitForOutput(containing: "standalone-host"))
        // A peer that sends nothing but empty lines is answered 431 and closed.
        let flood = String(repeating: "\r\n", count: 1_024)
        for _ in 0...(HTTPHeadParser.maxHeadBytes / flood.utf8.count) { pipe.write(flood) }
        await task.value
        #expect(pipe.outputText.contains("HTTP/1.1 431"))
        #expect(pipe.isClosed)
        await h.shutdown()
    }
}

@Suite("Listener", .serialized)
struct ListenerTests {
    func start(heartbeat: Duration = .seconds(15)) async throws -> (TranscriptionHost, StubSpeechBackend) {
        var config = HostConfiguration()
        config.token = "secret"
        config.port = 0
        config.workerCount = 1
        config.workerModels = [HostConfiguration.defaultModel]
        let backend = StubSpeechBackend()
        let host = try await TranscriptionHost.start(configuration: config, configURL: nil, backend: backend,
                                                     dashboardHTML: Array("<html></html>".utf8), serverVersion: "test", heartbeat: heartbeat)
        return (host, backend)
    }

    @Test func servesOverTCPWithURLSession() async throws {
        let (host, _) = try await start()
        var request = URLRequest(url: URL(string: "http://127.0.0.1:\(host.boundPort)/v1/health")!)
        request.setValue("Bearer secret", forHTTPHeaderField: "Authorization")
        let (data, response) = try await URLSession.shared.data(for: request)
        #expect((response as? HTTPURLResponse)?.statusCode == 200)
        let health = try JSONDecoder().decode(RemoteHealth.self, from: data)
        #expect(health.mode == "standalone-host")
        await host.stop()
    }

    @Test func sseHeadersSnapshotHeartbeatAndSubscriberLimit() async throws {
        let (host, _) = try await start(heartbeat: .milliseconds(150))
        let port = host.boundPort
        let first = try await RawClient(port: port)
        first.send("GET /v1/events?token=secret HTTP/1.1\r\nHost: x\r\n\r\n")
        #expect(await first.wait { $0.contains(": ping") })
        let response = try #require(ParsedResponse.parse(first.text))
        #expect(response.status == 200)
        #expect(response.headers["content-type"] == "text/event-stream")
        #expect(response.headers["cache-control"] == "no-cache")
        #expect(response.headers["connection"] == "close")
        #expect(response.headers["x-accel-buffering"] == "no")
        #expect(response.headers["transfer-encoding"] == "chunked")
        #expect(response.body.hasPrefix("event: snapshot\ndata: {\"serverVersion\":\"test\""))

        // HTTP/1.0: close-delimited, not chunked.
        let old = try await RawClient(port: port)
        old.send("GET /v1/events HTTP/1.0\r\nAuthorization: Bearer secret\r\n\r\n")
        #expect(await old.wait { $0.contains("event: snapshot") })
        #expect(!old.text.lowercased().contains("transfer-encoding"))
        #expect(old.text.contains("\r\n\r\nevent: snapshot\ndata: {"))

        var clients = [first, old]
        for _ in 0..<14 {
            let c = try await RawClient(port: port)
            c.send("GET /v1/events?token=secret HTTP/1.1\r\n\r\n")
            #expect(await c.wait { $0.contains("event: snapshot") })
            clients.append(c)
        }
        let seventeenth = try await RawClient(port: port)
        seventeenth.send("GET /v1/events?token=secret HTTP/1.1\r\n\r\n")
        #expect(await seventeenth.wait { $0.contains("Too many event subscribers (limit 16)") })
        #expect(ParsedResponse.parse(seventeenth.text)?.status == 503)

        // A closed client frees its slot promptly.
        clients[5].close()
        let deadline = ContinuousClock.now + .seconds(3)
        while host.runtime.metrics.events.subscriberCount > 15, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(20))
        }
        let again = try await RawClient(port: port)
        again.send("GET /v1/events?token=secret HTTP/1.1\r\n\r\n")
        #expect(await again.wait { $0.contains("event: snapshot") })
        clients.append(again)
        clients.forEach { $0.close() }
        seventeenth.close()
        await host.stop()
    }

    @Test func onlyOneSpecificNonLoopbackAddressNeedsALoopbackCompanion() {
        #expect(HostConfiguration.needsLoopbackCompanion("100.101.102.103"))
        #expect(HostConfiguration.needsLoopbackCompanion("192.168.1.20"))
        #expect(HostConfiguration.needsLoopbackCompanion("fd7a:115c:a1e0::1"))
        for covered in ["127.0.0.1", "127.0.0.2", "::1", "localhost", "0.0.0.0", "::", "*"] {
            #expect(!HostConfiguration.needsLoopbackCompanion(covered))
        }
    }

    /// Bound to this Mac's LAN or tailnet address, the host still answers apps on 127.0.0.1.
    @Test func aHostOnANonLoopbackAddressAlsoAnswersOnLoopback() async throws {
        guard let address = Self.firstNonLoopbackIPv4() else { return } // no network interface
        var config = HostConfiguration()
        config.token = "secret"
        config.bindAddress = address
        config.port = 0
        config.workerCount = 1
        config.workerModels = [HostConfiguration.defaultModel]
        let host = try await TranscriptionHost.start(configuration: config, configURL: nil, backend: StubSpeechBackend(),
                                                     dashboardHTML: [], serverVersion: "test")
        #expect(host.answersOnLoopback)
        for base in ["http://\(address):\(host.boundPort)", "http://127.0.0.1:\(host.boundPort)"] {
            var request = URLRequest(url: URL(string: "\(base)/v1/health")!)
            request.setValue("Bearer secret", forHTTPHeaderField: "Authorization")
            let (_, response) = try await URLSession.shared.data(for: request)
            #expect((response as? HTTPURLResponse)?.statusCode == 200, "\(base)")
        }
        await host.stop()
    }

    static func firstNonLoopbackIPv4() -> String? {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0, let first = head else { return nil }
        defer { freeifaddrs(head) }
        for pointer in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let entry = pointer.pointee
            guard let addr = entry.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET),
                  entry.ifa_flags & UInt32(IFF_UP) != 0, entry.ifa_flags & UInt32(IFF_LOOPBACK) == 0 else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            guard getnameinfo(addr, socklen_t(addr.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 else { continue }
            return String(cString: host)
        }
        return nil
    }

    @Test func portInUseIsAClearError() async throws {
        let (host, _) = try await start()
        var config = HostConfiguration()
        config.port = Int(host.boundPort)
        await #expect(throws: HTTPListener.ListenError.self) {
            _ = try await TranscriptionHost.start(configuration: config, configURL: nil, backend: StubSpeechBackend(),
                                                  dashboardHTML: [], serverVersion: "x")
        }
        await host.stop()
    }
}
