import Foundation
import Synchronization

/// Audio for a job: a complete recording (batch upload), or frames still arriving from a
/// client that is speaking, decoded as they land.
enum JobAudio: Sendable {
    case recording(HostRecording)
    case stream(StreamFeed)
}

enum StreamInput: Sendable {
    case frame([Int16], sampleRate: Int)
    /// The upload failed part-way; the request handler already answered the client.
    case abort
}

/// Frames from the request handler to the worker. Unbounded: a worker busy with a segment
/// decode never stalls the upload.
final class StreamFeed: Sendable {
    let stream: AsyncStream<StreamInput>
    private let continuation: AsyncStream<StreamInput>.Continuation
    private let ended = Mutex<ContinuousClock.Instant?>(nil)

    init() {
        (stream, continuation) = AsyncStream<StreamInput>.makeStream(bufferingPolicy: .unbounded)
    }

    func send(_ input: StreamInput) { continuation.yield(input) }

    /// Ends the upload, remembering when: a job's processing time is measured from here (the
    /// client's release), so it includes any wait for a worker after the client stopped.
    func finish() {
        ended.withLock { if $0 == nil { $0 = .now } }
        continuation.finish()
    }

    var endedAt: ContinuousClock.Instant? { ended.withLock { $0 } }
}

struct TranscriptionOutcome: Sendable {
    var text: String
    var backend: String
    var model: String
    var durationSeconds: Double
}

/// One-shot result handed from a worker back to the waiting request.
final class JobResult: Sendable {
    private let state = Mutex<(value: Result<TranscriptionOutcome, HostRuntime.RuntimeError>?, waiter: CheckedContinuation<Result<TranscriptionOutcome, HostRuntime.RuntimeError>, Never>?)>((nil, nil))

    func fulfil(_ value: Result<TranscriptionOutcome, HostRuntime.RuntimeError>) {
        let waiter = state.withLock { s -> CheckedContinuation<Result<TranscriptionOutcome, HostRuntime.RuntimeError>, Never>? in
            guard s.value == nil else { return nil }
            s.value = value
            defer { s.waiter = nil }
            return s.waiter
        }
        waiter?.resume(returning: value)
    }

    func wait() async -> Result<TranscriptionOutcome, HostRuntime.RuntimeError> {
        await withCheckedContinuation { cont in
            let ready = state.withLock { s -> Result<TranscriptionOutcome, HostRuntime.RuntimeError>? in
                if let v = s.value { return v }
                s.waiter = cont
                return nil
            }
            if let ready { cont.resume(returning: ready) }
        }
    }
}

struct TranscriptionJob: Sendable {
    var id: UInt64
    var audio: JobAudio
    var language: String
    var source: String
    var client: String?
    var acceptedAt: ContinuousClock.Instant
    var result: JobResult
}

/// Timeouts for waits that usually end some other way (a job or a frame arrives first). Each
/// is a sleeping task that whoever ends the wait cancels, so a busy host doesn't pile up
/// sleepers until their timeouts lapse.
final class WaitTimers: Sendable {
    private let live = Atomic<Int>(0)

    /// Timers still sleeping (or just finishing).
    var pending: Int { live.load(ordering: .relaxed) }

    /// Calls `fire` after `timeout` unless the returned task is cancelled first.
    func start(after timeout: Duration, _ fire: @escaping @Sendable () -> Void) -> Task<Void, Never> {
        live.add(1, ordering: .relaxed)
        return Task { [self] in
            defer { live.subtract(1, ordering: .relaxed) }
            try? await Task.sleep(for: timeout)
            if !Task.isCancelled { fire() }
        }
    }
}

/// Bounded FIFO of jobs waiting for a worker (the Rust host's `sync_channel(queue_capacity)`):
/// an idle worker takes a job straight away; otherwise up to `capacity` wait.
final class JobQueue: Sendable {
    enum EnqueueResult { case accepted, full, closed }

    /// An idle worker. Whoever removes it from `State.waiters` (under the lock) resumes it,
    /// exactly once, and cancels its timer.
    private struct Waiter {
        var token: UInt64
        var cont: CheckedContinuation<TranscriptionJob?, Never>
        var timer: Task<Void, Never>

        func resume(returning job: TranscriptionJob?) {
            timer.cancel()
            cont.resume(returning: job)
        }
    }

    private struct State {
        var jobs: [TranscriptionJob] = []
        var waiters: [Waiter] = []
        var nextToken: UInt64 = 0
        var closed = false
    }

    let capacity: Int
    private let state = Mutex(State())
    private let timers = WaitTimers()

    init(capacity: Int) { self.capacity = capacity }

    /// Idle-poll timers not yet finished.
    var pendingTimers: Int { timers.pending }

    func tryEnqueue(_ job: TranscriptionJob) -> EnqueueResult {
        let (result, waiter) = state.withLock { s -> (EnqueueResult, Waiter?) in
            if s.closed { return (.closed, nil) }
            if !s.waiters.isEmpty { return (.accepted, s.waiters.removeFirst()) }
            guard s.jobs.count < capacity else { return (.full, nil) }
            s.jobs.append(job)
            return (.accepted, nil)
        }
        waiter?.resume(returning: job)
        return result
    }

    /// The next job, or nil after `timeout` (so idle workers re-check their model) or once closed.
    func next(timeout: Duration) async -> TranscriptionJob? {
        await withCheckedContinuation { (cont: CheckedContinuation<TranscriptionJob?, Never>) in
            let immediate = state.withLock { s -> TranscriptionJob?? in
                if !s.jobs.isEmpty { return .some(s.jobs.removeFirst()) }
                if s.closed { return .some(nil) }
                s.nextToken += 1
                let token = s.nextToken
                // Started under the lock, so it is stored before anyone can take the waiter.
                let timer = timers.start(after: timeout) { [weak self] in self?.expire(token) }
                s.waiters.append(Waiter(token: token, cont: cont, timer: timer))
                return nil
            }
            if let immediate { cont.resume(returning: immediate) }
        }
    }

    private func expire(_ token: UInt64) {
        let expired = state.withLock { s -> Waiter? in
            guard let i = s.waiters.firstIndex(where: { $0.token == token }) else { return nil }
            return s.waiters.remove(at: i)
        }
        expired?.resume(returning: nil)
    }

    /// Wakes every idle worker now (a config change or a finished download).
    func wakeIdleWorkers() {
        let waiters = state.withLock { s -> [Waiter] in
            defer { s.waiters.removeAll() }
            return s.waiters
        }
        waiters.forEach { $0.resume(returning: nil) }
    }

    /// Stops the queue; jobs still waiting are returned so they can be failed.
    func close() -> [TranscriptionJob] {
        let (jobs, waiters) = state.withLock { s -> ([TranscriptionJob], [Waiter]) in
            s.closed = true
            defer { s.jobs.removeAll(); s.waiters.removeAll() }
            return (s.jobs, s.waiters)
        }
        waiters.forEach { $0.resume(returning: nil) }
        return jobs
    }
}
