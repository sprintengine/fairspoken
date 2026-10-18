import Foundation
import Testing
@testable import FairspokenCore

@Suite("Training data")
struct TrainingDataTests {
    private func temporaryRoot() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("training-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func entry(_ id: String, seconds: Double = 2) -> TrainingEntry {
        TrainingEntry(id: id, createdAt: 1_792_326_583, durationSeconds: seconds, speechModel: "parakeet-ultra",
                      appCategory: "code", rawText: "ask cloud code", finalText: "Ask cloud code.")
    }

    @Test func entryMatchesTheRustCaptureEntry() throws {
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(entry("a"))) as? [String: Any]
        #expect(Set(json?.keys.map { $0 } ?? []) ==
            ["id", "createdAt", "durationSeconds", "speechModel", "appCategory", "rawText", "finalText"])
        // A line the Tauri app wrote, with the optional fields present.
        let rust = #"{"id":"b","createdAt":1,"durationSeconds":4.2,"speechModel":"parakeet-tdt-0.6b-v3","appCategory":"email","rawText":"hi","polishedText":"Hi.","finalText":"Hi.","editedText":"Hello."}"#
        let decoded = try JSONDecoder().decode(TrainingEntry.self, from: Data(rust.utf8))
        #expect(decoded.editedText == "Hello.")
        #expect(decoded.polishedText == "Hi.")
    }

    @Test func recordsListsAndDeletes() throws {
        let root = try temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let store = TrainingDataStore(root: root)
        let samples = [Float](repeating: 0.25, count: 32_000)
        try store.record(entry("one"), samples: samples)
        try store.record(entry("two", seconds: 3), samples: samples)
        #expect(store.entries().map(\.id) == ["one", "two"])
        #expect(FileManager.default.fileExists(atPath: store.audioURL(for: "one").path))
        let meta = try Data(contentsOf: root.appendingPathComponent("one/meta.json"))
        #expect(try JSONDecoder().decode(TrainingEntry.self, from: meta) == entry("one"))
        let mode = try FileManager.default.attributesOfItem(atPath: store.audioURL(for: "one").path)[.posixPermissions] as? Int
        #expect(mode == 0o600)

        try store.delete("one")
        #expect(store.entries().map(\.id) == ["two"])
        #expect(!FileManager.default.fileExists(atPath: root.appendingPathComponent("one").path))

        try "keep me".write(to: root.appendingPathComponent("notes.txt"), atomically: true, encoding: .utf8)
        try store.deleteAll()
        #expect(store.entries().isEmpty)
        #expect(!FileManager.default.fileExists(atPath: root.appendingPathComponent("two").path))
        // Only what the index names is removed.
        #expect(FileManager.default.fileExists(atPath: root.appendingPathComponent("notes.txt").path))
    }

    @Test func skipsDamagedIndexLines() throws {
        let root = try temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let store = TrainingDataStore(root: root)
        try store.record(entry("one"), samples: [0.1])
        let index = root.appendingPathComponent(TrainingDataStore.indexFile)
        let handle = try FileHandle(forWritingTo: index)
        try handle.seekToEnd()
        try handle.write(contentsOf: Data("{not json\n".utf8))
        try handle.close()
        try store.record(entry("two"), samples: [0.1])
        #expect(store.entries().map(\.id) == ["one", "two"])
    }

    @Test func neverCreatesAMissingFolder() throws {
        let missing = FileManager.default.temporaryDirectory.appendingPathComponent("unmounted-\(UUID().uuidString)/Training")
        let store = TrainingDataStore(root: missing)
        #expect(!store.isAvailable)
        #expect(throws: TrainingDataStore.StoreError.folderUnavailable(missing.path)) {
            try store.record(entry("one"), samples: [0.1])
        }
        #expect(!FileManager.default.fileExists(atPath: missing.deletingLastPathComponent().path))
    }

    @Test func wavHeaderAndSamples() {
        let wav = TrainingDataStore.wav([0, 1, -1, 0.5])
        #expect(wav.count == 44 + 8)
        #expect(String(decoding: wav[0..<4], as: UTF8.self) == "RIFF")
        #expect(String(decoding: wav[8..<12], as: UTF8.self) == "WAVE")
        func u32(_ at: Int) -> UInt32 { wav[at..<at + 4].enumerated().reduce(0) { $0 | UInt32($1.element) << (8 * $1.offset) } }
        func i16(_ at: Int) -> Int16 { Int16(bitPattern: UInt16(wav[at]) | UInt16(wav[at + 1]) << 8) }
        #expect(u32(4) == 36 + 8)
        #expect(u32(24) == 16_000)
        #expect(u32(40) == 8)
        #expect([i16(44), i16(46), i16(48), i16(50)] == [0, 32767, -32767, 16384])
    }

    @Test func summaryAndReadiness() {
        let summary = TrainingSummary([entry("a", seconds: 1800), entry("b", seconds: 1800)])
        #expect(summary.count == 2)
        #expect(summary.isEnoughToTrain)
        #expect(summary.bytes == 2 * (44 + 1800 * 32_000))
        #expect(abs(summary.progress - 0.1) < 0.0001)
        #expect(!TrainingSummary([entry("c", seconds: 60)]).isEnoughToTrain)
    }

    @Test func categoriesFollowTheRustTable() {
        #expect(AppCategory.of(bundleID: "com.tinyspeck.slackmacgap") == .messaging)
        #expect(AppCategory.of(bundleID: "com.jetbrains.rustrover") == .code)
        #expect(AppCategory.of(bundleID: "com.mitchellh.ghostty") == .terminal)
        #expect(AppCategory.of(bundleID: "com.google.Chrome") == .other)
        #expect(AppCategory.of(bundleID: nil) == .other)
    }

    @Test func settingsKeepTheCaptureChoiceAndFolder() throws {
        let s = try JSONDecoder().decode(AppSettings.self, from: Data(#"{"trainingCapture":true,"trainingDataFolder":" /Volumes/workdrive/x "}"#.utf8))
        #expect(s.trainingCapture)
        #expect(s.trainingDataFolder == "/Volumes/workdrive/x")
        #expect(!AppSettings().trainingCapture)
    }
}
