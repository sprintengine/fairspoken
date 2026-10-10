import Foundation

/// Where speech models come from: `modelSource` in the client's settings.json and in
/// Fairspoken Server's host-config.json (the same rules as the Tauri app).
///
/// - `""`: Hugging Face.
/// - `http://` or `https://`: the base URL of a Hugging Face–compatible mirror (an Artifactory or
///   Nexus Hugging Face remote, hf-mirror, Olah). Files come from `{base}/{repo}/resolve/{revision}/{file}`
///   and file lists from `{base}/api/models/{repo}/tree/{revision}`.
/// - An absolute path, `~/…` or a `file://` URL: a local or network folder holding each model as
///   `{folder}/{owner}/{repo-name}/…`, the layout `hf download owner/repo-name --local-dir {folder}/owner/repo-name`
///   writes. The revision is ignored.
///
/// Repository paths (`FluidInference/parakeet-tdt-0.6b-v3-coreml`) are passed in, so this has
/// no FluidAudio dependency.
public enum ModelSource: Equatable, Sendable {
    case huggingFace
    /// A mirror's base URL, without a trailing slash.
    case mirror(URL)
    /// A folder (a file URL, absolute and standardised).
    case folder(URL)

    public static let huggingFaceURL = URL(string: "https://huggingface.co")!
    /// The longest value accepted, in characters.
    public static let maxLength = 2048

    // MARK: Parsing

    /// Classifies a settings value. Leading and trailing whitespace is ignored.
    public static func parse(_ raw: String, homeDirectory: String = NSHomeDirectory()) throws(ModelSourceError) -> ModelSource {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { return .huggingFace }
        guard text.count <= maxLength else { throw .tooLong(text.count) }
        let lower = text.lowercased()
        if lower.hasPrefix("http://") || lower.hasPrefix("https://") {
            return .mirror(try mirrorURL(trimmingSlashes(text)))
        }
        if lower.hasPrefix("file://") {
            guard let url = URL(string: text), url.isFileURL else { throw .invalidURL(text) }
            if let host = url.host(percentEncoded: false), !host.isEmpty, host.lowercased() != "localhost" {
                throw .remoteFileURL(text)
            }
            let path = url.plainPath
            guard path.hasPrefix("/") else { throw .relativePath(text) }
            return .folder(URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL)
        }
        if let scheme = scheme(of: text) { throw .unsupportedScheme(scheme) }
        if text == "~" || text.hasPrefix("~/") {
            let path = homeDirectory + String(text.dropFirst())
            return .folder(URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL)
        }
        guard text.hasPrefix("/") else { throw .relativePath(text) }
        return .folder(URL(fileURLWithPath: text, isDirectory: true).standardizedFileURL)
    }

    /// The value as saved when it is valid: trimmed, and a mirror URL or folder path without
    /// trailing slashes (`/` itself stays; `file://` URLs are kept as typed). Throws when the
    /// value can't be a model source.
    public static func normalize(_ raw: String) throws(ModelSourceError) -> String {
        _ = try parse(raw)
        return normalizeSetting(raw)
    }

