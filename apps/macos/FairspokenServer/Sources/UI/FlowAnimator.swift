import FairspokenUI
import FairspokenHost
import Foundation
import FairspokenCore
import QuartzCore

/// Turns host cues into short-lived particles and node pulses for the constellation.
/// Not observable on purpose: the Canvas reads it every frame inside a TimelineView,
/// and the TimelineView pauses (zero idle CPU) when `isAnimating` is false.
final class FlowAnimator {
    enum Node: Hashable {
        case client(String)
        case queue
        case worker(Int)
        case model(String)
    }

    enum Kind {
        case request   // audio/job travelling towards the model
        case result    // transcript metadata travelling back to the client
        case failure
    }

    struct Particle {
        var from: Node
        var to: Node
        var start: Double
        var duration: Double
        var kind: Kind
    }

    private(set) var particles: [Particle] = []
    /// Last time a node lit up (for glow rings).
    private(set) var pulses: [Node: (time: Double, kind: Kind)] = [:]
    /// When each job's particle reaches the queue, so the dispatch leg starts after it.
    private var queueArrival: [String: Double] = [:]
    private var workerArrival: [Int: Double] = [:]

    static func now() -> Double { CACurrentMediaTime() }

    func reset() {
        particles.removeAll()
        pulses.removeAll()
        queueArrival.removeAll()
        workerArrival.removeAll()
    }

    func isAnimating(at t: Double = now()) -> Bool {
        particles.contains { t < $0.start + $0.duration } || pulses.values.contains { t - $0.time < 1.2 }
    }

    func play(_ cues: [FlowCue]) {
        let t = Self.now()
        prune(t)
        for cue in cues {
            switch cue {
            case .streamStarted(let client):
                pulse(.client(client), at: t, .request)
            case .streamFinished(let client):
                pulse(.client(client), at: t, .request)
            case .queued(let job, let client):
                particles.append(Particle(from: .client(client), to: .queue, start: t, duration: 0.8, kind: .request))
                queueArrival[job] = t + 0.8
            case .dispatched(let job, _, let worker):
                let start = max(t, queueArrival.removeValue(forKey: job) ?? t)
                particles.append(Particle(from: .queue, to: .worker(worker), start: start, duration: 0.7, kind: .request))
                workerArrival[worker] = start + 0.7
                pulse(.queue, at: start, .request)
            case .completed(_, let client, let worker, let model):
                let start = max(t, workerArrival.removeValue(forKey: worker) ?? t)
                particles.append(Particle(from: .worker(worker), to: .model(model), start: start, duration: 0.5, kind: .request))
                particles.append(Particle(from: .model(model), to: .client(client), start: start + 0.5, duration: 1.3, kind: .result))
                pulse(.worker(worker), at: start, .result)
                pulse(.model(model), at: start + 0.5, .result)
                pulse(.client(client), at: start + 1.8, .result)
            case .failed(let job, let client, let worker):
                queueArrival.removeValue(forKey: job)
                let from: Node = worker.map { .worker($0) } ?? .queue
                particles.append(Particle(from: from, to: .client(client), start: t, duration: 0.8, kind: .failure))
                pulse(from, at: t, .failure)
            }
        }
    }

    private func pulse(_ node: Node, at time: Double, _ kind: Kind) {
        pulses[node] = (time, kind)
    }

    private func prune(_ t: Double) {
        particles.removeAll { t > $0.start + $0.duration + 0.1 }
        if particles.count > 400 { particles.removeFirst(particles.count - 400) }
        pulses = pulses.filter { t - $0.value.time < 2 }
        if queueArrival.count > 256 { queueArrival.removeAll() }
    }
}
