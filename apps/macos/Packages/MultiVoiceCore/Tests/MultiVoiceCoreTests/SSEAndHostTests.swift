import Foundation
import Testing
@testable import MultiVoiceCore

@Suite("SSE parser")
struct SSEParserTests {
    func events(_ messages: [SSEMessage]) -> [SSEEvent] {
        messages.compactMap { if case .event(let e) = $0 { e } else { nil } }
    }

    @Test func parsesTheHostFrameFormat() {
        var p = SSEParser()
        let out = p.feed("event: job_queued\ndata: {\"jobId\":1}\n\n: ping\n\n")
        #expect(out == [
            .event(SSEEvent(type: "job_queued", data: #"{"jobId":1}"#)),
            .comment("ping"),
        ])
    }

    @Test func handlesEveryLineEndingAndSplitCRLF() {
        var p = SSEParser()
        var out: [SSEMessage] = []
        out += p.feed("event: a\r\ndata: 1\r")
        out += p.feed("\n\r\n")           // CRLF split across chunks counts once
        out += p.feed("data: 2\r\rdata: 3\n\n")
        #expect(events(out) == [SSEEvent(type: "a", data: "1"), SSEEvent(data: "2"), SSEEvent(data: "3")])
    }

    @Test func joinsMultiLineDataAndStripsOneSpace() {
        var p = SSEParser()
        let out = p.feed("data:first\ndata:  second\ndata\n\n")
        #expect(events(out) == [SSEEvent(data: "first\n second\n")])
    }

    @Test func byteAtATimeEqualsWholeChunk() {
        let stream = "\u{FEFF}event: snapshot\ndata: {\"a\":1}\n\nid: 7\nevent: x\ndata: é\n\n"
        var a = SSEParser(), b = SSEParser()
        let whole = a.feed(stream)
        var single: [SSEMessage] = []
        for byte in Array(stream.utf8) { if let m = b.push(byte) { single.append(m) } }
        #expect(whole == single)
        #expect(events(whole).first?.type == "snapshot")   // BOM stripped
        #expect(events(whole).last == SSEEvent(type: "x", data: "é", id: "7"))
        #expect(b.lastEventID == "7")
    }

    @Test func ignoresEmptyEventsUnknownFieldsAndUnterminatedTail() {
        var p = SSEParser()
        let out = p.feed("event: lonely\n\nfoo: bar\nretry: 3000\ndata: x\n\ndata: unfinished")
        #expect(events(out) == [SSEEvent(data: "x")])
        #expect(p.retryMilliseconds == 3000)
    }

    @Test func decodesTypedHostEvents() throws {
        var p = SSEParser()
        let raw = """
        event: job_started
        data: {"at":1759600000000,"jobId":43,"worker":1,"model":"parakeet-ultra","client":null,"queueWaitMs":2}

        event: model_download
        data: {"at":1,"model":"parakeet-tdt-0.6b-v2","stage":"error","percentage":40,"error":"disk full"}

        event: brand_new_event
        data: {}


        """
        let typed = try events(p.feed(raw)).map(HostEvent.decode)
        #expect(typed[0] == .jobStarted(.init(jobId: "43", worker: 1, model: "parakeet-ultra", client: nil, queueWaitMs: 2, at: 1_759_600_000_000)))
        #expect(typed[1] == .modelDownload(.init(model: "parakeet-tdt-0.6b-v2", stage: "error", percentage: 40, error: "disk full", at: 1)))
        #expect(typed[2] == .unknown(type: "brand_new_event"))
    }

    @Test func rejectsMalformedKnownEvents() {
        #expect(throws: HostEvent.DecodeError.invalidJSON(type: "job_queued")) {
            try HostEvent.decode(SSEEvent(type: "job_queued", data: "{not json"))
        }
    }
}

@Suite("Host stats + live state")
struct HostLiveStateTests {
    static let statsJSON = """
    {"serverVersion":"0.1.0","bindAddr":"127.0.0.1:48173","uptimeSeconds":812,"activeSessions":2,"activeStreams":1,
     "queuedJobs":1,"runningJobs":1,"workerCount":2,"queueCapacity":8,"maxActiveStreams":4,"maxRecordingSeconds":600,
     "useGpu":true,"model":"parakeet-tdt-0.6b-v3",
     "models":[{"id":"parakeet-tdt-0.6b-v3","name":"Parakeet TDT 0.6B v3","publisher":"NVIDIA","sizeBytes":2549805858,"installed":true,"assignedWorkers":[0,1]}],
     "modelDownload":null,"rejectedJobs":0,"failedJobs":0,"totalTranscriptions":41,"totalAudioSeconds":512.4,
     "averageQueueMs":3,"averageProcessingMs":140,
     "workers":[{"index":0,"state":"transcribing","assignedModel":"parakeet-tdt-0.6b-v3","modelAvailable":true,
                 "loadedModel":"parakeet-tdt-0.6b-v3","completedJobs":20,"lastError":null,
                 "job":{"id":43,"model":"parakeet-tdt-0.6b-v3","source":"stream","client":"100.64.0.7","audioSeconds":0.0,"elapsedMs":2140}},
                {"index":1,"state":"idle","assignedModel":"parakeet-tdt-0.6b-v3","modelAvailable":true,"loadedModel":null,"completedJobs":21,"lastError":null,"job":null}],
     "queue":[{"id":44,"model":"parakeet-tdt-0.6b-v3","source":"batch","client":"10.0.0.9","audioSeconds":4.1,"waitingMs":12}],
     "streams":[{"id":12,"client":"100.64.0.7","elapsedMs":2200}],
     "clients":[{"address":"100.64.0.7","requests":9,"completed":8,"rejected":0,"failed":1,"totalAudioSeconds":61.0,"lastSeenMs":1759600000000,"lastModel":"parakeet-tdt-0.6b-v3"}],
     "recent":[{"id":41,"completedAtMs":1759600000000,"durationSeconds":7.9,"backend":"parakeet","model":"parakeet-tdt-0.6b-v3","source":"stream","client":"100.64.0.7","queueWaitMs":2,"processingMs":131}]}
    """