    /// The settings normalisation (Rust `normalize_setting`): like `normalize`, but a value that
    /// doesn't parse is kept, trimmed, so the error shows where it is used rather than the app
    /// quietly falling back to Hugging Face.
    public static func normalizeSetting(_ raw: String) -> String {
        let text = raw.replacingOccurrences(of: "\0", with: "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard (try? parse(text)) != nil, !text.lowercased().hasPrefix("file://") else { return text }
        return trimmingSlashes(text)
    }

    /// Why `raw` can't be a model source, or nil when it can.
    public static func problem(_ raw: String) -> String? {
        do {
            _ = try parse(raw)
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    private static func trimmingSlashes(_ text: String) -> String {
        var t = text
        while t.count > 1, t.hasSuffix("/") { t.removeLast() }
        // `file:///` and `https://` keep their own slashes; only a path's trailing ones go.
        if t.hasSuffix(":/") || t.hasSuffix(":") { return text }
        return t
    }

    private static func mirrorURL(_ text: String) throws(ModelSourceError) -> URL {
        guard let components = URLComponents(string: text), let url = components.url,
              let host = components.host, !host.isEmpty else { throw .invalidURL(text) }
        if components.user != nil || components.password != nil { throw .credentials(host) }
        if components.query != nil || components.fragment != nil { throw .queryOrFragment(text) }
        return url
    }

    /// `smb` for `smb://server/share`. Only a scheme followed by `://` counts, so a folder name
    /// with a colon in it is still a path.
    private static func scheme(of text: String) -> String? {
        guard let range = text.range(of: "://") else { return nil }
        let scheme = text[..<range.lowerBound]
        guard let first = scheme.first, first.isLetter,
              scheme.allSatisfy({ $0.isLetter || $0.isNumber || "+-.".contains($0) }) else { return nil }
        return String(scheme)
    }

    // MARK: Locations

    /// The base URL downloads go to (Hugging Face or the mirror); nil for a folder.
    public var baseURL: URL? {
        switch self {
        case .huggingFace: Self.huggingFaceURL
        case .mirror(let url): url
        case .folder: nil
        }
    }

    /// `{base}/{repo}/resolve/{revision}/{file}`; nil for a folder or an unusable repo path.
    public func fileURL(repo: String, revision: String = "main", file: String) -> URL? {
        guard let base = baseURL, Self.repoComponents(repo) != nil else { return nil }
        return URL(string: "\(base.absoluteString)/\(repo)/resolve/\(revision)/\(file)")
    }

    /// `{base}/api/models/{repo}/tree/{revision}`, the file list FluidAudio reads before it downloads.
    public func treeURL(repo: String, revision: String = "main") -> URL? {
        guard let base = baseURL, Self.repoComponents(repo) != nil else { return nil }
        return URL(string: "\(base.absoluteString)/api/models/\(repo)/tree/\(revision)")
    }

    /// `{folder}/{owner}/{repo-name}` for a folder source; nil otherwise.
    public func repoDirectory(_ repo: String) -> URL? {
        guard case .folder(let folder) = self, let parts = Self.repoComponents(repo) else { return nil }
        return parts.reduce(folder) { $0.appendingPathComponent($1, isDirectory: true) }
    }

    /// Where a model comes from, for messages: `https://huggingface.co/FluidInference/…`, the
    /// mirror's equivalent, or the folder path.
    public func location(of repo: String) -> String {
        switch self {
        case .huggingFace, .mirror: (baseURL?.absoluteString ?? "") + "/" + repo
        case .folder: repoDirectory(repo)?.plainPath ?? repo
        }
    }

    /// What the settings show: "Hugging Face", the mirror URL or the folder path.
    public var displayName: String {
        switch self {
        case .huggingFace: "Hugging Face"
        case .mirror(let url): url.absoluteString
        case .folder(let url): url.plainPath
        }
    }

    /// `owner/name` split into path components; nil when empty or with `.`/`..` segments.
    static func repoComponents(_ repo: String) -> [String]? {
        let parts = repo.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        guard parts.count >= 2, parts.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." && !$0.contains("\\") }) else { return nil }
        return parts
    }
}

/// What went wrong with a model source, always naming the value, URL or folder involved.
public enum ModelSourceError: Error, Equatable, Sendable, LocalizedError {
    case tooLong(Int)
    case unsupportedScheme(String)
    /// The host of a URL that carried a user name or password (never the credentials).
    case credentials(String)
    case invalidURL(String)
    case queryOrFragment(String)
    case relativePath(String)
    case remoteFileURL(String)
    /// The model's folder isn't there.
    case folderMissing(path: String)
    /// The folder is there but lacks some of the model's files.
    case filesMissing(path: String, files: [String])
    case copyFailed(path: String, reason: String)
    case downloadFailed(location: String, reason: String)

    public var errorDescription: String? {
        switch self {
        case .tooLong(let n):
            "The model source is \(n) characters long; the most is \(ModelSource.maxLength)."
        case .unsupportedScheme(let scheme):
            "The model source can't be a \(scheme):// address. Use an http:// or https:// mirror URL, or a folder path."
        case .credentials(let host):
            "The model source URL for \(host) contains a user name or password. Leave credentials out of the URL."
        case .invalidURL(let text):
            "The model source “\(text)” isn't a valid URL."
        case .queryOrFragment(let text):
            "The model source URL “\(text)” can't have a query (?…) or fragment (#…). Use the mirror's base URL."
        case .relativePath(let text):
            "The model source “\(text)” is a relative path. Use a full folder path such as /Volumes/Models or ~/Models, or an http(s):// mirror URL."
        case .remoteFileURL(let text):
            "The model source “\(text)” names another computer. Mount the share and use its folder path (/Volumes/…)."
        case .folderMissing(let path):
            "No model folder at \(path)."
        case .filesMissing(let path, let files):
            "\(path) is missing \(files.joined(separator: ", "))."
        case .copyFailed(let path, let reason):
            "Couldn't copy the model from \(path): \(reason)"
        case .downloadFailed(let location, let reason):
            "Download from \(location) failed: \(reason)"
        }
    }
}

/// The outcome of the Test button: whether the source can serve a model, and what was checked.
public struct ModelSourceCheck: Equatable, Sendable {
    public var ok: Bool
    public var message: String

    public init(ok: Bool, message: String) {
        self.ok = ok
        self.message = message
    }
}

/// Checks a source can serve one model, without downloading it. For Hugging Face or a mirror:
/// a HEAD of the model's first file and a GET of its file list (FluidAudio needs both). For a
/// folder: the model's folder holds every required file.
public enum ModelSourceProbe {
    public static func check(_ source: ModelSource, repo: String, revision: String = "main", requiredFiles: [String],
                             session: URLSession = .shared, timeout: TimeInterval = 15) async -> ModelSourceCheck {
        switch source {
        case .folder:
            guard let dir = source.repoDirectory(repo) else { return ModelSourceCheck(ok: false, message: "“\(repo)” isn't a repository path.") }
            let path = dir.plainPath
            var isDirectory: ObjCBool = false
            guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory), isDirectory.boolValue else {
                return ModelSourceCheck(ok: false, message: ModelSourceError.folderMissing(path: path).localizedDescription)
            }
            let missing = ModelFolderInstaller.missingFiles(in: dir, required: requiredFiles)
            guard missing.isEmpty else {
                return ModelSourceCheck(ok: false, message: ModelSourceError.filesMissing(path: path, files: missing).localizedDescription)
            }
            return ModelSourceCheck(ok: true, message: "Found \(repo) in \(path).")
        case .huggingFace, .mirror:
            guard let first = requiredFiles.first(where: { !$0.hasSuffix(".mlmodelc") && !$0.hasSuffix(".mlpackage") }) ?? requiredFiles.first,
                  let fileURL = source.fileURL(repo: repo, revision: revision, file: first),
                  let treeURL = source.treeURL(repo: repo, revision: revision) else {
                return ModelSourceCheck(ok: false, message: "“\(repo)” isn't a repository path.")
            }
            var head = URLRequest(url: fileURL, timeoutInterval: timeout)
            head.httpMethod = "HEAD"
            do {
                let (_, response) = try await session.data(for: head)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard (200..<300).contains(status) else {
                    return ModelSourceCheck(ok: false, message: "HTTP \(status) for \(fileURL.absoluteString)")
                }
            } catch {
                return ModelSourceCheck(ok: false, message: "Couldn't reach \(fileURL.absoluteString): \(describe(error))")
            }
            do {
                let (data, response) = try await session.data(for: URLRequest(url: treeURL, timeoutInterval: timeout))
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard (200..<300).contains(status) else {
                    return ModelSourceCheck(ok: false, message: "The file list isn't served: HTTP \(status) for \(treeURL.absoluteString)")
                }
                guard (try? JSONSerialization.jsonObject(with: data)) is [Any] else {
                    return ModelSourceCheck(ok: false, message: "The file list at \(treeURL.absoluteString) isn't a Hugging Face file list.")
                }
            } catch {
                return ModelSourceCheck(ok: false, message: "Couldn't reach \(treeURL.absoluteString): \(describe(error))")
            }
            let host = source.baseURL?.host() ?? source.displayName
            return ModelSourceCheck(ok: true, message: "\(host) serves \(repo).")
        }
    }
}

extension ModelSourceProbe {
    /// A network error in words. macOS allows plain http:// only to local addresses (loopback,
    /// private addresses, `.local` and single-label names), so say so rather than "the resource
    /// could not be loaded".
    public static func describe(_ error: Error) -> String {
        if let urlError = error as? URLError, urlError.code == .appTransportSecurityRequiresSecureConnection {
            return "macOS allows plain http:// only on the local network. Use the mirror's https:// address."
        }
        return error.localizedDescription
    }
}

/// Installs a model from a folder source: copies `{folder}/{owner}/{repo-name}` into the
/// directory the speech library loads from, through a staging folder, then verifies it.
public enum ModelFolderInstaller {
    /// The entries of `required` (paths relative to `directory`) that aren't there.
    public static func missingFiles(in directory: URL, required: [String]) -> [String] {
        required.filter { !FileManager.default.fileExists(atPath: directory.appendingPathComponent($0).plainPath) }
    }

    /// Copies what the speech library uses from `source` into `destination`: every required
    /// entry (a `.mlmodelc` bundle or a file) and any other top-level file (configs, vocabularies).
    /// Hidden entries (`.cache`, `.gitattributes`) and other bundles (`.mlpackage` sources,
    /// encoder variants) stay behind. The copy goes to a staging folder next to `destination`
    /// and replaces it only when complete; `verify` then lists anything still missing, and if it
    /// does the copy is removed. Synchronous: call it off the main thread.
    public static func install(from source: URL, to destination: URL, required: [String],
                               progress: (Double) -> Void = { _ in },
                               verify: (URL) -> [String]) throws(ModelSourceError) {
        let fm = FileManager.default
        let sourcePath = source.plainPath
        var isDirectory: ObjCBool = false
        guard fm.fileExists(atPath: sourcePath, isDirectory: &isDirectory), isDirectory.boolValue else {
            throw .folderMissing(path: sourcePath)
        }
        let missing = missingFiles(in: source, required: required)
        guard missing.isEmpty else { throw .filesMissing(path: sourcePath, files: missing) }

        let parent = destination.deletingLastPathComponent()
        let staging = parent.appendingPathComponent(".\(destination.lastPathComponent).copying-\(UUID().uuidString)", isDirectory: true)
        do {
            var entries = Set(required)
            let top = try fm.contentsOfDirectory(at: source, includingPropertiesForKeys: [.isDirectoryKey], options: [.skipsHiddenFiles])
            for item in top where !isDirectoryResolvingLinks(item) { entries.insert(item.lastPathComponent) }
            let plan = try entries.sorted().flatMap { try filesUnder(source.appendingPathComponent($0), relativePath: $0) }
            let total = max(1, plan.reduce(Int64(0)) { $0 + $1.size })
            var copied: Int64 = 0
            try fm.createDirectory(at: staging, withIntermediateDirectories: true)
            progress(0)
            for file in plan {
                let target = staging.appendingPathComponent(file.relativePath)
                try fm.createDirectory(at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
                try copyFile(file.url, to: target) { bytes in
                    copied += bytes
                    progress(min(1, Double(copied) / Double(total)))
                }
            }
            if fm.fileExists(atPath: destination.plainPath) { try fm.removeItem(at: destination) }
            try fm.moveItem(at: staging, to: destination)
        } catch {
            try? fm.removeItem(at: staging)
            throw .copyFailed(path: sourcePath, reason: error.localizedDescription)
        }
        let stillMissing = verify(destination)
        guard stillMissing.isEmpty else {
            try? fm.removeItem(at: destination)
            throw .filesMissing(path: sourcePath, files: stillMissing)
        }
        progress(1)
    }

    private struct PlannedFile {
        var url: URL
        var relativePath: String
        var size: Int64
    }

    private static func isDirectoryResolvingLinks(_ url: URL) -> Bool {
        var isDirectory: ObjCBool = false
        return FileManager.default.fileExists(atPath: url.resolvingSymlinksInPath().plainPath, isDirectory: &isDirectory)
            && isDirectory.boolValue
    }

    /// Every file under `url` (itself, if a file), following symbolic links (`hf download`
    /// without `--local-dir` links into its cache).
    private static func filesUnder(_ url: URL, relativePath: String, depth: Int = 0) throws -> [PlannedFile] {
        let resolved = url.resolvingSymlinksInPath()
        guard depth < 32 else { return [] }
        if isDirectoryResolvingLinks(url) {
            let children = try FileManager.default.contentsOfDirectory(at: resolved, includingPropertiesForKeys: nil)
            return try children.sorted { $0.lastPathComponent < $1.lastPathComponent }.flatMap {
                try filesUnder($0, relativePath: relativePath + "/" + $0.lastPathComponent, depth: depth + 1)
            }
        }
        let attributes = try FileManager.default.attributesOfItem(atPath: resolved.plainPath)
        let size = (attributes[.size] as? NSNumber)?.int64Value ?? 0
        return [PlannedFile(url: resolved, relativePath: relativePath, size: size)]
    }

    /// Streams in 4 MB chunks so a large weight file on a network share reports progress.
    private static func copyFile(_ source: URL, to target: URL, copied: (Int64) -> Void) throws {
        let input = try FileHandle(forReadingFrom: source)
        defer { try? input.close() }
        guard FileManager.default.createFile(atPath: target.plainPath, contents: nil) else {
            throw CocoaError(.fileWriteUnknown, userInfo: [NSFilePathErrorKey: target.plainPath])
        }
        let output = try FileHandle(forWritingTo: target)
        defer { try? output.close() }
        while let chunk = try input.read(upToCount: 4 << 20), !chunk.isEmpty {
            try output.write(contentsOf: chunk)
            copied(Int64(chunk.count))
        }
    }
}

extension URL {
    /// The file path, decoded and without a trailing slash (`/` stays), for messages and checks.
    var plainPath: String {
        var p = path(percentEncoded: false)
        while p.count > 1, p.hasSuffix("/") { p.removeLast() }
        return p
    }
}
