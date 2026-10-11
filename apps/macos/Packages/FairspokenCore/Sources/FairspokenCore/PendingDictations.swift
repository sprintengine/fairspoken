import Foundation

// A dictation's audio is written to disk as it is captured and deleted only once its transcript
// has been delivered. Until then it is a pending dictation: `<id>.wav` (16-bit PCM, mono,
// 16 kHz, so any player opens it and `POST /v1/transcriptions` takes it as is) and `<id>.json`.

/// One kept recording, `<id>.json` next to its `<id>.wav`.
public struct PendingDictation: Codable, Equatable, Identifiable, Sendable {
    public enum State: String, Codable, Sendable {
        /// Being captured.
        case recording
        /// Captured, with a transcription under way.
        case transcribing
        /// Every attempt so far failed; waiting to be tried again.
        case failed
    }

    public var id: String
    /// Milliseconds since 1970, when the recording started.
    public var createdAt: Int64
    public var durationSeconds: Double = 0
    public var state: State = .recording
    /// Failed transcription attempts so far.
    public var attempts = 0
    public var lastError: String?
    public var lastAttemptAt: Int64?
    public var appName: String?
    public var appBundleId: String?

    public init(id: String, createdAt: Date, appName: String? = nil, appBundleId: String? = nil) {
        self.id = id
        self.createdAt = Int64(createdAt.timeIntervalSince1970 * 1000)
        self.appName = appName
        self.appBundleId = appBundleId
    }

    public var date: Date { Date(timeIntervalSince1970: Double(createdAt) / 1000) }
    public var lastAttemptDate: Date? { lastAttemptAt.map { Date(timeIntervalSince1970: Double($0) / 1000) } }
}

/// The folder of pending dictations. Every call reads or writes the files directly, so any
/// number of copies can share a folder and a restart loses nothing.
public struct PendingDictationStore: Sendable {
    public static let sampleRate = 16_000

    public let directory: URL

    public init(directory: URL) {
        self.directory = directory
    }

    public func audioURL(_ id: String) -> URL { directory.appendingPathComponent("\(id).wav") }
    func metadataURL(_ id: String) -> URL { directory.appendingPathComponent("\(id).json") }

    /// Starts keeping a new recording: its metadata now, its audio as `append` delivers it.
    public func begin(_ item: PendingDictation) throws -> PendingAudioWriter {
        try prepareDirectory()
        try save(item)
        return try PendingAudioWriter(url: audioURL(item.id), sampleRate: Self.sampleRate)
    }