    @Test func decodesTheProtocolExampleSnapshot() throws {
        let s = try JSONDecoder().decode(HostStats.self, from: Data(Self.statsJSON.utf8))
        #expect(s.workers.count == 2)
        #expect(s.workers[0].job?.id == "43")
        #expect(s.queue.first?.id == "44")
        #expect(s.streams.first?.id == "12")
        #expect(s.models?.first?.assignedWorkers == [0, 1])
        #expect(s.recent.first?.processingMs == 131)
    }

    @Test func decodesOlderHostsWithoutOptionalFields() throws {
        let s = try JSONDecoder().decode(HostStats.self, from: Data(#"{"serverVersion":"0.0.9","workers":[{"index":0,"state":"idle"}]}"#.utf8))
        #expect(s.models == nil)
        #expect(s.workers.first?.assignedModel == "")
    }

    @Test func snapshotThenEventsTrackAJobEndToEnd() throws {
        var state = HostLiveState()
        let stats = try JSONDecoder().decode(HostStats.self, from: Data(Self.statsJSON.utf8))
        state.apply(.snapshot(stats), now: 1_000_000)
        #expect(state.workers.map(\.isBusy) == [true, false])
        #expect(state.workers[0].jobID == "43")
        #expect(state.queue.map(\.id) == ["44"])
        #expect(state.streams["12"] == "100.64.0.7")
        #expect(state.models.map(\.id) == ["parakeet-tdt-0.6b-v3"])

        // A new stream: queued at open with audioSeconds 0 and model "mixed".
        #expect(state.apply(.streamStarted(.init(streamId: "13", client: "dr@x.ie", at: 1))) == [.streamStarted(client: "dr@x.ie")])
        #expect(state.apply(.jobQueued(.init(jobId: "45", client: "dr@x.ie", source: "stream", model: "mixed", audioSeconds: 0, at: 1)))
                == [.queued(jobID: "45", client: "dr@x.ie")])
        #expect(!state.models.contains { $0.id == "mixed" })
        #expect(state.apply(.jobStarted(.init(jobId: "45", worker: 1, model: "parakeet-tdt-0.6b-v3", client: "dr@x.ie", queueWaitMs: 4, at: 2)))
                == [.dispatched(jobID: "45", client: "dr@x.ie", worker: 1)])
        #expect(state.workers[1].jobID == "45")
        state.apply(.streamFinished(.init(streamId: "13", client: "dr@x.ie", audioSeconds: 6, at: 3)))
        let cues = state.apply(.jobCompleted(.init(jobId: "45", worker: 1, model: "parakeet-tdt-0.6b-v3", client: "dr@x.ie",
                                                    audioSeconds: 6, processingMs: 50, at: 4)))
        #expect(cues == [.completed(jobID: "45", client: "dr@x.ie", worker: 1, model: "parakeet-tdt-0.6b-v3")])
        #expect(state.workers[1].jobID == nil)
        #expect(state.totalTranscriptions == 42)
        #expect(state.completions.last?.queueWaitMs == 4)
        #expect(state.clients.first { $0.id == "dr@x.ie" }?.completed == 1)
        #expect(state.streams["13"] == nil)
    }

    @Test func queueFullFailureHasNoWorker() {
        var state = HostLiveState()
        state.apply(.jobQueued(.init(jobId: "9", client: nil, source: "stream", model: "mixed", audioSeconds: 0, at: 1)))
        let cues = state.apply(.jobFailed(.init(jobId: "9", worker: nil, client: nil, error: "Transcription queue is full", at: 2)))
        #expect(cues == [.failed(jobID: "9", client: HostLiveState.unknownClient, worker: nil)])
        #expect(state.queue.isEmpty)
        #expect(state.failedJobs == 1)
    }

