import Foundation
import Testing
@testable import FairspokenCore

@Suite("Vocabulary packs")
struct VocabularyPacksTests {
    static let gp = "ie-general-practice"
    static let se = "software-engineering"

    @Test func userTermsComeFirstAndTheCapHolds() {
        let user = (0..<45).map { "term\($0)" }
        let hints = VocabularyPacks.hints(userTerms: user, enabledPacks: [Self.gp])
        #expect(hints.count == VocabularyPacks.hintLimit)
        #expect(Array(hints.prefix(45)) == user)
        #expect(VocabularyPacks.hints(userTerms: user, enabledPacks: []) == user)
    }

    @Test func packsFollowBundledOrderAndSkipDuplicates() {
        let hints = VocabularyPacks.hints(userTerms: ["github"], enabledPacks: [Self.se, Self.gp, "unknown"])
        #expect(hints.first == "github")
        #expect(!hints.contains("GitHub"))
        #expect(hints[1] == "Healthlink")
        #expect(hints.count == VocabularyPacks.hintLimit)
    }

    @Test func enabledPacksLoadSaveAndNormalise() throws {
        let s = try JSONDecoder().decode(AppSettings.self, from: Data(#"{"enabledPacks":[" software-engineering ","software-engineering","",null]}"#.utf8))
        #expect(s.enabledPacks == [])  // a malformed array falls back to the default
        let t = try JSONDecoder().decode(AppSettings.self, from: Data(#"{"enabledPacks":[" software-engineering ","software-engineering","","future-pack"]}"#.utf8))
        #expect(t.enabledPacks == [Self.se, "future-pack"])
        let out = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(t)).objectValue ?? [:]
        #expect(out["enabledPacks"] == .array([.string(Self.se), .string("future-pack")]))
        #expect(try JSONDecoder().decode(AppSettings.self, from: Data("{}".utf8)).enabledPacks.isEmpty)
    }

    /// The always-on lists are copied from the pack files the Tauri app bundles; keep them equal.
    @Test func alwaysOnListsMatchTheBundledPackFiles() throws {
        let packs = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // FairspokenCoreTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // FairspokenCore
            .deletingLastPathComponent()  // Packages
            .deletingLastPathComponent()  // macos
            .deletingLastPathComponent()  // apps
            .deletingLastPathComponent()  // repository root
            .appending(path: "src-tauri/packs")
        for pack in VocabularyPacks.all {
            let data = try Data(contentsOf: packs.appending(path: "\(pack.id).json"))
            let file = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
            #expect(file["name"] as? String == pack.name)
            #expect(file["always_on"] as? [String] == pack.terms)
        }
    }
}
