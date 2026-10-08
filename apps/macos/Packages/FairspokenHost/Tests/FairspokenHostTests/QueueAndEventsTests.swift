import Foundation
import Testing
@testable import FairspokenHost

@Suite("Job queue")
struct JobQueueTests {
    func job(_ id: UInt64) -> TranscriptionJob {
        TranscriptionJob(id: id, audio: .recording(HostRecording(samples: [1], sampleRate: 16_000)), language: "en", source: "batch",
                         client: nil, acceptedAt: .now, result: JobResult())
    }

    @Test func holdsUpToCapacityThenReportsFull() async {
        let queue = JobQueue(capacity: 2)
        #expect(queue.tryEnqueue(job(1)) == .accepted)
        #expect(queue.tryEnqueue(job(2)) == .accepted)
        #expect(queue.tryEnqueue(job(3)) == .full)
        #expect(await queue.next(timeout: .milliseconds(50))?.id == 1)
        #expect(queue.tryEnqueue(job(4)) == .accepted)
        #expect(await queue.next(timeout: .milliseconds(50))?.id == 2)
        #expect(await queue.next(timeout: .milliseconds(50))?.id == 4)
    }

    @Test func handsJobsStraightToAWaitingWorker() async {
        let queue = JobQueue(capacity: 1)
        let waiter = Task { await queue.next(timeout: .seconds(5)) }
        try? await Task.sleep(for: .milliseconds(30))
        #expect(queue.tryEnqueue(job(7)) == .accepted)
        // The waiting worker took it, so the queue still has its full capacity.
        #expect(queue.tryEnqueue(job(8)) == .accepted)
        #expect(await waiter.value?.id == 7)
    }

    @Test func timesOutAndCloses() async {
        let queue = JobQueue(capacity: 1)
        let started = ContinuousClock.now
        #expect(await queue.next(timeout: .milliseconds(40)) == nil)
        #expect(ContinuousClock.now - started >= .milliseconds(35))
        _ = queue.tryEnqueue(job(1))
        let leftover = queue.close()
        #expect(leftover.map(\.id) == [1])
        #expect(queue.tryEnqueue(job(2)) == .closed)
        #expect(await queue.next(timeout: .seconds(5)) == nil)
    }

