import Foundation
import MultiVoiceCore
import os

/// Lock-protected mic level shared between the audio thread (writer) and the UI
/// (`TimelineView` readers poll it per frame, so no main-actor hops per buffer).
nonisolated final class LevelMeter: @unchecked Sendable {
    struct Snapshot {
        var level: Float
        var history: [Float]
    }

    static let historyLength = 48
    private let state = OSAllocatedUnfairLock(initialState: (level: Float(0), ring: [Float](repeating: 0, count: LevelMeter.historyLength), head: 0))

    func push(rms: Float) {
        let value = PCM.meterLevel(rms: rms)
        state.withLock { s in
            // Fast attack, slower release reads as "alive" without jitter.
            s.level = value > s.level ? value : s.level * 0.75 + value * 0.25
            s.ring[s.head] = s.level
            s.head = (s.head + 1) % LevelMeter.historyLength
        }
    }

    func reset() {
        state.withLock { s in
            s.level = 0
            s.ring = [Float](repeating: 0, count: LevelMeter.historyLength)
            s.head = 0
        }
    }

    func snapshot() -> Snapshot {
        state.withLock { s in
            let ordered = Array(s.ring[s.head...] + s.ring[..<s.head])
            return Snapshot(level: s.level, history: ordered)
        }
    }
}
