import Foundation
import Synchronization

/// One `/v1/events` activity event. Field order and names follow `HostEventKind` in
/// src-tauri/src/host/events.rs; never carries transcript text or audio.
public enum HostWireEvent: Sendable {
    case streamStarted(streamId: UInt64, client: String?)
    case streamFinished(streamId: UInt64, client: String?, audioSeconds: Double)
    case jobQueued(jobId: UInt64, client: String?, source: String, model: String, audioSeconds: Double)
    case jobStarted(jobId: UInt64, worker: Int, model: String, client: String?, queueWaitMs: UInt64)
    case jobCompleted(jobId: UInt64, worker: Int, model: String, client: String?, audioSeconds: Double, processingMs: UInt64)
    case jobFailed(jobId: UInt64, worker: Int?, client: String?, error: String)
    case workerState(worker: Int, state: String, model: String?)
    case modelDownload(model: String, stage: String, percentage: Int, error: String?)

    public var name: String {
        switch self {
        case .streamStarted: "stream_started"
        case .streamFinished: "stream_finished"
        case .jobQueued: "job_queued"
        case .jobStarted: "job_started"
        case .jobCompleted: "job_completed"
        case .jobFailed: "job_failed"
        case .workerState: "worker_state"
        case .modelDownload: "model_download"
        }
    }

    func json(at: UInt64) -> JSONValue {
        let at: (String, JSONValue) = ("at", .int(Int64(at)))
        func id(_ v: UInt64) -> JSONValue { .int(Int64(v)) }
        switch self {
        case .streamStarted(let s, let c):
            return .object([at, ("streamId", id(s)), ("client", .optionalString(c))])
        case .streamFinished(let s, let c, let a):
            return .object([at, ("streamId", id(s)), ("client", .optionalString(c)), ("audioSeconds", .double(a))])
        case .jobQueued(let j, let c, let src, let m, let a):
            return .object([at, ("jobId", id(j)), ("client", .optionalString(c)), ("source", .string(src)), ("model", .string(m)),
                            ("audioSeconds", .double(a))])
        case .jobStarted(let j, let w, let m, let c, let q):
            return .object([at, ("jobId", id(j)), ("worker", .int(w)), ("model", .string(m)), ("client", .optionalString(c)),
                            ("queueWaitMs", id(q))])
        case .jobCompleted(let j, let w, let m, let c, let a, let p):
            return .object([at, ("jobId", id(j)), ("worker", .int(w)), ("model", .string(m)), ("client", .optionalString(c)),
                            ("audioSeconds", .double(a)), ("processingMs", id(p))])
        case .jobFailed(let j, let w, let c, let e):
            return .object([at, ("jobId", id(j)), ("worker", .optionalInt(w)), ("client", .optionalString(c)), ("error", .string(e))])
        case .workerState(let w, let s, let m):
            return .object([at, ("worker", .int(w)), ("state", .string(s)), ("model", .optionalString(m))])
        case .modelDownload(let m, let s, let p, let e):
            var fields: [(String, JSONValue)] = [at, ("model", .string(m)), ("stage", .string(s)), ("percentage", .int(p))]
            if let e { fields.append(("error", .string(e))) }
            return .object(fields)
        }
    }

    public func frame(at: UInt64) -> String { SSE.frame(event: name, data: json(at: at).serialized) }
}

public enum SSE {
    public static let heartbeat = ": ping\n\n"
    /// JSON never contains a raw newline, so the payload is always one `data:` line.
    public static func frame(event: String, data: String) -> String { "event: \(event)\ndata: \(data)\n\n" }
}

/// Broadcast of bounded per-subscriber buffers (Rust `EventHub`). Publishing never blocks: a
/// subscriber whose buffer is full is dropped and should reconnect to resync from a new
/// snapshot. In-process observers (the app's own UI) are separate and don't count against
/// the subscriber limit.
public final class EventHub: Sendable {
    public static let maxSubscribers = 16
    public static let subscriberBuffer = 256

    private struct State {
        var nextID: UInt64 = 0
        var subscribers: [UInt64: EventSubscription] = [:]
        var observers: [UInt64: AsyncStream<String>.Continuation] = [:]
    }

    private let state = Mutex(State())
    public init() {}

    public enum SubscribeError: Error, Equatable {
        case tooMany
        public var message: String { "Too many event subscribers (limit \(EventHub.maxSubscribers))" }
    }

    public func subscribe() throws(SubscribeError) -> EventSubscription {
        try state.withLock { s throws(SubscribeError) in
            guard s.subscribers.count < Self.maxSubscribers else { throw .tooMany }
            s.nextID += 1
            let sub = EventSubscription(id: s.nextID, hub: self)
            s.subscribers[s.nextID] = sub
            return sub
        }
    }