    @Test func waitsEndedByAJobAWakeOrCloseCancelTheirTimers() async throws {
        let queue = JobQueue(capacity: 4)
        for i in UInt64(0)..<20 {
            let waiter = Task { await queue.next(timeout: .seconds(60)) }
            try await Task.sleep(for: .milliseconds(2))
            #expect(queue.tryEnqueue(job(i)) == .accepted)
            #expect(await waiter.value?.id == i)
        }
        let woken = (0..<5).map { _ in Task { await queue.next(timeout: .seconds(60)) } }
        try await Task.sleep(for: .milliseconds(20))
        queue.wakeIdleWorkers()
        for w in woken { #expect(await w.value == nil) }
        let closed = (0..<5).map { _ in Task { await queue.next(timeout: .seconds(60)) } }
        try await Task.sleep(for: .milliseconds(20))
        _ = queue.close()
        for w in closed { #expect(await w.value == nil) }
        // Without cancellation 30 timers would sleep out their full minute.
        #expect(await settles { queue.pendingTimers == 0 })
    }

    @Test func racingTimeoutsAndJobsResumeEachWaiterExactlyOnce() async {
        let queue = JobQueue(capacity: 1_000)
        var received = 0
        for i in 0..<300 {
            async let got = queue.next(timeout: .microseconds(i % 50))
            _ = queue.tryEnqueue(job(UInt64(i)))
            if await got != nil { received += 1 }
        }
        // A job whose waiter timed out first stays queued for the next taker.
        while await queue.next(timeout: .milliseconds(20)) != nil { received += 1 }
        #expect(received == 300)
        #expect(await settles { queue.pendingTimers == 0 })
    }
}

/// Polls `condition` for up to two seconds.
func settles(_ condition: () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(2)
    while ContinuousClock.now < deadline {
        if condition() { return true }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return condition()
}

@Suite("SSE fan-out")
struct EventHubTests {
    @Test func limitsSubscribersToSixteenAndFreesSlots() throws {
        let hub = EventHub()
        var subs: [EventSubscription] = []
        for _ in 0..<16 { subs.append(try hub.subscribe()) }
        #expect(throws: EventHub.SubscribeError.tooMany) { try hub.subscribe() }
        #expect(EventHub.SubscribeError.tooMany.message == "Too many event subscribers (limit 16)")
        subs[3].cancel()
        #expect(hub.subscriberCount == 15)
        _ = try hub.subscribe()
        // In-process observers don't count.
        _ = hub.observe()
        #expect(hub.subscriberCount == 16)
    }

    @Test func deliversInOrderAndHeartbeatsWhenIdle() async throws {
        let hub = EventHub()
        let sub = try hub.subscribe()
        hub.publish("a")
        hub.publish("b")
        #expect(await sub.next(timeout: .seconds(1)) == .frame("a"))
        #expect(await sub.next(timeout: .seconds(1)) == .frame("b"))
        #expect(await sub.next(timeout: .milliseconds(30)) == .timeout)
        let pending = Task { await sub.next(timeout: .seconds(5)) }
        try await Task.sleep(for: .milliseconds(20))
        hub.publish("c")
        #expect(await pending.value == .frame("c"))
    }

    @Test func dropsASubscriberThatFallsBehindWithoutBlockingOthers() async throws {
        let hub = EventHub()
        let slow = try hub.subscribe()
        let fast = try hub.subscribe()
        for i in 0..<(EventHub.subscriberBuffer + 1) {
            hub.publish("f\(i)")
            if case .frame = await fast.next(timeout: .seconds(1)) {} else { Issue.record("fast subscriber starved") }
        }
        #expect(slow.isClosed)
        #expect(hub.subscriberCount == 1)
        #expect(!fast.isClosed)
    }

    @Test func waitsEndedByAFrameOrCloseCancelTheirHeartbeatTimers() async throws {
        let hub = EventHub()
        let sub = try hub.subscribe()
        for i in 0..<20 {
            let pending = Task { await sub.next(timeout: .seconds(60)) }
            try await Task.sleep(for: .milliseconds(2))
            hub.publish("f\(i)")
            #expect(await pending.value == .frame("f\(i)"))
        }
        let pending = Task { await sub.next(timeout: .seconds(60)) }
        try await Task.sleep(for: .milliseconds(20))
        sub.cancel()
        #expect(await pending.value == .closed)
        // Without cancellation 21 timers would sleep out their full minute.
        #expect(await settles { sub.pendingTimers == 0 })
    }

    @Test func racingHeartbeatsAndFramesResumeTheWriterExactlyOnce() async throws {
        let hub = EventHub()
        let sub = try hub.subscribe()
        var frames = 0
        for i in 0..<300 {
            async let next = sub.next(timeout: .microseconds(i % 50))
            hub.publish("f\(i)")
            if case .frame = await next { frames += 1 }
        }
        // A frame that lost the race to a heartbeat stays buffered for the next call.
        while case .frame = await sub.next(timeout: .milliseconds(20)) { frames += 1 }
        #expect(frames == 300)
        #expect(!sub.isClosed)
        #expect(await settles { sub.pendingTimers == 0 })
    }

    @Test func eventFramesMatchTheRustWireFormat() {
        let frame = HostWireEvent.jobQueued(jobId: 7, client: "192.168.1.31", source: "stream", model: "parakeet-tdt-0.6b-v3", audioSeconds: 0)
            .frame(at: 1_700_000_000_000)
        #expect(frame == "event: job_queued\ndata: {\"at\":1700000000000,\"jobId\":7,\"client\":\"192.168.1.31\",\"source\":\"stream\",\"model\":\"parakeet-tdt-0.6b-v3\",\"audioSeconds\":0.0}\n\n")
        let failed = HostWireEvent.jobFailed(jobId: 2, worker: nil, client: nil, error: "Transcription queue is full").frame(at: 5)
        #expect(failed == "event: job_failed\ndata: {\"at\":5,\"jobId\":2,\"worker\":null,\"client\":null,\"error\":\"Transcription queue is full\"}\n\n")
        let download = HostWireEvent.modelDownload(model: "m", stage: "downloading", percentage: 40, error: nil).frame(at: 1)
        #expect(!download.contains("error"))
    }
}

@Suite("Configuration")
struct ConfigurationTests {
    let known: Set<String> = ["parakeet-tdt-0.6b-v3", "parakeet-ultra"]

    @Test func updatesValidateEveryFieldBeforeApplyingAny() throws {
        let live = HostLiveSettings(maxActiveStreams: 4, maxRecordingSeconds: 600, useGpu: true, workerModels: ["parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v3"])
        #expect(throws: HostConfigUpdate.ParseError.invalid("maxRecordingSeconds must be between 10 and 600")) {
            try live.applying(HostConfigUpdate(maxActiveStreams: 8, maxRecordingSeconds: 9), knownModels: known)
        }
        #expect(throws: HostConfigUpdate.ParseError.self) { try live.applying(HostConfigUpdate(maxActiveStreams: 33), knownModels: known) }
        #expect(throws: HostConfigUpdate.ParseError.invalid("Provide either model or workerModels, not both")) {
            try live.applying(HostConfigUpdate(model: "parakeet-ultra", workerModels: ["parakeet-ultra", "parakeet-ultra"]), knownModels: known)
        }
        #expect(throws: HostConfigUpdate.ParseError.invalid("workerModels must list exactly 2 models (one per worker)")) {
            try live.applying(HostConfigUpdate(workerModels: ["parakeet-ultra"]), knownModels: known)
        }
        #expect(throws: HostConfigUpdate.ParseError.invalid("Unsupported model: tiny")) {
            try live.applying(HostConfigUpdate(model: "tiny"), knownModels: known)
        }
        let next = try live.applying(HostConfigUpdate(maxActiveStreams: 8, workerModels: ["parakeet-ultra", "parakeet-tdt-0.6b-v3"]), knownModels: known)
        #expect(next.maxActiveStreams == 8)
        #expect(next.modelSummary == "mixed")
    }

    @Test func parsesBodiesWithSerdeTypingRules() throws {
        #expect(try HostConfigUpdate.parse(Array(#"{"maxActiveStreams":3,"useGpu":false,"model":null}"#.utf8))
            == HostConfigUpdate(maxActiveStreams: 3, useGpu: false))
        #expect(throws: HostConfigUpdate.ParseError.self) { try HostConfigUpdate.parse(Array(#"{"bogus":1}"#.utf8)) }
        #expect(throws: HostConfigUpdate.ParseError.self) { try HostConfigUpdate.parse(Array(#"{"maxActiveStreams":-1}"#.utf8)) }
        #expect(throws: HostConfigUpdate.ParseError.self) { try HostConfigUpdate.parse(Array(#"{"maxActiveStreams":true}"#.utf8)) }
        #expect(throws: HostConfigUpdate.ParseError.self) { try HostConfigUpdate.parse(Array(#"{"maxRecordingSeconds":70000}"#.utf8)) }
        #expect(throws: HostConfigUpdate.ParseError.self) { try HostConfigUpdate.parse(Array("[1]".utf8)) }
        #expect(throws: HostConfigUpdate.ParseError.self) { try HostConfigUpdate.parse([]) }
    }

    @Test func environmentSeedsFirstBootAndOverridesRestartOnlySettings() throws {
        let env = ["FAIRSPOKEN_HOST_ADDR": "0.0.0.0:48981", "FAIRSPOKEN_HOST_TOKEN": "t", "FAIRSPOKEN_HOST_WORKERS": "3",
                   "FAIRSPOKEN_HOST_MODEL": "parakeet-ultra", "FAIRSPOKEN_HOST_MAX_ACTIVE_STREAMS": "2"]
        let first = try HostConfigurationStore.resolve(file: nil, environment: env, knownModels: known)
        #expect(first.bindAddress == "0.0.0.0" && first.port == 48981 && first.token == "t")
        #expect(first.workerModels == ["parakeet-ultra", "parakeet-ultra", "parakeet-ultra"])
        #expect(first.maxActiveStreams == 2)
        var file = HostConfiguration()
        file.maxActiveStreams = 9
        let later = try HostConfigurationStore.resolve(file: file, environment: env, knownModels: known)
        #expect(later.maxActiveStreams == 9) // the saved, dashboard-edited value wins
        #expect(later.workerCount == 3 && later.workerModels.count == 3)
        #expect(throws: HostConfigurationStore.StoreError.self) {
            try HostConfigurationStore.resolve(file: nil, environment: ["FAIRSPOKEN_HOST_MODEL": "nope"], knownModels: known)
        }
        #expect(HostConfigurationStore.parseAddress("[::1]:9") ?? ("", 0) == ("::1", 9))
    }

    @Test func legacyEnvironmentPrefixIsAFallback() throws {
        let legacy = try HostConfigurationStore.resolve(file: nil, environment: ["MULTIVOICE_HOST_TOKEN": "old", "MULTIVOICE_HOST_WORKERS": "2"],
                                                        knownModels: known)
        #expect(legacy.token == "old" && legacy.workerCount == 2)
        let both = ["FAIRSPOKEN_HOST_TOKEN": "new", "MULTIVOICE_HOST_TOKEN": "old", "FAIRSPOKEN_HOST_ADDR": "127.0.0.1:9", "MULTIVOICE_HOST_ADDR": "bad"]
        let config = try HostConfigurationStore.resolve(file: nil, environment: both, knownModels: known)
        #expect(config.token == "new" && config.port == 9) // the new name wins
        #expect(HostEnvironment.value("TOKEN", in: ["FAIRSPOKEN_HOST_TOKEN": "", "MULTIVOICE_HOST_TOKEN": "old"]) == "") // set, even empty, wins
        #expect(HostEnvironment.value("TOKEN", in: [:]) == nil)
        #expect(HostConfigurationStore.defaultURL(bundleID: "x", environment: ["MULTIVOICE_HOST_CONFIG_PATH": "/tmp/legacy.json"]).path == "/tmp/legacy.json")
    }

    @Test func readsTheRustHostConfigFile() throws {
        let rust = #"{"maxActiveStreams":6,"maxRecordingSeconds":120,"useGpu":false,"workerModels":["parakeet-ultra"]}"#
        let config = try JSONDecoder().decode(HostConfiguration.self, from: Data(rust.utf8)).normalized(knownModels: known)
        #expect(config.maxActiveStreams == 6 && config.maxRecordingSeconds == 120 && !config.useGpu)
        #expect(config.workerModels == Array(repeating: "parakeet-ultra", count: config.workerCount)) // one model fits every worker
    }
}
