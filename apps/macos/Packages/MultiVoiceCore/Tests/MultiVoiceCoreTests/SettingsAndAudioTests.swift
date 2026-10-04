import Foundation
import Testing
@testable import MultiVoiceCore

@Suite("Settings compatibility")
struct SettingsTests {
    /// A real `settings.json` as written by the Tauri app (tokens blanked).
    static let tauriJSON = """
    {"transcriptionLocation":"remote-host","model":"parakeet-ultra","remoteUrl":"http://192.168.0.35:48173/",
     "remoteAuthToken":"tok","remoteTimeoutSeconds":60,"cloudAuthToken":"","language":"en","audioDevice":"",
     "noiseSuppression":true,"echoCancellation":true,"inputGain":9,"postProcess":true,
     "vocabularyHints":["  Amoxicillin ","Amoxicillin","Dr   Byrne"],"transcriptCorrections":[{"from":"a","to":"b"}],
     "snippets":[],"alwaysOnTop":true,"interactionSounds":true,"maxRecordingSeconds":900,"noteRetentionMinutes":60,
     "useGpu":true,"recordingShortcut":"CommandOrControl+Shift+Digit1","recordingShortcutMode":"push-to-talk",
     "transcriptStackShortcut":"CommandOrControl+Shift+Digit2","insertAtCursor":true,"fnPushToTalk":true,
     "polishEnabled":false,"polishProvider":"local","polishModel":"speakoflow-mini","polishTones":{},
     "contextAwareness":true,"someFutureRustField":{"x":1}}
    """

    @Test func decodesAndNormalisesTheTauriFile() throws {
        let s = try JSONDecoder().decode(AppSettings.self, from: Data(Self.tauriJSON.utf8))
        #expect(s.transcriptionLocation == .remoteHost)
        #expect(s.model == "parakeet-ultra")
        #expect(s.remoteUrl == "http://192.168.0.35:48173")
        #expect(s.inputGain == 6)
        #expect(s.maxRecordingSeconds == 600)
        #expect(s.vocabularyHints == ["Amoxicillin", "Dr Byrne"])
        #expect(s.recordingShortcutMode == .pushToTalk)
        #expect(s.fnPushToTalk)
        #expect(s.transcriptCorrections.first?.wholePhrase == true)
        #expect(s.restoreClipboard)  // mac default when absent
    }