    public var subscriberCount: Int { state.withLock { $0.subscribers.count } }
    public var hasListeners: Bool { state.withLock { !$0.subscribers.isEmpty || !$0.observers.isEmpty } }

    /// An unbounded in-process feed of frames. Ends when the returned stream is dropped.
    public func observe() -> AsyncStream<String> {
        let (stream, continuation) = AsyncStream<String>.makeStream(bufferingPolicy: .bufferingNewest(8192))
        let id = state.withLock { s -> UInt64 in
            s.nextID += 1
            s.observers[s.nextID] = continuation
            return s.nextID
        }
        continuation.onTermination = { [weak self] _ in
            _ = self?.state.withLock { $0.observers.removeValue(forKey: id) }
        }
        return stream
    }

    public func publish(_ frame: String) {
        let (subs, observers) = state.withLock { s in (Array(s.subscribers.values), Array(s.observers.values)) }
        for sub in subs where !sub.push(frame) {
            unsubscribe(sub.id)
        }
        for o in observers { o.yield(frame) }
    }

    func unsubscribe(_ id: UInt64) {
        let sub = state.withLock { $0.subscribers.removeValue(forKey: id) }
        sub?.close()
    }

    /// Ends every subscription and observer (host shutdown).
    public func closeAll() {
        let (subs, observers) = state.withLock { s -> ([EventSubscription], [AsyncStream<String>.Continuation]) in
            defer { s.subscribers.removeAll(); s.observers.removeAll() }
            return (Array(s.subscribers.values), Array(s.observers.values))
        }
        subs.forEach { $0.close() }
        observers.forEach { $0.finish() }
    }
}

/// A connected `/v1/events` subscriber: a 256-frame buffer drained by its writer.
public final class EventSubscription: Sendable {
    public enum Next: Equatable, Sendable {
        case frame(String)
        case timeout
        case closed
    }

    private struct State {
        var frames: [String] = []
        var head = 0
        var closed = false
        var waiter: (token: UInt64, cont: CheckedContinuation<Next, Never>)?
        var nextToken: UInt64 = 0
        var buffered: Int { frames.count - head }
    }

    let id: UInt64
    private weak let hub: EventHub?
    private let state = Mutex(State())

    init(id: UInt64, hub: EventHub) {
        self.id = id
        self.hub = hub
    }

    /// False when the buffer was already full (the hub then drops this subscriber).
    func push(_ frame: String) -> Bool {
        let (ok, resume) = state.withLock { s -> (Bool, CheckedContinuation<Next, Never>?) in
            if s.closed { return (true, nil) }
            if let w = s.waiter {
                s.waiter = nil
                return (true, w.cont)
            }
            guard s.buffered < EventHub.subscriberBuffer else { return (false, nil) }
            s.frames.append(frame)
            return (true, nil)
        }
        resume?.resume(returning: .frame(frame))
        return ok
    }

    func close() {
        let waiter = state.withLock { s -> CheckedContinuation<Next, Never>? in
            s.closed = true
            defer { s.waiter = nil }
            return s.waiter?.cont
        }
        waiter?.resume(returning: .closed)
    }

    /// Releases the subscriber slot (the client went away).
    public func cancel() {
        if let hub { hub.unsubscribe(id) } else { close() }
    }

    public var isClosed: Bool { state.withLock { $0.closed } }

    /// The next frame, `.timeout` after `timeout` with nothing to send, or `.closed`.
    public func next(timeout: Duration) async -> Next {
        let token: UInt64? = nil
        _ = token
        return await withCheckedContinuation { (cont: CheckedContinuation<Next, Never>) in
            let immediate = state.withLock { s -> Next? in
                if s.buffered > 0 {
                    let f = s.frames[s.head]
                    s.head += 1
                    if s.head > 64 && s.head * 2 > s.frames.count {
                        s.frames.removeFirst(s.head)
                        s.head = 0
                    }
                    return .frame(f)
                }
                if s.closed { return .closed }
                s.nextToken += 1
                s.waiter = (s.nextToken, cont)
                return nil
            }
            if let immediate {
                cont.resume(returning: immediate)
                return
            }
            let waitToken = state.withLock { $0.nextToken }
            Task { [weak self] in
                try? await Task.sleep(for: timeout)
                guard let self else { return }
                let expired = self.state.withLock { s -> CheckedContinuation<Next, Never>? in
                    guard let w = s.waiter, w.token == waitToken else { return nil }
                    s.waiter = nil
                    return w.cont
                }
                expired?.resume(returning: .timeout)
            }
        }
    }
}
