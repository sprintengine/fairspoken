import Foundation

/// The update channel (docs: the update contract §1, §4). Stable is `X.Y.Z`; nightly is
/// `X.Y.Z-nightly.YYYYMMDD.N`. Both live in one appcast per app: nightly items carry
/// `<sparkle:channel>nightly</sparkle:channel>`, stable items carry no channel.
public enum UpdateChannel: String, CaseIterable, Identifiable, Sendable {
    case stable
    case nightly

    /// The UserDefaults key the choice is saved under. Absent only until the first launch, which
    /// saves the build's own channel (`resolveAndRemember`).
    public static let defaultsKey = "updateChannel"

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .stable: "Stable"
        case .nightly: "Nightly"
        }
    }

    /// One line each, shown under the picker.
    public var explanation: String {
        switch self {
        case .stable: "Releases that have already run as nightlies. Changes less often."
        case .nightly: "The newest changes, built every day before they reach Stable."
        }
    }

    /// Whether a version string is a nightly (`-nightly.` anywhere after the base version).
    public static func isNightly(version: String) -> Bool {
        version.contains("-nightly.")
    }

    /// The channel a build belongs to by its own version.
    public static func defaultChannel(forVersion version: String) -> UpdateChannel {
        isNightly(version: version) ? .nightly : .stable
    }

    /// The saved choice wins; with none (or an unknown value), the build's own channel.
    public static func resolve(saved: String?, version: String) -> UpdateChannel {
        if let saved, let channel = UpdateChannel(rawValue: saved) { return channel }
        return defaultChannel(forVersion: version)
    }

    /// `resolve`, and when nothing valid is saved yet, save what it resolved. An install that
    /// never touched the picker must stay on the channel it started on: a nightly install takes
    /// the promoted stable (stable items have no channel, so every install sees them), and if the
    /// channel still followed the build, that stable would silently move it off nightly for good.
    public static func resolveAndRemember(in defaults: UserDefaults, version: String) -> UpdateChannel {
        let saved = defaults.string(forKey: defaultsKey)
        let channel = resolve(saved: saved, version: version)
        if saved != channel.rawValue { defaults.set(channel.rawValue, forKey: defaultsKey) }
        return channel
    }

    /// What `SPUUpdaterDelegate.allowedChannels(for:)` answers. The default channel (stable
    /// items, which have no `<sparkle:channel>`) is always allowed by Sparkle.
    public var sparkleChannels: Set<String> {
        switch self {
        case .stable: []
        case .nightly: ["nightly"]
        }
    }

    /// Sparkle never downgrades: a nightly build on the Stable channel is offered the next stable
    /// cut after it (build numbers are timestamps), so Settings says so.
    public static func showsMoveToStableNote(channel: UpdateChannel, currentVersion: String) -> Bool {
        channel == .stable && isNightly(version: currentVersion)
    }
}