    /// Keeps a whole recording at once, when it couldn't be written as it was captured.
    public func write(_ item: PendingDictation, samples16k: [Float]) throws {
        try prepareDirectory()
        try WAVFile.encode(PCM.int16(from: samples16k), sampleRate: Self.sampleRate).write(to: audioURL(item.id), options: .atomic)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: audioURL(item.id).path)
        try save(item)
    }

    public func item(_ id: String) -> PendingDictation? {
        guard let data = try? Data(contentsOf: metadataURL(id)) else { return nil }
        return try? JSONDecoder().decode(PendingDictation.self, from: data)
    }

    public func save(_ item: PendingDictation) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let url = metadataURL(item.id)
        try encoder.encode(item).write(to: url, options: .atomic)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
    }

    /// Records a state change; a dictation already removed stays removed.
    public func update(_ id: String, _ change: (inout PendingDictation) -> Void) {
        guard var item = item(id) else { return }
        change(&item)
        try? save(item)
    }

    /// Deletes the audio and its metadata (after delivery, or when the user discards it).
    public func remove(_ id: String) {
        try? FileManager.default.removeItem(at: audioURL(id))
        try? FileManager.default.removeItem(at: metadataURL(id))
    }

    /// Every pending dictation that has audio, oldest first.
    public func all() -> [PendingDictation] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? []
        return names.filter { $0.hasSuffix(".json") }
            .compactMap { item(String($0.dropLast(5))) }
            .filter { FileManager.default.fileExists(atPath: audioURL($0.id).path) }
            .sorted { $0.createdAt < $1.createdAt }
    }

    /// At launch: whatever the last run left recording or transcribing (it quit, crashed or the
    /// Mac lost power) becomes a failed dictation to retry, and audio without metadata gets some.
    /// Metadata whose audio is gone is dropped.
    @discardableResult
    public func recoverInterrupted(now: Date = Date()) -> [PendingDictation] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? []
        for name in names where name.hasSuffix(".json") {
            let id = String(name.dropLast(5))
            if !FileManager.default.fileExists(atPath: audioURL(id).path) { try? FileManager.default.removeItem(at: metadataURL(id)) }
        }
        for name in names where name.hasSuffix(".wav") {
            let id = String(name.dropLast(4))
            var item = item(id) ?? PendingDictation(id: id, createdAt: Self.creationDate(audioURL(id)) ?? now)
            guard item.state != .failed else { continue }
            if item.state == .recording { WAVFile.repairHeader(audioURL(id)) }
            item.durationSeconds = WAVFile.durationSeconds(audioURL(id)) ?? item.durationSeconds
            item.state = .failed
            item.lastError = item.lastError ?? "Fairspoken quit before this dictation was transcribed"
            try? save(item)
        }
        return all()
    }

    /// The recording as a WAV file's bytes, to upload.
    public func wav(_ id: String) throws -> Data { try Data(contentsOf: audioURL(id)) }

    /// The recording as 16 kHz Float32, for the on-device engine.
    public func samples(_ id: String) throws -> [Float] {
        let decoded = try WAVFile.decode(wav(id))
        return decoded.samples.map { Float($0) / 32768 }
    }

    private func prepareDirectory() throws {
        guard !FileManager.default.fileExists(atPath: directory.path) else { return }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        // Audio that exists only until it is transcribed has no place in a backup.
        var url = directory
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
    }

    private static func creationDate(_ url: URL) -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: url.path))?[.creationDate] as? Date
    }
}

/// Appends a recording to its WAV file as it is captured. Writes happen on a serial queue, never
/// on the audio thread; the header's sizes are filled in by `finish` (or by
/// `PendingDictationStore.recoverInterrupted` if the app never got that far).
public final class PendingAudioWriter: @unchecked Sendable {
    public let url: URL
    private let handle: FileHandle
    private let queue = DispatchQueue(label: "pending-dictation-writer", qos: .userInitiated)
    private var dataBytes = 0
    private var closed = false
    private var failure: Error?

    init(url: URL, sampleRate: Int) throws {
        self.url = url
        guard FileManager.default.createFile(atPath: url.path, contents: WAVFile.header(sampleRate: sampleRate, dataBytes: 0),
                                             attributes: [.posixPermissions: 0o600]) else {
            throw CocoaError(.fileWriteUnknown, userInfo: [NSFilePathErrorKey: url.path])
        }
        handle = try FileHandle(forWritingTo: url)
        try handle.seekToEnd()
    }

    /// 16 kHz mono samples; called from the audio thread, returns at once.
    public func append(_ samples16k: [Float]) {
        guard !samples16k.isEmpty else { return }
        queue.async {
            guard !self.closed, self.failure == nil else { return }
            let pcm = PCM.int16(from: samples16k)
            let data = pcm.withUnsafeBufferPointer { Data(buffer: $0) }
            do {
                try self.handle.write(contentsOf: data)
                self.dataBytes += data.count
            } catch {
                self.failure = error
            }
        }
    }

    /// Writes what is queued, completes the header and flushes the file to disk. Returns the
    /// number of samples kept; throws if the disk refused any of them.
    @discardableResult
    public func finish() throws -> Int {
        try queue.sync {
            if closed { return dataBytes / 2 }
            closed = true
            defer { try? handle.close() }
            if let failure { throw failure }
            try handle.seek(toOffset: 4)
            try handle.write(contentsOf: WAVFile.le32(36 + dataBytes))
            try handle.seek(toOffset: 40)
            try handle.write(contentsOf: WAVFile.le32(dataBytes))
            try handle.synchronize()
            return dataBytes / 2
        }
    }

    /// Waits for the queued writes (tests).
    func drain() { queue.sync {} }

    /// Stops writing and deletes the file (a cancelled or rejected recording).
    public func discard() {
        queue.sync {
            if !closed { try? handle.close() }
            closed = true
        }
        try? FileManager.default.removeItem(at: url)
    }
}

