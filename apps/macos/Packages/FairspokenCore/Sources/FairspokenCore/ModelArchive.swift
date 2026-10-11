import Foundation

/// The archive formats a model link may point at, told apart by their first bytes rather than
/// the file name (a link like `…/download?id=42` has none).
public enum ArchiveFormat: String, Equatable, Sendable {
    case zip
    case tar
    /// gzip, unpacked as a tar.gz.
    case gzip

    /// How many leading bytes `detect` needs (a tar's `ustar` magic sits at offset 257).
    public static let headerLength = 512

    /// The format of a file starting with `header`, or nil when it is none of them.
    public static func detect(_ header: Data) -> ArchiveFormat? {
        let bytes = [UInt8](header.prefix(headerLength))
        if bytes.count >= 4, bytes[0] == 0x50, bytes[1] == 0x4B,
           (bytes[2] == 0x03 && bytes[3] == 0x04) || (bytes[2] == 0x05 && bytes[3] == 0x06) || (bytes[2] == 0x07 && bytes[3] == 0x08) {
            return .zip
        }
        if bytes.count >= 2, bytes[0] == 0x1F, bytes[1] == 0x8B { return .gzip }
        // POSIX `ustar\0` and old GNU `ustar  \0`.
        if bytes.count >= 262, Array(bytes[257..<262]) == Array("ustar".utf8) { return .tar }
        return nil
    }

    /// The format of the file at `url`; nil when it is none of them or can't be read.
    public static func detect(fileAt url: URL) -> ArchiveFormat? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        return detect((try? handle.read(upToCount: headerLength)) ?? Data())
    }

    /// True when `header` looks like an HTML page (what a sign-in redirect returns).
    public static func looksLikeWebPage(_ header: Data) -> Bool {
        let text = String(decoding: header.prefix(headerLength), as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return text.hasPrefix("<!doctype html") || text.hasPrefix("<html") || text.hasPrefix("<head") || text.hasPrefix("<?xml")
    }
}

/// Unpacks and checks a downloaded model archive with the system tools (`/usr/bin/ditto` for
/// zip, `/usr/bin/tar` for tar and tar.gz), and finds the model's folder inside it.
public enum ModelArchive {
    /// What an archive may unpack to.
    public struct Limits: Equatable, Sendable {
        public var maxBytes: Int64
        public var maxEntries: Int

        public init(maxBytes: Int64 = 8 << 30, maxEntries: Int = 10_000) {
            self.maxBytes = maxBytes
            self.maxEntries = maxEntries
        }

        public static let standard = Limits()
    }

    /// Why an archive can't be unpacked; `ModelLinkInstaller` turns these into messages that
    /// name the link.
    public enum Failure: Error, Equatable, Sendable {
        case unsafeEntry(String)
        case tooLarge
        case tooManyEntries
        case toolFailed(String)
    }

    // MARK: Listing

    /// The entry names, read with `zipinfo -1` or `tar -tf` without unpacking anything.
    /// `scratch` is an empty folder for the tool's output.
    public static func entryNames(of archive: URL, format: ArchiveFormat, scratch: URL, limits: Limits = .standard) async throws(Failure) -> [String] {
        let listing = scratch.appendingPathComponent("listing.txt")
        let (executable, arguments) = format == .zip
            ? ("/usr/bin/zipinfo", ["-1", archive.plainPath])
            : ("/usr/bin/tar", ["-tf", archive.plainPath])
        // A listing longer than this has far more entries than the limit allows.
        let maxListingBytes = Int64(limits.maxEntries) * 4096
        try await run(executable, arguments, stdout: listing, scratch: scratch) {
            let size = (try? FileManager.default.attributesOfItem(atPath: listing.plainPath)[.size] as? NSNumber)?.int64Value ?? 0
            if size > maxListingBytes { throw Failure.tooManyEntries }
        }
        let text = (try? Data(contentsOf: listing)).map { String(decoding: $0, as: UTF8.self) } ?? ""
        try? FileManager.default.removeItem(at: listing)
        return text.split(separator: "\n", omittingEmptySubsequences: true).map(String.init)
    }

    /// The first entry name that would land outside the folder it is unpacked into (an absolute
    /// path or a `..` component), or nil when all stay inside.
    public static func unsafeEntry(in names: [String]) -> String? {
        names.first { name in
            name.hasPrefix("/") || name.hasPrefix("\\")
                || name.split(whereSeparator: { $0 == "/" || $0 == "\\" }).contains("..")
        }
    }

