import AppKit

/// Start/stop/done cues, preloaded so playback never delays capture start.
@MainActor
enum Sounds {
    private static let start = NSSound(named: "Tink")
    private static let stop = NSSound(named: "Pop")
    private static let error = NSSound(named: "Basso")

    static func play(_ kind: Kind, enabled: Bool) {
        guard enabled else { return }
        let sound: NSSound? = switch kind {
        case .start: start
        case .stop: stop
        case .error: error
        }
        sound?.stop()
        sound?.volume = 0.35
        sound?.play()
    }

    enum Kind { case start, stop, error }
}
