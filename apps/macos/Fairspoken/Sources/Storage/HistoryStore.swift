import Foundation
import FairspokenCore
import Observation
import OSLog

/// Recent dictations (`transcript-history.json`, 50 items, Rust schema) and lifetime usage
/// (`usage-stats.json`). Both import once from the Tauri app. Phase 1 replaces the simple
/// list with full history/notes behind the same file formats.
@Observable
final class HistoryStore {
    private(set) var items: [TranscriptHistoryItem] = []
    private(set) var usage = UsageStats()

    @ObservationIgnored private var usageRaw: [String: JSONValue] = [:]
    @ObservationIgnored private let directory: URL
    @ObservationIgnored private let persists: Bool
    private static let log = Logger(subsystem: AppInfo.bundleID, category: "history")

    init(directory: URL = AppInfo.supportDirectory, persists: Bool = !AppInfo.isDemo) {
        self.directory = directory
        self.persists = persists
        if persists {
            items = Self.load([TranscriptHistoryItem].self, "transcript-history.json", in: directory) ?? []
            if let data = Self.data("usage-stats.json", in: directory) {
                usage = (try? JSONDecoder().decode(UsageStats.self, from: data)) ?? UsageStats()
                usageRaw = (try? JSONDecoder().decode(JSONValue.self, from: data))?.objectValue ?? [:]
            }
        } else {
            seedDemo()
        }
    }

    var historyURL: URL { directory.appendingPathComponent("transcript-history.json") }
    var usageURL: URL { directory.appendingPathComponent("usage-stats.json") }

    func add(_ item: TranscriptHistoryItem) {
        items = TranscriptHistory.adding(item, to: items)
        usage.record(words: item.wordCount, recordingSeconds: item.durationSeconds, at: item.date)
        save()
    }

    func delete(_ id: String) {
        items.removeAll { $0.id == id }
        save()
    }

    /// Latency the user feels: release → text, median of recent dictations.
    var typicalLatencyMs: Int? {
        let values = items.prefix(20).compactMap { $0.timings?.totalMs }.filter { $0 > 0 }.sorted()
        guard !values.isEmpty else { return nil }
        return values[values.count / 2]
    }

    private func save() {
        guard persists else { return }
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .withoutEscapingSlashes]
            try encoder.encode(items).write(to: historyURL, options: .atomic)
            try JSONMerge.encode(usage, over: usageRaw).write(to: usageURL, options: .atomic)
        } catch {
            Self.log.error("Saving history failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    private static func data(_ name: String, in dir: URL) -> Data? {
        if let d = try? Data(contentsOf: dir.appendingPathComponent(name)) { return d }
        // One-time import from the Tauri app; written to our directory on first save.
        guard let d = try? Data(contentsOf: AppInfo.tauriDirectory.appendingPathComponent(name)) else { return nil }
        try? d.write(to: dir.appendingPathComponent(name), options: .atomic)
        return d
    }

    private static func load<T: Decodable>(_ type: T.Type, _ name: String, in dir: URL) -> T? {
        guard let d = data(name, in: dir) else { return nil }
        return try? JSONDecoder().decode(type, from: d)
    }

    // MARK: Demo data (screenshots, `--demo`)

    private func seedDemo() {
        let now = Date()
        let samples: [(String, String, String, Double, Int, Int)] = [
            ("Patient reports intermittent chest tightness on exertion for three weeks, no radiation, settles with rest. ECG today, bloods including troponin and lipids.", "Socrates", "com.socrates.pms", 21.4, 182, 7),
            ("Referral letter to cardiology outpatients: please assess for stable angina, family history of IHD, ex-smoker, BP well controlled on amlodipine.", "Mail", "com.apple.mail", 17.9, 164, 31),
            ("Can we move the Thursday practice meeting to 1 pm? The new nurse starts that morning and I'd like her to sit in.", "Slack", "com.tinyspeck.slackmacgap", 7.2, 121, 58),
            ("Reminder to call the pharmacy about the repeat prescription for Mrs O'Keeffe, the amoxicillin dose needs checking.", "Notes", "com.apple.Notes", 8.6, 133, 96),
            ("Thanks for the update. I've attached the audit results and we can go through them on Friday.", "Outlook", "com.microsoft.Outlook", 5.4, 117, 180),
            ("Review in two weeks, sooner if symptoms worsen. Safety-netting advice given and understood.", "Socrates", "com.socrates.pms", 4.8, 109, 260),
        ]
        items = samples.enumerated().map { i, s in
            TranscriptHistoryItem(id: "demo-\(i)", createdAt: Int64((now.timeIntervalSince1970 - Double(s.5 * 60)) * 1000),
                                  text: s.0, backend: "parakeet", location: "local", durationSeconds: s.3,
                                  timings: DictationTimings(transcribeMs: s.4 - 20, speechModelMs: s.4 - 60, totalMs: s.4),
                                  model: "parakeet-tdt-0.6b-v3", appName: s.1, appBundleId: s.2)
        }
        var u = UsageStats()
        let perDay = [1_420, 2_310, 980, 2_760, 1_890, 260, 0, 1_640, 2_120, 1_310, 2_980, 2_240, 410, 1_186]
        for (ago, words) in perDay.enumerated() where words > 0 {
            let day = now.addingTimeInterval(-Double(perDay.count - 1 - ago) * 86_400)
            let n = max(1, words / 42)
            for _ in 0..<n { u.record(words: words / n, recordingSeconds: Double(words / n) / 2.4, at: day) }
        }
        u.totalWords += 418_000
        u.totalRecordingSeconds += 418_000 / 2.4
        u.totalDictations += 10_900
        usage = u
    }
}
