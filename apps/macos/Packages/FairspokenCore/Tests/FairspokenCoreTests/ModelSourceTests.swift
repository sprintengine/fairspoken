import Foundation
import Testing
@testable import FairspokenCore

@Suite("Model source")
struct ModelSourceTests {
    static let repo = "FluidInference/parakeet-tdt-0.6b-v3-coreml"
    static let required = ["Preprocessor.mlmodelc", "Encoder.mlmodelc", "Decoder.mlmodelc", "JointDecisionv3.mlmodelc", "parakeet_vocab.json"]

    // MARK: Classification

    @Test func emptyIsHuggingFace() throws {
        #expect(try ModelSource.parse("") == .huggingFace)
        #expect(try ModelSource.parse("  \n ") == .huggingFace)
        #expect(try ModelSource.normalize("   ") == "")
    }

    @Test func httpAndHttpsAreMirrors() throws {
        #expect(try ModelSource.parse("https://hf-mirror.com") == .mirror(URL(string: "https://hf-mirror.com")!))
        #expect(try ModelSource.parse(" HTTP://10.0.0.5:8081/artifactory/api/huggingfaceml/hf/// ")
            == .mirror(URL(string: "HTTP://10.0.0.5:8081/artifactory/api/huggingfaceml/hf")!))
        #expect(try ModelSource.normalize(" https://nexus.example.org/repository/hf/ ") == "https://nexus.example.org/repository/hf")
    }

    @Test func pathsAndFileURLsAreFolders() throws {
        #expect(try ModelSource.parse("/Volumes/Models/") == .folder(URL(fileURLWithPath: "/Volumes/Models", isDirectory: true)))
        #expect(try ModelSource.parse("~/Models", homeDirectory: "/Users/ann") == .folder(URL(fileURLWithPath: "/Users/ann/Models", isDirectory: true)))
        #expect(try ModelSource.parse("~", homeDirectory: "/Users/ann") == .folder(URL(fileURLWithPath: "/Users/ann", isDirectory: true)))
        #expect(try ModelSource.parse("file:///Volumes/Team%20Share/hf") == .folder(URL(fileURLWithPath: "/Volumes/Team Share/hf", isDirectory: true)))
        #expect(try ModelSource.parse("file://localhost/srv/models") == .folder(URL(fileURLWithPath: "/srv/models", isDirectory: true)))
        // A colon without :// is part of a folder name.
        #expect(try ModelSource.parse("/Volumes/Models:2026") == .folder(URL(fileURLWithPath: "/Volumes/Models:2026", isDirectory: true)))
        #expect(try ModelSource.normalize("/Volumes/Models//") == "/Volumes/Models")
        #expect(try ModelSource.normalize("~/") == "~")
        #expect(try ModelSource.normalize("/") == "/")
        #expect(try ModelSource.normalize("file:///srv/models/") == "file:///srv/models/") // as typed, like the Tauri app
    }

    @Test func rejectsOtherSchemesCredentialsAndRelativePaths() {
        #expect(throws: ModelSourceError.unsupportedScheme("ftp")) { try ModelSource.parse("ftp://mirror.example.org/hf") }
        #expect(throws: ModelSourceError.unsupportedScheme("smb")) { try ModelSource.parse("smb://nas/models") }
        #expect(throws: ModelSourceError.credentials("mirror.example.org")) { try ModelSource.parse("https://ann:secret@mirror.example.org") }
        #expect(throws: ModelSourceError.credentials("mirror.example.org")) { try ModelSource.parse("https://token@mirror.example.org") }
        #expect(throws: ModelSourceError.relativePath("models")) { try ModelSource.parse("models") }
        #expect(throws: ModelSourceError.relativePath("./models")) { try ModelSource.parse("./models") }
        #expect(throws: ModelSourceError.relativePath("~ann/models")) { try ModelSource.parse("~ann/models") }
        #expect(throws: ModelSourceError.invalidURL("https://")) { try ModelSource.parse("https://") }
        #expect(throws: ModelSourceError.queryOrFragment("https://m.example.org/hf?x=1")) { try ModelSource.parse("https://m.example.org/hf?x=1") }
        #expect(throws: ModelSourceError.remoteFileURL("file://nas/models")) { try ModelSource.parse("file://nas/models") }
        let long = "https://m.example.org/" + String(repeating: "a", count: 2048)
        #expect(throws: ModelSourceError.tooLong(long.count)) { try ModelSource.parse(long) }
        #expect(throws: Never.self) { try ModelSource.parse("/" + String(repeating: "a", count: 2047)) }
    }

    @Test func messagesNameTheValueButNeverTheCredentials() throws {
        let credentials = try #require(ModelSource.problem("https://ann:hunter22@mirror.example.org/hf"))
        #expect(credentials.contains("mirror.example.org"))
        #expect(!credentials.contains("hunter22"))
        #expect(ModelSource.problem("models")?.contains("“models”") == true)
        #expect(ModelSource.problem("ftp://x")?.contains("ftp://") == true)
        #expect(ModelSource.problem("https://hf-mirror.com") == nil)
    }

    // MARK: URLs and folders

    @Test func buildsFileAndTreeURLs() throws {
        let hf = ModelSource.huggingFace
        #expect(hf.fileURL(repo: Self.repo, file: "parakeet_vocab.json")?.absoluteString
            == "https://huggingface.co/FluidInference/parakeet-tdt-0.6b-v3-coreml/resolve/main/parakeet_vocab.json")
        let mirror = try ModelSource.parse("https://artifactory.example.org/artifactory/api/huggingfaceml/hf/")
        #expect(mirror.baseURL?.absoluteString == "https://artifactory.example.org/artifactory/api/huggingfaceml/hf")
        #expect(mirror.fileURL(repo: Self.repo, revision: "df26", file: "Decoder.mlmodelc/coremldata.bin")?.absoluteString
            == "https://artifactory.example.org/artifactory/api/huggingfaceml/hf/FluidInference/parakeet-tdt-0.6b-v3-coreml/resolve/df26/Decoder.mlmodelc/coremldata.bin")
        #expect(mirror.treeURL(repo: Self.repo)?.absoluteString
            == "https://artifactory.example.org/artifactory/api/huggingfaceml/hf/api/models/FluidInference/parakeet-tdt-0.6b-v3-coreml/tree/main")
        #expect(mirror.location(of: Self.repo) == "https://artifactory.example.org/artifactory/api/huggingfaceml/hf/FluidInference/parakeet-tdt-0.6b-v3-coreml")
        #expect(try ModelSource.parse("/srv").fileURL(repo: Self.repo, file: "x") == nil)
        #expect(hf.fileURL(repo: "../etc", file: "x") == nil)
    }

    @Test func resolvesFolderRepoPaths() throws {
        let folder = try ModelSource.parse("/Volumes/Models")
        #expect(folder.repoDirectory(Self.repo)?.path == "/Volumes/Models/FluidInference/parakeet-tdt-0.6b-v3-coreml")
        #expect(folder.repoDirectory("FluidInference/parakeet-ultra-coreml")?.path == "/Volumes/Models/FluidInference/parakeet-ultra-coreml")
        #expect(folder.location(of: Self.repo) == "/Volumes/Models/FluidInference/parakeet-tdt-0.6b-v3-coreml")
        #expect(folder.repoDirectory("FluidInference/../secrets") == nil)
        #expect(folder.repoDirectory("no-owner") == nil)
        #expect(ModelSource.huggingFace.repoDirectory(Self.repo) == nil)
    }

    // MARK: Folder install (dummy files, no real models)

    /// `{root}/FluidInference/parakeet-tdt-0.6b-v3-coreml` laid out as `hf download --local-dir` does.
    private func makeFolderSource(missing: Set<String> = []) throws -> (root: URL, repoDir: URL) {
        let fm = FileManager.default
        let root = fm.temporaryDirectory.appendingPathComponent("model-source-\(UUID().uuidString)", isDirectory: true)
        let repoDir = root.appendingPathComponent(Self.repo, isDirectory: true)
        try fm.createDirectory(at: repoDir, withIntermediateDirectories: true)
        for name in Self.required where !missing.contains(name) {
            if name.hasSuffix(".mlmodelc") {
                let bundle = repoDir.appendingPathComponent(name, isDirectory: true)
                try fm.createDirectory(at: bundle.appendingPathComponent("weights"), withIntermediateDirectories: true)
                try Data("coreml".utf8).write(to: bundle.appendingPathComponent("coremldata.bin"))
                try Data(repeating: 7, count: 5_000_000).write(to: bundle.appendingPathComponent("weights/weight.bin"))
            } else {
                try Data(#"{"0":"a"}"#.utf8).write(to: repoDir.appendingPathComponent(name))
            }
        }
        try Data("{}".utf8).write(to: repoDir.appendingPathComponent("config.json"))
        // Left behind: hidden entries and bundles nobody loads.
        try fm.createDirectory(at: repoDir.appendingPathComponent(".cache/huggingface"), withIntermediateDirectories: true)
        try fm.createDirectory(at: repoDir.appendingPathComponent("Encoder.mlpackage"), withIntermediateDirectories: true)
        try Data("source".utf8).write(to: repoDir.appendingPathComponent("Encoder.mlpackage/model.mlmodel"))
        return (root, repoDir)
    }

    @Test func installsFromAFolderAndSkipsWhatIsNotLoaded() throws {
        let (root, _) = try makeFolderSource()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try ModelSource.parse(root.path)
        let from = try #require(source.repoDirectory(Self.repo))
        let cache = root.appendingPathComponent("cache/parakeet-tdt-0.6b-v3", isDirectory: true)
        var fractions: [Double] = []
        try ModelFolderInstaller.install(from: from, to: cache, required: Self.required, progress: { fractions.append($0) }) {
            ModelFolderInstaller.missingFiles(in: $0, required: Self.required)
        }
        #expect(ModelFolderInstaller.missingFiles(in: cache, required: Self.required).isEmpty)
        #expect(FileManager.default.fileExists(atPath: cache.appendingPathComponent("Encoder.mlmodelc/weights/weight.bin").path))
        #expect(FileManager.default.fileExists(atPath: cache.appendingPathComponent("config.json").path))
        #expect(!FileManager.default.fileExists(atPath: cache.appendingPathComponent("Encoder.mlpackage").path))
        #expect(!FileManager.default.fileExists(atPath: cache.appendingPathComponent(".cache").path))
        #expect(try Data(contentsOf: cache.appendingPathComponent("Decoder.mlmodelc/weights/weight.bin")).count == 5_000_000)
        #expect(fractions.first == 0 && fractions.last == 1)
        #expect(fractions == fractions.sorted())
        #expect(fractions.count > 4)
        // No staging folder left next to the cache.
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: cache.deletingLastPathComponent().path)
        #expect(leftovers == ["parakeet-tdt-0.6b-v3"])
    }

    @Test func refusesAnIncompleteFolderBeforeCopying() throws {
        let (root, repoDir) = try makeFolderSource(missing: ["Encoder.mlmodelc", "parakeet_vocab.json"])
        defer { try? FileManager.default.removeItem(at: root) }
        let cache = root.appendingPathComponent("cache/parakeet-tdt-0.6b-v3", isDirectory: true)
        #expect(throws: ModelSourceError.filesMissing(path: repoDir.path, files: ["Encoder.mlmodelc", "parakeet_vocab.json"])) {
            try ModelFolderInstaller.install(from: repoDir, to: cache, required: Self.required) { _ in [] }
        }
        #expect(!FileManager.default.fileExists(atPath: cache.path))
        let message = ModelSourceError.filesMissing(path: repoDir.path, files: ["Encoder.mlmodelc"]).localizedDescription
        #expect(message == "\(repoDir.path) is missing Encoder.mlmodelc.")
    }

    @Test func removesACopyThatDoesNotVerify() throws {
        let (root, repoDir) = try makeFolderSource()
        defer { try? FileManager.default.removeItem(at: root) }
        let cache = root.appendingPathComponent("cache/parakeet-tdt-0.6b-v3", isDirectory: true)
        #expect(throws: ModelSourceError.filesMissing(path: repoDir.path, files: ["JointDecisionv3.mlmodelc"])) {
            try ModelFolderInstaller.install(from: repoDir, to: cache, required: Self.required) { _ in ["JointDecisionv3.mlmodelc"] }
        }
        #expect(!FileManager.default.fileExists(atPath: cache.path))
        #expect(try FileManager.default.contentsOfDirectory(atPath: cache.deletingLastPathComponent().path).isEmpty)
    }

    @Test func missingFolderNamesThePath() throws {
        let missing = FileManager.default.temporaryDirectory.appendingPathComponent("nowhere-\(UUID().uuidString)")
        #expect(throws: ModelSourceError.folderMissing(path: missing.path)) {
            try ModelFolderInstaller.install(from: missing, to: missing.appendingPathComponent("x"), required: []) { _ in [] }
        }
    }

    @Test func probesAFolder() async throws {
        let (root, repoDir) = try makeFolderSource()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try ModelSource.parse(root.path)
        let ok = await ModelSourceProbe.check(source, repo: Self.repo, requiredFiles: Self.required)
        #expect(ok == ModelSourceCheck(ok: true, message: "Found \(Self.repo) in \(repoDir.path)."))
        let other = await ModelSourceProbe.check(source, repo: "FluidInference/parakeet-ultra-coreml", requiredFiles: Self.required)
        #expect(!other.ok)
        #expect(other.message == "No model folder at \(root.path)/FluidInference/parakeet-ultra-coreml.")
    }

    @Test func probesAMirror() async throws {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubMirror.self]
        let session = URLSession(configuration: config)
        let good = try ModelSource.parse("https://mirror.test/hf")
        let ok = await ModelSourceProbe.check(good, repo: Self.repo, requiredFiles: Self.required, session: session)
        #expect(ok == ModelSourceCheck(ok: true, message: "mirror.test serves \(Self.repo)."))
        // Files but no tree API: FluidAudio couldn't list what to download.
        let noAPI = try ModelSource.parse("https://files-only.test/hf")
        let bad = await ModelSourceProbe.check(noAPI, repo: Self.repo, requiredFiles: Self.required, session: session)
        #expect(bad == ModelSourceCheck(ok: false, message:
            "The file list isn't served: HTTP 404 for https://files-only.test/hf/api/models/\(Self.repo)/tree/main"))
        let missing = await ModelSourceProbe.check(good, repo: "FluidInference/nope-coreml", requiredFiles: Self.required, session: session)
        #expect(missing == ModelSourceCheck(ok: false, message:
            "HTTP 404 for https://mirror.test/hf/FluidInference/nope-coreml/resolve/main/parakeet_vocab.json"))
    }

    // MARK: Settings

    @Test func settingsDefaultAndRoundTrip() throws {
        #expect(AppSettings().modelSource == "")
        let absent = try JSONDecoder().decode(AppSettings.self, from: Data("{}".utf8))
        #expect(absent.modelSource == "")
        let decoded = try JSONDecoder().decode(AppSettings.self, from: Data(#"{"modelSource":"  https://hf-mirror.com/ "}"#.utf8))
        #expect(decoded.modelSource == "https://hf-mirror.com")
        let json = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(decoded)).objectValue ?? [:]
        #expect(json["modelSource"] == .string("https://hf-mirror.com"))
        var folder = AppSettings()
        folder.modelSource = "~/Models/"
        #expect(folder.normalized().modelSource == "~/Models")
    }

    @Test func settingsKeepBadValuesTrimmedSoTheErrorShows() throws {
        // The Tauri app's `normalize_setting`: an invalid value is kept so its error is visible.
        for (json, kept) in [(#"{"modelSource":"  models  "}"#, "models"), (#"{"modelSource":"ftp://x/y/"}"#, "ftp://x/y/"),
                             (#"{"modelSource":"https://u:p@x"}"#, "https://u:p@x")] {
            let s = try JSONDecoder().decode(AppSettings.self, from: Data(json.utf8))
            #expect(s.modelSource == kept, "\(json)")
        }
        #expect(try JSONDecoder().decode(AppSettings.self, from: Data(#"{"modelSource":42}"#.utf8)).modelSource == "")
    }
}

/// `mirror.test` serves files and the tree API; `files-only.test` serves files only.
final class StubMirror: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let url = request.url!
        let path = url.path
        var status = 404
        var body = Data()
        if path.contains("/parakeet-tdt-0.6b-v3-coreml/resolve/main/") {
            status = 200
        } else if url.host == "mirror.test", path.hasSuffix("/api/models/FluidInference/parakeet-tdt-0.6b-v3-coreml/tree/main") {
            status = 200
            body = Data(#"[{"type":"file","path":"parakeet_vocab.json","size":10}]"#.utf8)
        }
        let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
