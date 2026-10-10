import FairspokenCore
import Foundation
import Testing
@testable import FairspokenHost

@Suite("Host config: model source")
struct ModelSourceConfigTests {
    let known: Set<String> = [HostConfiguration.defaultModel, "parakeet-ultra"]

    @Test func defaultsToHuggingFaceAndIsLeftOutOfTheFile() throws {
        #expect(HostConfiguration().modelSource == "")
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(HostConfiguration())) as? [String: Any]
        #expect(json?["modelSource"] == nil)
        // A file without the key (the Rust host's, or an older one) reads as Hugging Face.
        let rust = #"{"maxActiveStreams":4,"maxRecordingSeconds":600,"useGpu":true,"workerModels":["parakeet-tdt-0.6b-v3"]}"#
        #expect(try JSONDecoder().decode(HostConfiguration.self, from: Data(rust.utf8)).modelSource == "")
    }

    @Test func roundTripsThroughTheFile() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("host-config-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: url) }
        var config = HostConfiguration()
        config.modelSource = "https://artifactory.example.org/api/huggingfaceml/hf"
        try HostConfigurationStore.save(config, to: url)
        let loaded = try #require(try HostConfigurationStore.load(url))
        #expect(loaded.modelSource == config.modelSource)
        #expect(loaded == config)
        let raw = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any]
        #expect(raw?["modelSource"] as? String == "https://artifactory.example.org/api/huggingfaceml/hf")
    }

    @Test func normalisesValidValues() {
        var config = HostConfiguration()
        config.modelSource = "  /Volumes/Models/ "
        #expect(config.normalized(knownModels: known).modelSource == "/Volumes/Models")
        config.modelSource = "https://hf-mirror.com///"
        #expect(config.normalized(knownModels: known).modelSource == "https://hf-mirror.com")
    }

    @Test func anInvalidFileValueStopsTheServerWithAReason() {
        var file = HostConfiguration()
        file.modelSource = "smb://nas/models"
        let error = #expect(throws: HostConfigurationStore.StoreError.self) {
            try HostConfigurationStore.resolve(file: file, environment: [:], knownModels: known)
        }
        #expect(error?.localizedDescription
            == "The host config's modelSource is invalid (fix or delete it): The model source can't be a smb:// address. Use an http:// or https:// mirror URL, or a folder path.")
        file.modelSource = "https://ann:pw@mirror.example.org"
        #expect(throws: HostConfigurationStore.StoreError.self) { try HostConfigurationStore.resolve(file: file, environment: [:], knownModels: known) }
        file.modelSource = "relative/models"
        #expect(throws: HostConfigurationStore.StoreError.self) { try HostConfigurationStore.resolve(file: file, environment: [:], knownModels: known) }
    }

    @Test func environmentWinsOverTheFileWhichWinsOverTheDefault() throws {
        var file = HostConfiguration()
        file.modelSource = "/Volumes/Models"
        #expect(try HostConfigurationStore.resolve(file: nil, environment: [:], knownModels: known).modelSource == "")
        #expect(try HostConfigurationStore.resolve(file: file, environment: [:], knownModels: known).modelSource == "/Volumes/Models")
        let env = ["FAIRSPOKEN_MODEL_SOURCE": " https://hf-mirror.com/ "]
        #expect(try HostConfigurationStore.resolve(file: file, environment: env, knownModels: known).modelSource == "https://hf-mirror.com")
        #expect(try HostConfigurationStore.resolve(file: nil, environment: env, knownModels: known).modelSource == "https://hf-mirror.com")
        // Blank means unset; an env value also stands in for an invalid file value.
        #expect(try HostConfigurationStore.resolve(file: file, environment: ["FAIRSPOKEN_MODEL_SOURCE": "  "], knownModels: known).modelSource
            == "/Volumes/Models")
        var badFile = HostConfiguration()
        badFile.modelSource = "nope"
        #expect(try HostConfigurationStore.resolve(file: badFile, environment: env, knownModels: known).modelSource == "https://hf-mirror.com")
        #expect(HostEnvironment.modelSource(in: ["FAIRSPOKEN_HOST_MODEL_SOURCE": "/x"]) == nil)
    }

    @Test func anInvalidEnvironmentValueNamesTheVariable() {
        let error = #expect(throws: HostConfigurationStore.StoreError.self) {
            try HostConfigurationStore.resolve(file: nil, environment: ["FAIRSPOKEN_MODEL_SOURCE": "models"], knownModels: known)
        }
        #expect(error?.localizedDescription
            == "FAIRSPOKEN_MODEL_SOURCE: The model source “models” is a relative path. Use a full folder path such as /Volumes/Models or ~/Models, or an http(s):// mirror URL.")
    }
}
