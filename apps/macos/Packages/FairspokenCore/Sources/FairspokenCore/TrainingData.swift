import Foundation

/// The kind of app a dictation went into: `AppCategory` in `src-tauri/src/app_categories.rs`
/// (same bundle-id prefixes, same ids). Only the category is kept with a recording, never the
/// app's name or window contents.
public enum AppCategory: String, Sendable, CaseIterable {
    case messaging, email, docs, code, terminal, other

    /// Bundle-id prefixes, as in the Rust table (JetBrains ships many ids under `com.jetbrains.`).
    static let table: [(String, AppCategory)] = [
        ("com.tinyspeck.slackmacgap", .messaging), ("com.apple.MobileSMS", .messaging),
        ("net.whatsapp.WhatsApp", .messaging), ("com.hnc.Discord", .messaging),
        ("org.telegram.desktop", .messaging), ("ru.keepcoder.Telegram", .messaging),
        ("com.microsoft.teams2", .messaging), ("us.zoom.xos", .messaging),
        ("com.apple.mail", .email), ("com.microsoft.Outlook", .email), ("com.superhuman.electron", .email),
        ("com.readdle.SparkDesktop", .email), ("com.mimestream.Mimestream", .email),
        ("com.apple.iWork.Pages", .docs), ("com.microsoft.Word", .docs), ("com.apple.Notes", .docs),
        ("md.obsidian", .docs), ("notion.id", .docs), ("com.literatureandlatte.scrivener3", .docs),
        ("com.microsoft.VSCode", .code), ("com.todesktop.230313mzl4w4u92", .code), ("com.jetbrains.", .code),
        ("dev.zed.Zed", .code), ("com.sublimetext.4", .code), ("com.apple.dt.Xcode", .code),
        ("com.apple.Terminal", .terminal), ("com.googlecode.iterm2", .terminal), ("dev.warp.Warp-Stable", .terminal),
        ("com.github.wez.wezterm", .terminal), ("net.kovidgoyal.kitty", .terminal),
        ("com.mitchellh.ghostty", .terminal), ("co.zeit.hyper", .terminal),
    ]

    public static func of(bundleID: String?) -> AppCategory {
        guard let bundleID, !bundleID.isEmpty else { return .other }
        return table.first { bundleID.hasPrefix($0.0) }?.1 ?? .other
    }
}

/// One kept dictation: `CaptureEntry` in `src-tauri/src/training_capture.rs`, field for field
/// (camelCase, absent optionals omitted), so both apps' folders read the same way and
/// `training/` tools take either.
public struct TrainingEntry: Codable, Equatable, Sendable, Identifiable {
    public var id: String
    /// Epoch seconds.
    public var createdAt: Int64
    public var durationSeconds: Double
    /// The model that heard it (`parakeet-ultra`…), or what the host reported.
    public var speechModel: String
    /// `AppCategory` id of the app dictated into.
    public var appCategory: String
    /// The speech model's text, before corrections.
    public var rawText: String
    /// The polish model's text, when polish changed anything (never set by the Mac app yet).
    public var polishedText: String?
    /// What was inserted.
    public var finalText: String
    /// The dictation as the user left it, when an edit was seen.
    public var editedText: String?

    public init(id: String = UUID().uuidString.lowercased(), createdAt: Int64, durationSeconds: Double, speechModel: String,
                appCategory: String, rawText: String, polishedText: String? = nil, finalText: String, editedText: String? = nil) {
        self.id = id
        self.createdAt = createdAt
        self.durationSeconds = durationSeconds
        self.speechModel = speechModel
        self.appCategory = appCategory
        self.rawText = rawText
        self.polishedText = polishedText
        self.finalText = finalText
        self.editedText = editedText
    }

    public var date: Date { Date(timeIntervalSince1970: TimeInterval(createdAt)) }
}

/// Totals for the Post-training screen.
public struct TrainingSummary: Equatable, Sendable {
    public var count = 0
    public var seconds: Double = 0
    public var bytes: Int64 = 0

    public init() {}

    public init(_ entries: [TrainingEntry]) {
        count = entries.count
        seconds = entries.reduce(0) { $0 + $1.durationSeconds }
        // A WAV is a 44-byte header and 2 bytes a sample: exact without walking the folder.
        bytes = entries.reduce(0) { $0 + 44 + Int64(($1.durationSeconds * TrainingDataStore.sampleRate).rounded()) * 2 }
    }

    /// How far toward `TrainingReadiness.recommendedSeconds`, 0…1.
    public var progress: Double { min(1, seconds / TrainingReadiness.recommendedSeconds) }
    public var isEnoughToTrain: Bool { seconds >= TrainingReadiness.minimumSeconds }
}

/// How much speech post-training wants. Single-speaker adaptation papers report most of the
/// gain from tens of minutes of transcribed speech; labels from a stronger model are noisier
/// than hand transcripts, so we ask for more.
public enum TrainingReadiness {
    public static let minimumSeconds: Double = 60 * 60
    public static let recommendedSeconds: Double = 10 * 60 * 60
}

/// The training-data folder, in the Rust layout:
///
/// ```text
/// <root>/
///   index.jsonl          one TrainingEntry per line
///   <id>/audio.wav       16 kHz mono 16-bit PCM
///   <id>/meta.json       the same TrainingEntry
/// ```
///
/// Never creates `root` itself: on an external drive that isn't mounted, creating it would
/// quietly fill the startup disk under `/Volumes`. The app creates it when the folder is chosen.
/// Not thread-safe; the app calls it from one serial queue.
public struct TrainingDataStore: Sendable {
    public static let sampleRate: Double = 16_000
    public static let indexFile = "index.jsonl"
    public static let audioFile = "audio.wav"
    public static let metaFile = "meta.json"

