import Foundation
import Synchronization
import Testing
@testable import FairspokenCore

// Serialized: these run system tools and copy files, which would slow the timing-sensitive
// suites running alongside.
@Suite("Model link", .serialized)
struct ModelLinkTests {
    // MARK: Parsing

    @Test func webLinksKeepTheirQueryAndLoseTheirFragment() throws {
        let link = try ModelLink.parse("  https://artifactory.example.org/artifactory/models/parakeet-v3.zip?X-Token=abc123&dl=1#section  ")
        #expect(link.url.absoluteString == "https://artifactory.example.org/artifactory/models/parakeet-v3.zip?X-Token=abc123&dl=1")
        #expect(!link.isFile)
        #expect(try ModelLink.normalize(" https://files.example.org/a.zip?dl=1#top ") == "https://files.example.org/a.zip?dl=1")
        #expect(try ModelLink.normalize("HTTP://10.0.0.5:8081/models/v3.zip") == "HTTP://10.0.0.5:8081/models/v3.zip")
    }

    @Test func displayNeverShowsTheQuery() throws {
        let link = try ModelLink.parse("https://share.example.org/sites/IT/Models/parakeet%20v3.zip?download=1&token=s3cr3t")
        #expect(link.display == "https://share.example.org/sites/IT/Models/parakeet v3.zip")
        #expect(link.shortDisplay == "share.example.org/sites/IT/Models/parakeet v3.zip")
        let port = try ModelLink.parse("http://models.local:8081/v3.zip?k=1")
        #expect(port.display == "http://models.local:8081/v3.zip")
        #expect(port.shortDisplay == "models.local:8081/v3.zip")
        #expect(ModelLink.display("https://files.example.org/a.zip?sig=xyz") == "https://files.example.org/a.zip")
        // A value that doesn't parse is shown up to its query.
        #expect(ModelLink.shortDisplay("nonsense?token=xyz") == "nonsense")
    }

    @Test func filePathsAndFileURLs() throws {
        #expect(try ModelLink.parse("/Volumes/Models/parakeet-v3.zip").location == .file(URL(fileURLWithPath: "/Volumes/Models/parakeet-v3.zip")))
        #expect(try ModelLink.parse("~/Downloads/v3.zip", homeDirectory: "/Users/ann").location == .file(URL(fileURLWithPath: "/Users/ann/Downloads/v3.zip")))
        #expect(try ModelLink.parse("file:///Volumes/Team%20Share/v3.zip").location == .file(URL(fileURLWithPath: "/Volumes/Team Share/v3.zip")))
        #expect(try ModelLink.parse("file://localhost/srv/v3.tgz").location == .file(URL(fileURLWithPath: "/srv/v3.tgz")))
        #expect(try ModelLink.parse("/Volumes/Models:2026/v3.zip").isFile) // a colon without :// is part of a name
        #expect(try ModelLink.parse("/Volumes/Team Share/v3.zip").display == "/Volumes/Team Share/v3.zip")
        #expect(try ModelLink.normalize("  /Volumes/Models/v3.zip ") == "/Volumes/Models/v3.zip")
    }

    @Test func rejectsWhatIsNotALink() {
        #expect(throws: ModelLinkError.empty) { try ModelLink.parse("   ") }
        #expect(throws: ModelLinkError.unsupportedScheme("ftp")) { try ModelLink.parse("ftp://files.example.org/v3.zip") }
        #expect(throws: ModelLinkError.unsupportedScheme("smb")) { try ModelLink.parse("smb://nas/models/v3.zip") }
        #expect(throws: ModelLinkError.relativePath("models/v3.zip")) { try ModelLink.parse("models/v3.zip") }
        #expect(throws: ModelLinkError.relativePath("~ann/v3.zip")) { try ModelLink.parse("~ann/v3.zip") }
        #expect(throws: ModelLinkError.remoteFileURL("file://nas/models/v3.zip")) { try ModelLink.parse("file://nas/models/v3.zip") }
        #expect(throws: ModelLinkError.invalidURL("https://")) { try ModelLink.parse("https://") }
        let long = "https://files.example.org/" + String(repeating: "a", count: 2048)
        #expect(throws: ModelLinkError.tooLong(long.count)) { try ModelLink.parse(long) }
        #expect(throws: Never.self) { try ModelLink.parse("/" + String(repeating: "a", count: 2047)) }
    }

