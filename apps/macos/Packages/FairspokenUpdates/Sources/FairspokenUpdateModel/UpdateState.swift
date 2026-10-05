import Foundation

/// Where the app's update is (contract §6). Versions are display versions
/// (`CFBundleShortVersionString` / `sparkle:shortVersionString`).
public enum UpdateState: Equatable, Sendable {
    /// Up to date, or not checked yet.
    case idle
    /// A check the person asked for is running.
    case checking
    /// A newer version is offered and nothing is downloaded yet.
    case available(version: String)
    /// Downloading (then unpacking) the update. `progress` is 0...1, nil while unknown.
    case downloading(version: String, progress: Double?)
    /// Downloaded and ready: a restart installs it.
    case ready(version: String)
    /// The last check or install failed.
    case error(message: String)

    /// An update is waiting for the person (the Settings badge).
    public var isPending: Bool {
        switch self {
        case .available, .ready: true
        default: false
        }
    }

    /// The version this state is about, if any.
    public var version: String? {
        switch self {
        case .available(let v), .ready(let v), .downloading(let v, _): v
        default: nil
        }
    }

    /// What clicking the sidebar button does in this state.
    public var primaryAction: UpdateAction {
        switch self {
        case .idle, .error: .check
        case .checking, .downloading: .none
        case .available: .install
        case .ready: .relaunch
        }
    }
}

public enum UpdateAction: Equatable, Sendable {
    case none, check, install, relaunch
}

/// Download, then unpack, as one 0...1 ring: downloading fills 90 %, unpacking the rest.
public struct DownloadProgress: Equatable, Sendable {
    public var expectedLength: UInt64 = 0
    public var receivedLength: UInt64 = 0
    /// Unpacking progress, once it has started.
    public var extraction: Double?

    public init(expectedLength: UInt64 = 0, receivedLength: UInt64 = 0, extraction: Double? = nil) {
        self.expectedLength = expectedLength
        self.receivedLength = receivedLength
        self.extraction = extraction
    }

    public var fraction: Double? {
        if let extraction { return 0.9 + 0.1 * min(max(extraction, 0), 1) }
        guard expectedLength > 0 else { return nil }
        return 0.9 * min(Double(receivedLength) / Double(expectedLength), 1)
    }
}

extension UpdateState {
    /// The states by name, for development and screenshots (`-FAIRSPOKEN_UPDATE_STATE ready`).
    public static let sampleNames = ["idle", "checking", "available", "downloading", "ready", "error"]

    public static func sample(named name: String, version: String = "0.3.0") -> UpdateState? {
        switch name.lowercased() {
        case "idle": .idle
        case "checking": .checking
        case "available": .available(version: version)
        case "downloading": .downloading(version: version, progress: 0.62)
        case "ready": .ready(version: version)
        case "error": .error(message: "The network connection was lost.")
        default: nil
        }
    }
}

/// "Fairspoken 0.3.0 (Nightly)": the channel is the offered version's, not the picker's.
public func updateDisplayName(appName: String, version: String) -> String {
    "\(appName) \(version)" + (UpdateChannel.isNightly(version: version) ? " (Nightly)" : "")
}

/// Once per version: an offer the person has seen (and maybe waved away) is not offered again
/// by the next scheduled check. A check they asked for always answers.
public enum UpdateOfferLedger {
    public static let defaultsKey = "updateLastOfferedVersion"

    public static func shouldOffer(version: String, lastOffered: String?, userInitiated: Bool) -> Bool {
        userInitiated || version != lastOffered
    }
}