    // MARK: Unpacking

    /// Lists the archive, refuses unsafe names and oversized listings, unpacks it into
    /// `destination` (an empty folder) while watching its size, then checks the result: no more
    /// than `limits` and no symbolic link pointing outside `destination`. On failure whatever
    /// was unpacked is left for the caller to remove with its work folder.
    public static func unpack(_ archive: URL, format: ArchiveFormat, into destination: URL, scratch: URL,
                              limits: Limits = .standard) async throws(Failure) {
        let names = try await entryNames(of: archive, format: format, scratch: scratch, limits: limits)
        if let entry = unsafeEntry(in: names) { throw .unsafeEntry(entry) }
        guard names.count <= limits.maxEntries else { throw .tooManyEntries }
        let (executable, arguments) = format == .zip
            ? ("/usr/bin/ditto", ["-x", "-k", archive.plainPath, destination.plainPath])
            : ("/usr/bin/tar", ["-xf", archive.plainPath, "-C", destination.plainPath])
        try await run(executable, arguments, stdout: nil, scratch: scratch) {
            _ = try measure(destination, limits: limits)
        }
        _ = try measure(destination, limits: limits)
    }

    /// Counts and sizes everything under `root`, without following links, and refuses a
    /// symbolic link that resolves outside `root`. Throws as soon as a limit is passed.
    @discardableResult
    public static func measure(_ root: URL, limits: Limits) throws(Failure) -> (entries: Int, bytes: Int64) {
        // The walk may report paths through /private (or not) whichever way `root` was given.
        let roots = Set([root.standardizedFileURL.plainPath, root.resolvingSymlinksInPath().plainPath])
        func inside(_ path: String) -> Bool { roots.contains { path == $0 || path.hasPrefix($0 + "/") } }
        let keys: [URLResourceKey] = [.isSymbolicLinkKey, .isRegularFileKey, .fileSizeKey]
        guard let walker = FileManager.default.enumerator(at: root, includingPropertiesForKeys: keys, options: []) else { return (0, 0) }
        var entries = 0
        var bytes: Int64 = 0
        for case let url as URL in walker {
            entries += 1
            if entries > limits.maxEntries { throw .tooManyEntries }
            let values = try? url.resourceValues(forKeys: Set(keys))
            if values?.isSymbolicLink == true {
                // Each link is checked on its own, so a chain through links can't leave either.
                let path = url.standardizedFileURL.plainPath
                let base = roots.filter { path.hasPrefix($0 + "/") }.max { $0.count < $1.count }
                let relative = base.map { String(path.dropFirst($0.count + 1)) } ?? url.lastPathComponent
                guard let destination = try? FileManager.default.destinationOfSymbolicLink(atPath: url.plainPath) else {
                    throw .unsafeEntry(relative)
                }
                let target = destination.hasPrefix("/")
                    ? URL(fileURLWithPath: destination)
                    : URL(fileURLWithPath: path).deletingLastPathComponent().appendingPathComponent(destination)
                guard inside(target.standardizedFileURL.plainPath) else { throw .unsafeEntry(relative) }
            } else if values?.isRegularFile == true {
                bytes += Int64(values?.fileSize ?? 0)
                if bytes > limits.maxBytes { throw .tooLarge }
            }
        }
        return (entries, bytes)
    }