    @Test func refusesCredentialsWithoutRepeatingThem() throws {
        #expect(throws: ModelLinkError.credentials("files.example.org")) { try ModelLink.parse("https://ann:hunter22@files.example.org/v3.zip") }
        #expect(throws: ModelLinkError.credentials("files.example.org")) { try ModelLink.parse("https://token@files.example.org/v3.zip") }
        let message = try #require(ModelLink.problem("https://ann:hunter22@files.example.org/v3.zip"))
        #expect(message == "The link to files.example.org contains a user name or password. Leave them out of the link.")
        #expect(ModelLink.problem("https://files.example.org/v3.zip?token=x") == nil)
    }

    @Test func normalisesAStoredMap() {
        let map = ModelLink.normalizeMap([
            "parakeet-tdt-0.6b-v3": "  https://files.example.org/v3.zip?dl=1#x ",
            "parakeet-ultra": "   ",
            "parakeet-tdt-0.6b-v2": " not a link ",
        ])
        #expect(map == ["parakeet-tdt-0.6b-v3": "https://files.example.org/v3.zip?dl=1", "parakeet-tdt-0.6b-v2": "not a link"])
    }

    // MARK: Formats

    @Test func detectsFormatsByTheirFirstBytes() throws {
        #expect(ArchiveFormat.detect(Data([0x50, 0x4B, 0x03, 0x04, 0, 0])) == .zip)
        #expect(ArchiveFormat.detect(Data([0x50, 0x4B, 0x05, 0x06] + [UInt8](repeating: 0, count: 18))) == .zip) // empty zip
        #expect(ArchiveFormat.detect(Data([0x1F, 0x8B, 0x08, 0x00])) == .gzip)
        var tar = Data(repeating: 0, count: 512)
        tar.replaceSubrange(257..<263, with: Array("ustar\0".utf8))
        #expect(ArchiveFormat.detect(tar) == .tar)
        #expect(ArchiveFormat.detect(Data("PK".utf8)) == nil)
        #expect(ArchiveFormat.detect(Data("hello, world".utf8)) == nil)
        #expect(ArchiveFormat.detect(Data()) == nil)
        #expect(ArchiveFormat.looksLikeWebPage(Data("\n <!DOCTYPE html><html><body>Sign in</body></html>".utf8)))
        #expect(!ArchiveFormat.looksLikeWebPage(Data([0x50, 0x4B, 0x03, 0x04])))

        // Real archives from the system tools, named without an extension.
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let model = try Fixture.model(in: dir.appendingPathComponent("src"))
        let zip = dir.appendingPathComponent("a")
        let tarFile = dir.appendingPathComponent("b")
        let tgz = dir.appendingPathComponent("c")
        try Fixture.run("/usr/bin/ditto", "-c", "-k", model.path, zip.path)
        try Fixture.run("/usr/bin/tar", "-cf", tarFile.path, "-C", model.path, ".")
        try Fixture.run("/usr/bin/tar", "-czf", tgz.path, "-C", model.path, ".")
        #expect(ArchiveFormat.detect(fileAt: zip) == .zip)
        #expect(ArchiveFormat.detect(fileAt: tarFile) == .tar)
        #expect(ArchiveFormat.detect(fileAt: tgz) == .gzip)
        #expect(ArchiveFormat.detect(fileAt: model.appendingPathComponent("parakeet_vocab.json")) == nil)
    }

    // MARK: Finding the model