/// 16-bit PCM mono WAV with the canonical 44-byte header.
public enum WAVFile {
    public static let headerSize = 44

    public enum DecodeError: Error, Equatable {
        case notWAV
        case unsupported
    }

    public static func header(sampleRate: Int, dataBytes: Int) -> Data {
        var d = Data()
        d.append(contentsOf: Array("RIFF".utf8)); d.append(le32(36 + dataBytes))
        d.append(contentsOf: Array("WAVE".utf8))
        d.append(contentsOf: Array("fmt ".utf8)); d.append(le32(16))
        d.append(le16(1)); d.append(le16(1))                      // PCM, mono
        d.append(le32(sampleRate)); d.append(le32(sampleRate * 2)) // byte rate
        d.append(le16(2)); d.append(le16(16))                     // block align, bits
        d.append(contentsOf: Array("data".utf8)); d.append(le32(dataBytes))
        return d
    }

    public static func encode(_ samples: [Int16], sampleRate: Int) -> Data {
        var data = header(sampleRate: sampleRate, dataBytes: samples.count * 2)
        samples.withUnsafeBufferPointer { data.append(Data(buffer: $0)) }
        return data
    }

    /// Reads a file written by `encode` or `PendingAudioWriter`. The data chunk runs to the end
    /// of the file when its size was never filled in.
    public static func decode(_ data: Data) throws(DecodeError) -> (samples: [Int16], sampleRate: Int) {
        let bytes = [UInt8](data)
        guard bytes.count >= headerSize, bytes[0..<4].elementsEqual("RIFF".utf8), bytes[8..<12].elementsEqual("WAVE".utf8) else {
            throw .notWAV
        }
        var offset = 12
        var rate = 0
        while offset + 8 <= bytes.count {
            let id = String(decoding: bytes[offset..<offset + 4], as: UTF8.self)
            let size = read32(bytes, offset + 4)
            let body = offset + 8
            if id == "fmt " {
                guard body + 16 <= bytes.count, read16(bytes, body) == 1, read16(bytes, body + 2) == 1,
                      read16(bytes, body + 14) == 16 else { throw .unsupported }
                rate = read32(bytes, body + 4)
            } else if id == "data" {
                guard rate > 0 else { throw .unsupported }
                let end = size == 0 || body + size > bytes.count ? bytes.count : body + size
                let count = (end - body) / 2
                var samples = [Int16](repeating: 0, count: count)
                for i in 0..<count {
                    samples[i] = Int16(bitPattern: UInt16(bytes[body + 2 * i]) | UInt16(bytes[body + 2 * i + 1]) << 8)
                }
                return (samples, rate)
            }
            offset = body + size + (size & 1)
        }
        throw .notWAV
    }

    /// Length of a file written here, from its size on disk.
    public static func durationSeconds(_ url: URL) -> Double? {
        guard let size = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? Int, size >= headerSize,
              let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        guard let head = try? handle.read(upToCount: headerSize), head.count == headerSize else { return nil }
        let rate = read32([UInt8](head), 24)
        return rate > 0 ? Double(size - headerSize) / 2 / Double(rate) : nil
    }

    /// Fills in the sizes of a file whose writer never finished.
    public static func repairHeader(_ url: URL) {
        guard let size = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? Int, size >= headerSize,
              let handle = try? FileHandle(forWritingTo: url) else { return }
        defer { try? handle.close() }
        let dataBytes = (size - headerSize) & ~1
        try? handle.seek(toOffset: 4)
        try? handle.write(contentsOf: le32(36 + dataBytes))
        try? handle.seek(toOffset: 40)
        try? handle.write(contentsOf: le32(dataBytes))
    }

    static func le32(_ v: Int) -> Data { withUnsafeBytes(of: UInt32(truncatingIfNeeded: v).littleEndian) { Data($0) } }
    static func le16(_ v: Int) -> Data { withUnsafeBytes(of: UInt16(truncatingIfNeeded: v).littleEndian) { Data($0) } }
    private static func read16(_ b: [UInt8], _ o: Int) -> Int { Int(b[o]) | Int(b[o + 1]) << 8 }
    private static func read32(_ b: [UInt8], _ o: Int) -> Int { read16(b, o) | read16(b, o + 2) << 16 }
}
