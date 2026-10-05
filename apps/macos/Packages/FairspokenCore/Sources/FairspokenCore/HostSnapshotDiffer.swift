import Foundation

/// Polling fallback for hosts without `/v1/events`: turns consecutive `/v1/stats`
/// snapshots into the same `HostEvent`s the SSE stream would have sent, so the
/// visualisation animates identically (at 2 s granularity).
public struct HostSnapshotDiffer: Sendable {
    private var previous: HostStats?
    private var openStreams: [String: [String]] = [:] // client → synthetic stream ids
    private var seenRecordIDs: Set<String> = []
    private var serial = 0

    public init() {}

    public mutating func diff(_ next: HostStats, now: Double = Date().timeIntervalSince1970 * 1000) -> [HostEvent] {
        defer { previous = next }
        guard let prev = previous else {
            seenRecordIDs = Set(next.recent.map(\.id))
            for (n, s) in next.streams.enumerated() {
                openStreams[HostLiveState.key(s.client), default: []].append("snapshot-\(n)")
            }
            return [.snapshot(next)]
        }
        var events: [HostEvent] = []

        // Streams: compare per-client counts.
        let before = Dictionary(grouping: prev.streams, by: { HostLiveState.key($0.client) }).mapValues(\.count)
        let after = Dictionary(grouping: next.streams, by: { HostLiveState.key($0.client) }).mapValues(\.count)
        for client in Set(before.keys).union(after.keys).sorted() {
            let delta = (after[client] ?? 0) - (before[client] ?? 0)
            if delta > 0 {
                for _ in 0..<delta {
                    serial += 1
                    let id = "poll-s\(serial)"
                    openStreams[client, default: []].append(id)
                    events.append(.streamStarted(.init(streamId: id, client: client, at: now)))
                }
            } else if delta < 0 {
                for _ in 0..<(-delta) {
                    let id = openStreams[client]?.popLast() ?? "poll-s-unknown"
                    events.append(.streamFinished(.init(streamId: id, client: client, audioSeconds: 0, at: now)))
                }
            }
        }

        // Queue additions.
        let prevQueue = Set(prev.queue.map(\.id))
        for job in next.queue where !prevQueue.contains(job.id) {
            events.append(.jobQueued(.init(jobId: job.id, client: job.client, source: job.source, model: job.model,
                                            audioSeconds: job.audioSeconds, at: now - job.waitingMs)))
        }
        // Jobs that left the queue, available to label dispatches.
        let nextQueue = Set(next.queue.map(\.id))
        var departed = prev.queue.filter { !nextQueue.contains($0.id) }

        // Workers that picked up a job.
        for w in next.workers {
            let had = prev.workers.first { $0.index == w.index }
            if let job = w.job, had?.job == nil || had?.job?.id != job.id || had?.job?.client != job.client {
                let match = job.id.flatMap { id in departed.firstIndex { $0.id == id } }
                    ?? departed.firstIndex { $0.client == job.client } ?? departed.indices.first
                let id: String
                var wait: Double = 0
                if let match {
                    id = departed[match].id
                    wait = departed[match].waitingMs
                    departed.remove(at: match)
                } else if let jobID = job.id {
                    id = jobID
                } else {
                    serial += 1
                    id = "poll-j\(serial)"
                }
                events.append(.jobStarted(.init(jobId: id, worker: w.index, model: job.model, client: job.client,
                                                 queueWaitMs: wait, at: now - job.elapsedMs)))
            } else if w.job == nil, w.state != had?.state {
                events.append(.workerState(.init(worker: w.index, state: w.state, model: w.loadedModel ?? w.assignedModel, at: now)))
            }
        }

        // New completions from the `recent` ring (newest first in the snapshot).
        for record in next.recent.reversed() where !seenRecordIDs.contains(record.id) {
            seenRecordIDs.insert(record.id)
            let worker = prev.workers.first { $0.job?.client == record.client }?.index
                ?? next.workers.first { ($0.loadedModel ?? $0.assignedModel) == record.model }?.index ?? 0
            events.append(.jobCompleted(.init(jobId: "rec-\(record.id)", worker: worker, model: record.model, client: record.client,
                                               audioSeconds: record.durationSeconds, processingMs: record.processingMs,
                                               at: record.completedAtMs)))
        }
        if seenRecordIDs.count > 1000 { seenRecordIDs = Set(next.recent.map(\.id)) }

        // Failures carry no detail in stats.
        if next.failedJobs > prev.failedJobs {
            for _ in 0..<min(5, next.failedJobs - prev.failedJobs) {
                serial += 1
                events.append(.jobFailed(.init(jobId: "poll-f\(serial)", worker: nil, client: nil, error: "failed", at: now)))
            }
        }

        // Finally fold the authoritative snapshot in (counters, worker states, models).
        events.append(.snapshot(next))
        return events
    }
}