    @Test func findsTheModelFolderAtAnyDepth() throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        // At the root.
        let root = try Fixture.model(in: dir.appendingPathComponent("root"))
        #expect(ModelArchive.findModelDirectory(in: root, required: Fixture.required) == root)
        // Two folders down, next to a decoy that lacks files, a hidden copy and __MACOSX.
        let nested = dir.appendingPathComponent("nested")
        let deep = try Fixture.model(in: nested.appendingPathComponent("release/parakeet-tdt-0.6b-v3"))
        _ = try Fixture.model(in: nested.appendingPathComponent("aaa-partial"), missing: ["Encoder.mlmodelc"])
        _ = try Fixture.model(in: nested.appendingPathComponent(".hidden"))
        _ = try Fixture.model(in: nested.appendingPathComponent("__MACOSX"))
        #expect(ModelArchive.findModelDirectory(in: nested, required: Fixture.required)?.standardizedFileURL == deep.standardizedFileURL)
        // The shallowest wins.
        let both = dir.appendingPathComponent("both")
        let shallow = try Fixture.model(in: both.appendingPathComponent("b"))
        _ = try Fixture.model(in: both.appendingPathComponent("a/inner"))
        #expect(ModelArchive.findModelDirectory(in: both, required: Fixture.required)?.standardizedFileURL == shallow.standardizedFileURL)
        // None.
        let none = try Fixture.model(in: dir.appendingPathComponent("none"), missing: ["parakeet_vocab.json"])
        #expect(ModelArchive.findModelDirectory(in: none, required: Fixture.required) == nil)
    }

    @Test func spotsEntriesThatLeaveTheArchive() {
        #expect(ModelArchive.unsafeEntry(in: ["a/", "a/b.bin", "c.json"]) == nil)
        #expect(ModelArchive.unsafeEntry(in: ["a/", "../evil.txt"]) == "../evil.txt")
        #expect(ModelArchive.unsafeEntry(in: ["a/../../evil"]) == "a/../../evil")
        #expect(ModelArchive.unsafeEntry(in: ["/etc/evil"]) == "/etc/evil")
        #expect(ModelArchive.unsafeEntry(in: ["..data/ok", "a..b"]) == nil)
    }

    // MARK: Installing (dummy files, an injected destination; the real model cache is never touched)

    @Test(arguments: [Fixture.Layout.zipAtRoot, .zipInAFolder, .tarGzInAFolder, .tarAtRoot])
    func installsFromAnArchive(_ layout: Fixture.Layout) async throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let archive = try Fixture.archive(layout, in: dir)
        let work = dir.appendingPathComponent("work", isDirectory: true)
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
        let destination = dir.appendingPathComponent("cache/parakeet-tdt-0.6b-v3", isDirectory: true)
        let stages = StageLog()
        let link = try ModelLink.parse(layout == .tarGzInAFolder ? "file://" + archive.path : archive.path)
        try await ModelLinkInstaller.install(link: link, modelName: "Parakeet TDT 0.6B v3", required: Fixture.required,
                                             destination: destination, workDirectory: work, progress: { stages.add($0) }) {
            ModelDirectoryInstaller.missingFiles(in: $0, required: Fixture.required)
        }
        #expect(ModelDirectoryInstaller.missingFiles(in: destination, required: Fixture.required).isEmpty)
        #expect(try Data(contentsOf: destination.appendingPathComponent("Encoder.mlmodelc/weights/weight.bin")).count == 300_000)
        #expect(FileManager.default.fileExists(atPath: destination.appendingPathComponent("config.json").path))
        // Not loaded, so not copied.
        #expect(!FileManager.default.fileExists(atPath: destination.appendingPathComponent("Encoder.mlpackage").path))
        let seen = stages.all
        #expect(seen.first == .downloading(0))
        #expect(seen.contains(.unpacking))
        #expect(seen.last == .installing(1))
        // The work folder and the staging copy are gone.
        #expect(try FileManager.default.contentsOfDirectory(atPath: work.path).isEmpty)
        #expect(try FileManager.default.contentsOfDirectory(atPath: destination.deletingLastPathComponent().path) == ["parakeet-tdt-0.6b-v3"])
    }

    @Test func downloadsOverHTTPAndReportsHTTPErrorsWithoutTheQuery() async throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let archive = try Fixture.archive(.zipInAFolder, in: dir)
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubFileServer.self]
        let destination = dir.appendingPathComponent("cache/parakeet-ultra", isDirectory: true)
        let served = "https://files.test/serve/parakeet-ultra.zip?path=\(archive.path)&token=s3cr3t"
        let stages = StageLog()
        try await ModelLinkInstaller.install(link: try ModelLink.parse(served), modelName: "Parakeet Ultra", required: Fixture.required,
                                             destination: destination, workDirectory: dir, configuration: config,
                                             progress: { stages.add($0) }) { _ in [] }
        #expect(ModelDirectoryInstaller.missingFiles(in: destination, required: Fixture.required).isEmpty)
        #expect(stages.all.contains { if case .downloading(let f) = $0 { f > 0 && f < 1 } else { false } })

        let forbidden = try ModelLink.parse("https://files.test/forbidden/v3.zip?token=s3cr3t#frag")
        let error = await #expect(throws: ModelLinkError.self) {
            try await ModelLinkInstaller.install(link: forbidden, modelName: "Parakeet TDT 0.6B v3", required: Fixture.required,
                                                 destination: dir.appendingPathComponent("never"), workDirectory: dir, configuration: config) { _ in [] }
        }
        #expect(error?.localizedDescription == "Couldn't download https://files.test/forbidden/v3.zip: HTTP 403")
        #expect(!FileManager.default.fileExists(atPath: dir.appendingPathComponent("never").path))
    }

    @Test func saysWhatIsMissing() async throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let model = try Fixture.model(in: dir.appendingPathComponent("src"), missing: ["Encoder.mlmodelc"])
        let zip = dir.appendingPathComponent("v3.zip")
        try Fixture.run("/usr/bin/ditto", "-c", "-k", model.path, zip.path)
        let error = await #expect(throws: ModelLinkError.self) {
            try await ModelLinkInstaller.install(link: try ModelLink.parse(zip.path), modelName: "Parakeet TDT 0.6B v3", required: Fixture.required,
                                                 destination: dir.appendingPathComponent("cache/v3"), workDirectory: dir) { _ in [] }
        }
        #expect(error?.localizedDescription == "The download from \(zip.path) doesn't contain the Parakeet TDT 0.6B v3 Core ML files. "
            + "It needs Decoder.mlmodelc, Encoder.mlmodelc, JointDecisionv3.mlmodelc, Preprocessor.mlmodelc, parakeet_vocab.json.")
        #expect(!FileManager.default.fileExists(atPath: dir.appendingPathComponent("cache").path))
    }

    @Test func removesACopyThatDoesNotVerify() async throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let archive = try Fixture.archive(.zipAtRoot, in: dir)
        let destination = dir.appendingPathComponent("cache/v3", isDirectory: true)
        await #expect(throws: ModelLinkError.missingModelFiles(link: archive.path, model: "Parakeet TDT 0.6B v3", needed: ["JointDecisionv3.mlmodelc"])) {
            try await ModelLinkInstaller.install(link: try ModelLink.parse(archive.path), modelName: "Parakeet TDT 0.6B v3", required: Fixture.required,
                                                 destination: destination, workDirectory: dir) { _ in ["JointDecisionv3.mlmodelc"] }
        }
        #expect(try FileManager.default.contentsOfDirectory(atPath: destination.deletingLastPathComponent().path).isEmpty)
    }

    @Test func refusesWhatIsNotAnArchive() async throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let text = dir.appendingPathComponent("notes.zip")
        try Data("just some text".utf8).write(to: text)
        let page = dir.appendingPathComponent("login")
        try Data("<!DOCTYPE html><html><title>Sign in</title></html>".utf8).write(to: page)
        func failure(_ path: String) async -> String? {
            await #expect(throws: ModelLinkError.self) {
                try await ModelLinkInstaller.install(link: try ModelLink.parse(path), modelName: "Parakeet Ultra", required: Fixture.required,
                                                     destination: dir.appendingPathComponent("cache/u"), workDirectory: dir) { _ in [] }
            }?.localizedDescription
        }
        #expect(await failure(text.path) == "\(text.path) isn't a zip or tar archive.")
        #expect(await failure(page.path)?.hasPrefix("\(page.path) isn't a zip or tar archive: it returned a web page") == true)
        #expect(await failure(dir.path) == "\(dir.path) is a folder. Use a .zip of the model instead.")
        #expect(await failure(dir.appendingPathComponent("absent.zip").path)
            == "Couldn't download \(dir.appendingPathComponent("absent.zip").path): There's no file there.")
    }

    @Test(arguments: ["tar", "zip"])
    func refusesEntriesThatEscape(_ format: String) async throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let source = dir.appendingPathComponent("src", isDirectory: true)
        try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
        try Data("bad".utf8).write(to: source.appendingPathComponent("evil.txt"))
        let archive = dir.appendingPathComponent("evil")
        // bsdtar writes `../evil.txt` into the archive with -s.
        try Fixture.run("/usr/bin/tar", "--format", format == "zip" ? "zip" : "ustar", "-cf", archive.path, "-s", ",^,../,", "-C", source.path, "evil.txt")
        let work = dir.appendingPathComponent("work/inner", isDirectory: true)
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
        await #expect(throws: ModelLinkError.unsafeEntry(link: archive.path, entry: "../evil.txt")) {
            try await ModelLinkInstaller.install(link: try ModelLink.parse(archive.path), modelName: "Parakeet Ultra", required: Fixture.required,
                                                 destination: dir.appendingPathComponent("cache/u"), workDirectory: work) { _ in [] }
        }
        #expect(try FileManager.default.contentsOfDirectory(atPath: work.path).isEmpty)
        #expect(try FileManager.default.contentsOfDirectory(atPath: dir.appendingPathComponent("work").path) == ["inner"])
    }

    @Test func refusesALinkPointingOutside() async throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let model = try Fixture.model(in: dir.appendingPathComponent("src"))
        try FileManager.default.createSymbolicLink(atPath: model.appendingPathComponent("secrets").path, withDestinationPath: "/etc")
        let archive = dir.appendingPathComponent("linked.tar")
        try Fixture.run("/usr/bin/tar", "-cf", archive.path, "-C", model.path, ".")
        await #expect(throws: ModelLinkError.unsafeEntry(link: archive.path, entry: "secrets")) {
            try await ModelLinkInstaller.install(link: try ModelLink.parse(archive.path), modelName: "Parakeet Ultra", required: Fixture.required,
                                                 destination: dir.appendingPathComponent("cache/u"), workDirectory: dir) { _ in [] }
        }
        #expect(!FileManager.default.fileExists(atPath: dir.appendingPathComponent("cache").path))
    }

    @Test func refusesArchivesThatUnpackTooLarge() async throws {
        let dir = try Fixture.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let archive = try Fixture.archive(.zipAtRoot, in: dir)
        let link = try ModelLink.parse(archive.path)
        let work = dir.appendingPathComponent("work", isDirectory: true)
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
        let bytes = await #expect(throws: ModelLinkError.self) {
            try await ModelLinkInstaller.install(link: link, modelName: "Parakeet Ultra", required: Fixture.required,
                                                 destination: dir.appendingPathComponent("cache/u"), workDirectory: work,
                                                 limits: .init(maxBytes: 100_000, maxEntries: 10_000)) { _ in [] }
        }
        #expect(bytes == .tooLarge(link: archive.path, limit: 100_000))
        let entries = await #expect(throws: ModelLinkError.self) {
            try await ModelLinkInstaller.install(link: link, modelName: "Parakeet Ultra", required: Fixture.required,
                                                 destination: dir.appendingPathComponent("cache/u"), workDirectory: work,
                                                 limits: .init(maxBytes: 8 << 30, maxEntries: 5)) { _ in [] }
        }
        #expect(entries == .tooManyEntries(link: archive.path, limit: 5))
        #expect(entries?.localizedDescription == "\(archive.path) holds more than 5 files, so it wasn't installed.")
        #expect(ModelLinkError.tooLarge(link: "https://x/y.zip", limit: 8 << 30).localizedDescription
            == "https://x/y.zip unpacks to more than 8 GiB, so it wasn't installed.")
        #expect(ModelLinkError.tooManyEntries(link: "https://x/y.zip", limit: 10_000).localizedDescription
            == "https://x/y.zip holds more than 10,000 files, so it wasn't installed.")
        #expect(try FileManager.default.contentsOfDirectory(atPath: work.path).isEmpty)
        #expect(!FileManager.default.fileExists(atPath: dir.appendingPathComponent("cache").path))
        // The size check runs on what was unpacked, not on what the archive claims.
        let measured = try ModelArchive.measure(try Fixture.model(in: dir.appendingPathComponent("m")), limits: .standard)
        #expect(measured.bytes >= 4 * 300_000)
    }

    @Test func explainsPlainHTTPRefusals() {
        #expect(ModelLinkError.describe(URLError(.appTransportSecurityRequiresSecureConnection))
            == "macOS allows plain http:// only on the local network. Use the server's https:// address.")
    }

    // MARK: Settings

    @Test func settingsRoundTripTheLinks() throws {
        #expect(AppSettings().modelLinks.isEmpty)
        let json = #"{"modelLinks":{"parakeet-tdt-0.6b-v3":"  https://files.example.org/v3.zip?dl=1#x ","parakeet-ultra":""}}"#
        let decoded = try JSONDecoder().decode(AppSettings.self, from: Data(json.utf8))
        #expect(decoded.modelLinks == ["parakeet-tdt-0.6b-v3": "https://files.example.org/v3.zip?dl=1"])
        let encoded = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(decoded)).objectValue ?? [:]
        #expect(encoded["modelLinks"] == .object(["parakeet-tdt-0.6b-v3": .string("https://files.example.org/v3.zip?dl=1")]))
        #expect(try JSONDecoder().decode(AppSettings.self, from: JSONEncoder().encode(decoded)) == decoded)
        // Wrong types fall back to none.
        #expect(try JSONDecoder().decode(AppSettings.self, from: Data(#"{"modelLinks":"https://x/y.zip"}"#.utf8)).modelLinks.isEmpty)
    }

    @Test func anOldModelSourceIsIgnored() throws {
        let json = #"{"model":"parakeet-ultra","modelSource":"https://hf-mirror.com"}"#
        let decoded = try JSONDecoder().decode(AppSettings.self, from: Data(json.utf8))
        #expect(decoded.model == "parakeet-ultra")
        #expect(decoded.modelLinks.isEmpty)
        let encoded = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(decoded)).objectValue ?? [:]
        #expect(encoded["modelSource"] == nil)
    }

    @Test func theTauriImportDropsItsModelLinks() throws {
        let tauri = #"{"model":"parakeet-ultra","language":"de","modelLinks":{"parakeet-tdt-0.6b-v3":"https://files.example.org/onnx.zip"},"modelSource":"/Volumes/M","theme":"dark"}"#
        let imported = try #require(AppSettings.importingTauri(Data(tauri.utf8)))
        #expect(imported.settings.model == "parakeet-ultra")
        #expect(imported.settings.language == "de")
        #expect(imported.settings.modelLinks.isEmpty)
        #expect(imported.raw["modelLinks"] == nil)
        #expect(imported.raw["modelSource"] == nil)
        #expect(imported.raw["theme"] == .string("dark")) // other unknown keys are kept
        let saved = try JSONDecoder().decode(JSONValue.self, from: JSONMerge.encode(imported.settings, over: imported.raw)).objectValue ?? [:]
        #expect(saved["modelLinks"] == .object([:]))
        #expect(AppSettings.importingTauri(Data("not json".utf8)) == nil)
    }
}