    @Test func metricsComputeLatencyAndThroughput() {
        var state = HostLiveState()
        for i in 0..<10 {
            state.apply(.jobStarted(.init(jobId: "\(i)", worker: 0, model: "m", client: "c", queueWaitMs: 10, at: Double(i) * 1000)))
            state.apply(.jobCompleted(.init(jobId: "\(i)", worker: 0, model: "m", client: "c", audioSeconds: 12,
                                             processingMs: Double(90 + i), at: Double(i) * 1000 + 100)))
        }
        let m = state.metrics(now: 10_000)
        #expect(m.jobsPerMinute == 10)
        #expect(m.audioSecondsPerMinute == 120)
        #expect(m.latencyP50Ms == 104.5)
        #expect((m.speedFactor ?? 0) > 120 && (m.speedFactor ?? 0) < 135)
    }

    @Test func clientDisplayNames() {
        #expect(HostLiveState.displayName(forClient: "alice@example.com") == "alice")
        #expect(HostLiveState.displayName(forClient: "100.64.0.7") == "100.64.0.7")
        #expect(HostLiveState.displayName(forClient: HostLiveState.unknownClient) == "Unknown client")
    }
}

@Suite("Polling fallback + simulator")
struct HostDifferAndSimulatorTests {
    @Test func differSynthesisesEventsFromSnapshots() throws {
        var differ = HostSnapshotDiffer()
        var a = HostStats()
        a.workers = [HostStats.Worker(index: 0, state: "idle", assignedModel: "m")]
        #expect(differ.diff(a, now: 0) == [.snapshot(a)])

        var b = a
        b.streams = [HostStats.Stream(id: "5", client: "c", elapsedMs: 10)]
        b.queue = [HostStats.QueuedJob(id: "7", model: "mixed", source: "stream", client: "c", audioSeconds: 0, waitingMs: 5)]
        let e1 = differ.diff(b, now: 2000)
        #expect(e1.contains { if case .streamStarted = $0 { true } else { false } })
        #expect(e1.contains { if case .jobQueued(let q) = $0 { q.jobId == "7" } else { false } })

        var c = b
        c.queue = []
        c.workers = [HostStats.Worker(index: 0, state: "transcribing", assignedModel: "m",
                                      job: HostStats.RunningJob(id: "7", model: "m", source: "stream", client: "c", audioSeconds: 0, elapsedMs: 100))]
        let e2 = differ.diff(c, now: 4000)
        #expect(e2.contains { if case .jobStarted(let s) = $0 { s.jobId == "7" && s.worker == 0 } else { false } })

        var d = c
        d.streams = []
        d.workers = [HostStats.Worker(index: 0, state: "idle", assignedModel: "m")]
        d.recent = [HostStats.Record(id: "100", completedAtMs: 5900, durationSeconds: 4, model: "m", source: "stream",
                                     client: "c", queueWaitMs: 5, processingMs: 60)]
        let e3 = differ.diff(d, now: 6000)
        #expect(e3.contains { if case .streamFinished = $0 { true } else { false } })
        #expect(e3.contains { if case .jobCompleted(let j) = $0 { j.worker == 0 && j.processingMs == 60 } else { false } })

        // Replaying through the reducer keeps it consistent.
        var state = HostLiveState()
        for e in [HostEvent.snapshot(a)] + e1 + e2 + e3 { state.apply(e, now: 6000) }
        #expect(state.queue.isEmpty)
        #expect(state.workers.first?.isBusy == false)
    }

    @Test func simulatorFollowsTheEventContract() throws {
        var sim = HostSimulator(now: 0)
        var state = HostLiveState()
        state.apply(.snapshot(sim.snapshot()), now: 0)
        var queued = Set<String>(), terminal = [String: Int](), started = Set<String>()
        var t: Double = 0
        while t < 120_000 {
            t += 100
            for e in sim.advance(to: t) {
                // Round-trip each event through JSON + SSE like a real host would send it.
                state.apply(e, now: t)
                switch e {
                case .jobQueued(let q): queued.insert(q.jobId); #expect(q.audioSeconds == 0)
                case .jobStarted(let s): started.insert(s.jobId); #expect(queued.contains(s.jobId))
                case .jobCompleted(let c): terminal[c.jobId, default: 0] += 1; #expect(started.contains(c.jobId))
                case .jobFailed(let f): terminal[f.jobId, default: 0] += 1
                default: break
                }
            }
        }
        #expect(queued.count > 50)
        #expect(terminal.values.allSatisfy { $0 == 1 })
        #expect(Set(terminal.keys).isSubset(of: queued))
        // Jobs still in flight at the end are the only ones without a terminal event.
        #expect(queued.count - terminal.count <= 8 + 3)
        #expect(state.metrics(now: t).jobsPerMinute > 10)
    }

    @Test func simulatorIsDeterministic() {
        var a = HostSimulator(now: 0), b = HostSimulator(now: 0)
        #expect(a.advance(to: 30_000) == b.advance(to: 30_000))
    }
}
