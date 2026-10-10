import FairspokenCore
import Foundation
import Testing
@testable import FairspokenHost

@Suite("Host config: model links")
struct ModelLinkConfigTests {
    let known: Set<String> = [HostConfiguration.defaultModel, "parakeet-ultra"]
    static let v3 = "https://artifactory.example.org/artifactory/models/parakeet-v3-mac.zip?token=abc"

    @Test func noneByDefaultAndLeftOutOfTheFile() throws {
        #expect(HostConfiguration().modelLinks.isEmpty)
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(HostConfiguration())) as? [String: Any]
        #expect(json?["modelLinks"] == nil)
        // A file without the key (the Rust host's, or an older one) has none.
        let rust = #"{"maxActiveStreams":4,"maxRecordingSeconds":600,"useGpu":true,"workerModels":["parakeet-tdt-0.6b-v3"]}"#
        #expect(try JSONDecoder().decode(HostConfiguration.self, from: Data(rust.utf8)).modelLinks.isEmpty)
    }

    @Test func roundTripsThroughTheFile() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("host-config-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: url) }
        var config = HostConfiguration()
        config.modelLinks = ["parakeet-tdt-0.6b-v3": Self.v3, "parakeet-ultra": "/Volumes/Models/ultra.zip"]
        try HostConfigurationStore.save(config, to: url)
        let loaded = try #require(try HostConfigurationStore.load(url))
        #expect(loaded.modelLinks == config.modelLinks)
        #expect(loaded == config)
        let raw = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any]
        #expect((raw?["modelLinks"] as? [String: String])?["parakeet-tdt-0.6b-v3"] == Self.v3)
    }

    @Test func anOldModelSourceIsIgnored() throws {
        let old = #"{"maxActiveStreams":4,"workerModels":["parakeet-tdt-0.6b-v3"],"workerCount":1,"modelSource":"https://hf-mirror.com"}"#
        let decoded = try JSONDecoder().decode(HostConfiguration.self, from: Data(old.utf8))
        #expect(decoded.modelLinks.isEmpty)
        #expect(try HostConfigurationStore.resolve(file: decoded, environment: [:], knownModels: known).modelLinks.isEmpty)
        let saved = try JSONSerialization.jsonObject(with: JSONEncoder().encode(decoded)) as? [String: Any]
        #expect(saved?["modelSource"] == nil)
    }

    @Test func normalisesLinks() {
        var config = HostConfiguration()
        config.modelLinks = ["parakeet-ultra": "  https://files.example.org/u.zip?dl=1#frag ", "parakeet-tdt-0.6b-v3": "  "]
        #expect(config.normalized(knownModels: known).modelLinks == ["parakeet-ultra": "https://files.example.org/u.zip?dl=1"])
    }

    @Test func anInvalidFileLinkStopsTheServerWithAReason() {
        var file = HostConfiguration()
        file.modelLinks = ["parakeet-ultra": "https://ann:pw@files.example.org/u.zip"]
        let error = #expect(throws: HostConfigurationStore.StoreError.self) {
            try HostConfigurationStore.resolve(file: file, environment: [:], knownModels: known)
        }
        #expect(error?.localizedDescription == "The host config's link for parakeet-ultra is invalid (fix or delete it): "
            + "The link to files.example.org contains a user name or password. Leave them out of the link.")
        file.modelLinks = ["parakeet-ultra": "models/u.zip"]
        #expect(throws: HostConfigurationStore.StoreError.self) { try HostConfigurationStore.resolve(file: file, environment: [:], knownModels: known) }
    }

    @Test func environmentLinksWinOverTheFile() throws {
        var file = HostConfiguration()
        file.modelLinks = ["parakeet-tdt-0.6b-v3": "/Volumes/Models/v3.zip", "parakeet-ultra": "/Volumes/Models/ultra.zip"]
        #expect(try HostConfigurationStore.resolve(file: file, environment: [:], knownModels: known).modelLinks == file.modelLinks)
        let env = ["FAIRSPOKEN_MODEL_LINKS": "  parakeet-tdt-0.6b-v3=\(Self.v3)#x \n "]
        let resolved = try HostConfigurationStore.resolve(file: file, environment: env, knownModels: known)
        #expect(resolved.modelLinks == ["parakeet-tdt-0.6b-v3": Self.v3, "parakeet-ultra": "/Volumes/Models/ultra.zip"])
        #expect(try HostConfigurationStore.resolve(file: nil, environment: env, knownModels: known).modelLinks == ["parakeet-tdt-0.6b-v3": Self.v3])
        // Two pairs; blank means unset.
        let two = "parakeet-tdt-0.6b-v3=https://f.example.org/v3.zip parakeet-ultra=file:///Volumes/Team%20Share/u.zip"
        #expect(try HostEnvironment.modelLinks(in: ["FAIRSPOKEN_MODEL_LINKS": two], knownModels: known)
            == ["parakeet-tdt-0.6b-v3": "https://f.example.org/v3.zip", "parakeet-ultra": "file:///Volumes/Team%20Share/u.zip"])
        #expect(try HostEnvironment.modelLinks(in: ["FAIRSPOKEN_MODEL_LINKS": "  "], knownModels: known) == nil)
        #expect(try HostEnvironment.modelLinks(in: [:], knownModels: known) == nil)
        // An environment link stands in for an invalid file link for the same model.
        var bad = HostConfiguration()
        bad.modelLinks = ["parakeet-tdt-0.6b-v3": "nope"]
        #expect(try HostConfigurationStore.resolve(file: bad, environment: env, knownModels: known).modelLinks == ["parakeet-tdt-0.6b-v3": Self.v3])
    }

    @Test func rejectsUnknownModelsAndBadPairs() {
        func message(_ value: String) -> String? {
            #expect(throws: HostConfigurationStore.StoreError.self) {
                try HostConfigurationStore.resolve(file: nil, environment: ["FAIRSPOKEN_MODEL_LINKS": value], knownModels: known)
            }?.localizedDescription
        }
        #expect(message("whisper-large=https://f.example.org/w.zip")
            == "FAIRSPOKEN_MODEL_LINKS: unknown model “whisper-large”. Use one of: parakeet-tdt-0.6b-v3, parakeet-ultra.")
        #expect(message("https://f.example.org/v3.zip")
            == "FAIRSPOKEN_MODEL_LINKS: “https://f.example.org/v3.zip” isn't model-id=link.")
        #expect(message("parakeet-ultra=ftp://f.example.org/u.zip")
            == "FAIRSPOKEN_MODEL_LINKS: parakeet-ultra: A model link can't be a ftp:// address. Use an http:// or https:// link, or the path to a file.")
        #expect(message("parakeet-ultra=") == "FAIRSPOKEN_MODEL_LINKS: parakeet-ultra: Enter a link to the model's .zip file, or the path to one.")
    }
}