// MARK: - Fixtures

/// Stages as reported, in order.
final class StageLog: Sendable {
    private let stages = Mutex<[ModelLinkInstaller.Stage]>([])
    func add(_ stage: ModelLinkInstaller.Stage) { stages.withLock { $0.append(stage) } }
    var all: [ModelLinkInstaller.Stage] { stages.withLock { $0 } }
}

enum Fixture {
    static let required = ["Decoder.mlmodelc", "Encoder.mlmodelc", "JointDecisionv3.mlmodelc", "Preprocessor.mlmodelc", "parakeet_vocab.json"]

    enum Layout: String, CaseIterable, Sendable {
        case zipAtRoot, zipInAFolder, tarGzInAFolder, tarAtRoot
    }

    /// Incompressible, so an archive of it is big enough to report download progress.
    static let weights = Data((0..<300_000).map { _ in UInt8.random(in: .min ... .max) })

    static func temporaryDirectory() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("model-link-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// A dummy model folder: each bundle a folder with a small weight file, the vocabulary, a
    /// config and things that aren't loaded (a hidden folder, an `.mlpackage`).
    @discardableResult
    static func model(in dir: URL, missing: Set<String> = []) throws -> URL {
        let fm = FileManager.default
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        for name in required where !missing.contains(name) {
            if name.hasSuffix(".mlmodelc") {
                let bundle = dir.appendingPathComponent(name, isDirectory: true)
                try fm.createDirectory(at: bundle.appendingPathComponent("weights"), withIntermediateDirectories: true)
                try Data("coreml".utf8).write(to: bundle.appendingPathComponent("coremldata.bin"))
                try weights.write(to: bundle.appendingPathComponent("weights/weight.bin"))
            } else {
                try Data(#"{"0":"a"}"#.utf8).write(to: dir.appendingPathComponent(name))
            }
        }
        try Data("{}".utf8).write(to: dir.appendingPathComponent("config.json"))
        try fm.createDirectory(at: dir.appendingPathComponent(".cache"), withIntermediateDirectories: true)
        try fm.createDirectory(at: dir.appendingPathComponent("Encoder.mlpackage"), withIntermediateDirectories: true)
        try Data("source".utf8).write(to: dir.appendingPathComponent("Encoder.mlpackage/model.mlmodel"))
        return dir
    }

    /// An archive of a dummy model, built with the system tools.
    static func archive(_ layout: Layout, in dir: URL) throws -> URL {
        let parent = dir.appendingPathComponent("archive-src-\(layout.rawValue)", isDirectory: true)
        let model = try self.model(in: parent.appendingPathComponent("parakeet-tdt-0.6b-v3"))
        switch layout {
        case .zipAtRoot:
            let out = dir.appendingPathComponent("model.zip")
            try run("/usr/bin/ditto", "-c", "-k", model.path, out.path)
            return out
        case .zipInAFolder:
            let out = dir.appendingPathComponent("model-folder.zip")
            try run("/usr/bin/ditto", "-c", "-k", "--keepParent", model.path, out.path)
            return out
        case .tarGzInAFolder:
            let out = dir.appendingPathComponent("model.tar.gz")
            try run("/usr/bin/tar", "-czf", out.path, "-C", dir.path, "archive-src-\(layout.rawValue)")
            return out
        case .tarAtRoot:
            let out = dir.appendingPathComponent("model.tar")
            try run("/usr/bin/tar", "-cf", out.path, "-C", model.path, ".")
            return out
        }
    }

    static func run(_ executable: String, _ arguments: String...) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw CocoaError(.executableLoad, userInfo: [NSLocalizedDescriptionKey: "\(executable) \(arguments) failed"])
        }
    }
}

/// `files.test/serve/…?path=<file>` answers with that file (with its length, for progress);
/// anything else is 403.
final class StubFileServer: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "files.test" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let url = request.url!
        let path = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "path" }?.value
        if url.path.hasPrefix("/serve/"), let path, let data = FileManager.default.contents(atPath: path) {
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                                           headerFields: ["Content-Length": "\(data.count)", "Content-Type": "application/zip"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            // In pieces, so progress is reported along the way.
            var offset = 0
            while offset < data.count {
                let end = min(data.count, offset + 64 * 1024)
                client?.urlProtocol(self, didLoad: data.subdata(in: offset..<end))
                offset = end
            }
        } else {
            let response = HTTPURLResponse(url: url, statusCode: 403, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "text/html"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data("<html>Forbidden</html>".utf8))
        }
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