    public enum StoreError: LocalizedError, Equatable {
        case folderUnavailable(String)

        public var errorDescription: String? {
            switch self {
            case .folderUnavailable(let path): "The recordings folder isn't available (\(path)). Is the drive connected?"
            }
        }
    }

    public let root: URL

    public init(root: URL) { self.root = root }

    public var isAvailable: Bool {
        var isDirectory: ObjCBool = false
        return FileManager.default.fileExists(atPath: root.path, isDirectory: &isDirectory) && isDirectory.boolValue
    }

    public func audioURL(for id: String) -> URL {
        root.appendingPathComponent(id, isDirectory: true).appendingPathComponent(Self.audioFile)
    }

    /// Writes the dictation's folder, then appends it to the index.
    public func record(_ entry: TrainingEntry, samples: [Float]) throws {
        guard isAvailable else { throw StoreError.folderUnavailable(root.path) }
        let folder = root.appendingPathComponent(entry.id, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
        Self.restrict(folder, to: 0o700)
        try Self.private(Self.wav(samples), at: folder.appendingPathComponent(Self.audioFile))
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        try Self.private(encoder.encode(entry), at: folder.appendingPathComponent(Self.metaFile))
        try appendToIndex(entry)
    }

    /// Every readable entry, oldest first; a damaged line is skipped, not fatal.
    public func entries() -> [TrainingEntry] {
        guard let data = try? Data(contentsOf: root.appendingPathComponent(Self.indexFile)) else { return [] }
        let decoder = JSONDecoder()
        return data.split(separator: UInt8(ascii: "\n")).compactMap { try? decoder.decode(TrainingEntry.self, from: Data($0)) }
    }

    public func delete(_ id: String) throws {
        guard isAvailable else { throw StoreError.folderUnavailable(root.path) }
        let kept = entries().filter { $0.id != id }
        try writeIndex(kept)
        let folder = root.appendingPathComponent(id, isDirectory: true)
        if !id.isEmpty, !id.contains("/"), FileManager.default.fileExists(atPath: folder.path) {
            try FileManager.default.removeItem(at: folder)
        }
    }

    /// Removes every kept dictation, and only those: the index and the folders it names.
    public func deleteAll() throws {
        guard isAvailable else { throw StoreError.folderUnavailable(root.path) }
        for entry in entries() where !entry.id.isEmpty && !entry.id.contains("/") {
            try? FileManager.default.removeItem(at: root.appendingPathComponent(entry.id, isDirectory: true))
        }
        try? FileManager.default.removeItem(at: root.appendingPathComponent(Self.indexFile))
    }

    // MARK: Files

    private func appendToIndex(_ entry: TrainingEntry) throws {
        var line = try JSONEncoder().encode(entry)
        line.append(UInt8(ascii: "\n"))
        let url = root.appendingPathComponent(Self.indexFile)
        if !FileManager.default.fileExists(atPath: url.path) {
            try Self.private(line, at: url)
            return
        }
        let handle = try FileHandle(forWritingTo: url)
        defer { try? handle.close() }
        try handle.seekToEnd()
        try handle.write(contentsOf: line)
    }

    private func writeIndex(_ entries: [TrainingEntry]) throws {
        let encoder = JSONEncoder()
        var payload = Data()
        for entry in entries {
            payload.append(try encoder.encode(entry))
            payload.append(UInt8(ascii: "\n"))
        }
        try Self.private(payload, at: root.appendingPathComponent(Self.indexFile))
    }

    /// Atomic, owner-only: kept dictations can hold anything the user said.
    private static func `private`(_ data: Data, at url: URL) throws {
        try data.write(to: url, options: .atomic)
        restrict(url, to: 0o600)
    }

    /// Best effort: exFAT and network volumes don't keep POSIX modes.
    private static func restrict(_ url: URL, to mode: Int) {
        try? FileManager.default.setAttributes([.posixPermissions: mode], ofItemAtPath: url.path)
    }

    /// 16 kHz mono 16-bit PCM WAV.
    public static func wav(_ samples: [Float]) -> Data {
        let pcm = PCM.int16(from: samples)
        let dataBytes = UInt32(pcm.count * 2)
        let rate = UInt32(sampleRate)
        var out = Data(capacity: 44 + Int(dataBytes))
        func append<T: FixedWidthInteger>(_ value: T) { withUnsafeBytes(of: value.littleEndian) { out.append(contentsOf: $0) } }
        out.append(contentsOf: Array("RIFF".utf8)); append(36 + dataBytes)
        out.append(contentsOf: Array("WAVE".utf8))
        out.append(contentsOf: Array("fmt ".utf8)); append(UInt32(16))
        append(UInt16(1)); append(UInt16(1))  // PCM, mono
        append(rate); append(rate * 2)       // sample rate, byte rate
        append(UInt16(2)); append(UInt16(16)) // block align, bits per sample
        out.append(contentsOf: Array("data".utf8)); append(dataBytes)
        // Apple silicon is little-endian, as WAV is: the samples go in as they are.
        pcm.withUnsafeBytes { out.append(contentsOf: $0) }
        return out
    }
}