    /// Runs a system tool, polling `check` while it runs (it throws to stop the tool), and
    /// fails with the tool's last error line when it exits non-zero. Cancelling the task stops it.
    private static func run(_ executable: String, _ arguments: [String], stdout: URL?, scratch: URL,
                            check: () throws -> Void) async throws(Failure) {
        let errors = scratch.appendingPathComponent("stderr-\(UUID().uuidString).txt")
        defer { try? FileManager.default.removeItem(at: errors) }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        do {
            FileManager.default.createFile(atPath: errors.plainPath, contents: nil)
            process.standardError = try FileHandle(forWritingTo: errors)
            if let stdout {
                FileManager.default.createFile(atPath: stdout.plainPath, contents: nil)
                process.standardOutput = try FileHandle(forWritingTo: stdout)
            } else {
                process.standardOutput = FileHandle.nullDevice
            }
            try process.run()
        } catch {
            throw .toolFailed(error.localizedDescription)
        }
        // Polled rather than `waitUntilExit()`, which needs a run loop this thread may not have.
        func stop() async {
            process.terminate()
            for _ in 0..<50 where process.isRunning { try? await Task.sleep(for: .milliseconds(100)) }
            if process.isRunning { kill(process.processIdentifier, SIGKILL) }
            while process.isRunning { try? await Task.sleep(for: .milliseconds(50)) }
        }
        while process.isRunning {
            try? await Task.sleep(for: .milliseconds(200))
            if Task.isCancelled {
                await stop()
                throw .toolFailed("Cancelled.")
            }
            do { try check() } catch let failure as Failure {
                await stop()
                throw failure
            } catch {
                await stop()
                throw .toolFailed(error.localizedDescription)
            }
        }
        guard process.terminationStatus == 0 else {
            let text = (try? Data(contentsOf: errors)).map { String(decoding: $0, as: UTF8.self) } ?? ""
            let line = text.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }.last { !$0.isEmpty }
            throw .toolFailed(line ?? "\(URL(fileURLWithPath: executable).lastPathComponent) exited with status \(process.terminationStatus).")
        }
    }

    // MARK: Finding the model

    /// The shallowest folder under `root` (`root` itself included) that holds every entry of
    /// `required`, or nil. Hidden folders, `__MACOSX` and bundles (`.mlmodelc`, `.mlpackage`)
    /// are not searched, and links are not followed.
    public static func findModelDirectory(in root: URL, required: [String], maxDepth: Int = 8) -> URL? {
        var level = [root]
        for _ in 0...maxDepth {
            var next: [URL] = []
            for dir in level {
                if ModelDirectoryInstaller.missingFiles(in: dir, required: required).isEmpty { return dir }
                let children = (try? FileManager.default.contentsOfDirectory(
                    at: dir, includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey], options: [.skipsHiddenFiles])) ?? []
                for child in children.sorted(by: { $0.lastPathComponent < $1.lastPathComponent }) {
                    let values = try? child.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
                    guard values?.isDirectory == true, values?.isSymbolicLink != true else { continue }
                    let name = child.lastPathComponent
                    if name == "__MACOSX" || name.hasSuffix(".mlmodelc") || name.hasSuffix(".mlpackage") { continue }
                    next.append(child)
                }
            }
            if next.isEmpty { return nil }
            level = next
        }
        return nil
    }
}

/// Installs a model folder into the directory the speech library loads from: copies the
/// bundles and files it uses into a staging folder next to the destination, swaps it in, then
/// verifies it. Synchronous: call it off the main thread.
public enum ModelDirectoryInstaller {
    public enum Failure: Error, Equatable, Sendable {
        /// The model's files that are missing (before copying, or after, from `verify`).
        case filesMissing([String])
        case copyFailed(String)
    }

    /// The entries of `required` (paths relative to `directory`) that aren't there.
    public static func missingFiles(in directory: URL, required: [String]) -> [String] {
        required.filter { !FileManager.default.fileExists(atPath: directory.appendingPathComponent($0).plainPath) }
    }

    /// Copies what the speech library uses from `source` into `destination`: every required
    /// entry (a `.mlmodelc` bundle or a file) and any other top-level file (configs, vocabularies).
    /// Hidden entries and other folders (`.mlpackage` sources, encoder variants) stay behind.
    /// `destination` is replaced only by a complete copy; `verify` then lists anything still
    /// missing, and if it does the copy is removed.
    public static func install(from source: URL, to destination: URL, required: [String],
                               progress: (Double) -> Void = { _ in },
                               verify: (URL) -> [String]) throws(Failure) {
        let fm = FileManager.default
        let missing = missingFiles(in: source, required: required)
        guard missing.isEmpty else { throw .filesMissing(missing) }

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
            throw .copyFailed(error.localizedDescription)
        }
        let stillMissing = verify(destination)
        guard stillMissing.isEmpty else {
            try? fm.removeItem(at: destination)
            throw .filesMissing(stillMissing)
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

    /// Every file under `url` (itself, if a file), following symbolic links (the archive was
    /// checked to have none pointing outside it).
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

    /// Streams in 4 MB chunks so a large weight file reports progress.
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