    @Test func unknownModelFallsBackToDefault() throws {
        let s = try JSONDecoder().decode(AppSettings.self, from: Data(#"{"model":"large-v3-turbo"}"#.utf8))
        #expect(s.model == SpeechModelCatalog.defaultModelID)
        #expect(s.language == "en")
    }

    @Test func saveKeepsKeysTheMacAppDoesNotModel() throws {
        let raw = try JSONDecoder().decode(JSONValue.self, from: Data(Self.tauriJSON.utf8)).objectValue ?? [:]
        var s = try JSONDecoder().decode(AppSettings.self, from: Data(Self.tauriJSON.utf8))
        s.language = "ga"
        let out = try JSONDecoder().decode(JSONValue.self, from: JSONMerge.encode(s, over: raw)).objectValue ?? [:]
        #expect(out["someFutureRustField"] == .object(["x": .number(1)]))
        #expect(out["language"] == .string("ga"))
        #expect(out["recordingShortcutMode"] == .string("push-to-talk"))
        #expect(out["transcriptionLocation"] == .string("remote-host"))
    }

    @Test func acceleratorStringsRoundTrip() {
        let a = AcceleratorString(parsing: "CommandOrControl+Shift+Digit1")
        #expect(a == AcceleratorString(modifiers: [.command, .shift], code: "Digit1"))
        #expect(a?.string == "CommandOrControl+Shift+Digit1")
        #expect(a?.symbols == "⇧⌘1")
        #expect(AcceleratorString(parsing: "Alt+Space")?.symbols == "⌥Space")
        #expect(AcceleratorString(parsing: "Ctrl+a")?.code == "KeyA")
        #expect(AcceleratorString(parsing: "Shift+A+B") == nil)
    }
}

@Suite("History + usage")
struct HistoryTests {
    @Test func historyDedupesAndCaps() {
        var items: [TranscriptHistoryItem] = []
        for i in 0..<60 {
            items = TranscriptHistory.adding(TranscriptHistoryItem(id: "\(i)", createdAt: Int64(i), text: "note \(i)", backend: "parakeet",
                                                                   location: "local", durationSeconds: 1), to: items)
        }
        #expect(items.count == 50)
        items = TranscriptHistory.adding(TranscriptHistoryItem(id: "x", createdAt: 99, text: "NOTE   59", backend: "parakeet",
                                                               location: "local", durationSeconds: 1), to: items)
        #expect(items.count == 50)
        #expect(items.first?.id == "x")
        #expect(!items.contains { $0.id == "59" })
    }

    @Test func decodesRustHistoryItems() throws {
        let json = #"[{"id":"transcript-1","createdAt":1759600000000,"text":"Hello","backend":"parakeet","location":"local","durationSeconds":1.5,"timings":{"transcribeMs":120,"speechModelMs":80,"polishMs":0,"totalMs":130}}]"#
        let items = try JSONDecoder().decode([TranscriptHistoryItem].self, from: Data(json.utf8))
        #expect(items.first?.timings?.totalMs == 130)
        #expect(items.first?.polished == false)
    }

    @Test func usageBucketsByUTCDay() {
        var u = UsageStats()
        let day = Date(timeIntervalSince1970: 20_610 * 86_400 + 3600)
        u.record(words: 10, recordingSeconds: 4, at: day)
        u.record(words: 5, recordingSeconds: 2, at: day)
        u.record(words: 7, recordingSeconds: 3, at: day.addingTimeInterval(-86_400))
        #expect(u.days["20610"]?.words == 15)
        #expect(u.words(lastDays: 7, from: day) == 22)
        #expect(u.streak(from: day) == 2)
        #expect(u.series(days: 7, from: day).count == 7)
        #expect(u.totalDictations == 3)
    }

    @Test func wordCounterIgnoresPunctuationOnlyTokens() {
        #expect(WordCounter.count("Hello, world — it's 3 pm.") == 5)
    }
}

@Suite("Audio gate")
struct AudioGateTests {
    @Test func rejectsShortAndSilentRecordings() {
        #expect(RecordingGate.evaluate(AudioStats(samples: [Float](repeating: 0.1, count: 4000), sampleRate: 16_000)) == .tooShort)
        #expect(RecordingGate.evaluate(AudioStats(samples: [Float](repeating: 0.0005, count: 16_000), sampleRate: 16_000)) == .silent)
        let speech = (0..<16_000).map { Float(sin(Double($0) / 8)) * 0.2 }
        #expect(RecordingGate.evaluate(AudioStats(samples: speech, sampleRate: 16_000)) == .accept)
    }

    @Test func meterLevelIsBounded() {
        #expect(PCM.meterLevel(rms: 0) == 0)
        #expect(PCM.meterLevel(rms: 1) == 1)
        #expect(PCM.meterLevel(rms: 0.01) > 0.2 && PCM.meterLevel(rms: 0.01) < 0.5)
    }
}

@Suite("Vocabulary casing")
struct VocabularyCasingTests {
    @Test func restoresUserCasingOnWholeWords() {
        let vocab = ["Amoxicillin", "pnpm", "Dr O'Keeffe", "kubectl"]
        #expect(VocabularyCasing.apply("start amoxicillin 500 mg, see dr o'keeffe.", vocabulary: vocab)
                == "start Amoxicillin 500 mg, see Dr O'Keeffe.")
        #expect(VocabularyCasing.apply("Run PNPM install", vocabulary: vocab) == "Run pnpm install")
        // Never touches substrings of other words.
        #expect(VocabularyCasing.apply("kubectlx and pnpmfoo", vocabulary: vocab) == "kubectlx and pnpmfoo")
        #expect(VocabularyCasing.apply("nothing here", vocabulary: []) == "nothing here")
    }

    @Test func basicPostProcessorTrimsAndCases() async {
        let out = await BasicPostProcessor().process("  the patient takes amoxicillin  ",
                                                     context: PostProcessContext(appBundleID: nil, vocabulary: ["Amoxicillin"]))
        #expect(out == "the patient takes Amoxicillin")
    }
}
