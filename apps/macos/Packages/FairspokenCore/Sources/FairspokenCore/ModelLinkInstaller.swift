import Foundation
import Synchronization

/// Downloads a model archive from a `ModelLink`, unpacks it, finds the model's folder in it and
/// installs that folder where the speech library loads from. The speech-library parts (the
/// files a model needs, where it goes and how to check it) are passed in, so this has no
/// FluidAudio dependency and is tested with dummy files.
public enum ModelLinkInstaller {
    public enum Stage: Equatable, Sendable {
        /// Downloading (or, for a file link, copying) the archive: 0…1.
        case downloading(Double)
        case unpacking
        /// Copying the model's files into place: 0…1.
        case installing(Double)
    }

    /// One install.
    /// - `modelName`: for messages ("Parakeet TDT 0.6B v3").
    /// - `required`: the bundles and files the model's folder must hold (`Encoder.mlmodelc`, …).
    /// - `destination`: the model's folder, replaced only by a complete, verified copy.
    /// - `workDirectory`: where a temporary folder for the download and unpacking is made
    ///   (and always removed).
    /// - `verify`: what is still missing from the installed folder (empty when it loads).
    public static func install(link: ModelLink, modelName: String, required: [String], destination: URL,
                               workDirectory: URL = FileManager.default.temporaryDirectory,
                               configuration: URLSessionConfiguration = .default,
                               limits: ModelArchive.Limits = .standard,
                               progress: @escaping @Sendable (Stage) -> Void = { _ in },
                               verify: @escaping @Sendable (URL) -> [String]) async throws(ModelLinkError) {
        let shown = link.display
        let work = workDirectory.appendingPathComponent("fairspoken-model-\(UUID().uuidString)", isDirectory: true)
        defer { removeTree(work) }
        do {
            try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
        } catch {
            throw .installFailed(link: shown, reason: error.localizedDescription)
        }

        let archive = work.appendingPathComponent("download")
        progress(.downloading(0))
        try await fetch(link, to: archive, configuration: configuration) { progress(.downloading($0)) }
        if Task.isCancelled { throw .downloadFailed(link: shown, reason: "Cancelled.") }

        guard let format = ArchiveFormat.detect(fileAt: archive) else {
            let header = (try? FileHandle(forReadingFrom: archive)).flatMap { handle in
                defer { try? handle.close() }
                return try? handle.read(upToCount: ArchiveFormat.headerLength)
            } ?? Data()
            throw .notAnArchive(link: shown, webPage: ArchiveFormat.looksLikeWebPage(header))
        }
        progress(.unpacking)
        let unpacked = work.appendingPathComponent("unpacked", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: unpacked, withIntermediateDirectories: true)
            try await ModelArchive.unpack(archive, format: format, into: unpacked, scratch: work, limits: limits)
        } catch let failure as ModelArchive.Failure {
            throw error(failure, link: shown, limits: limits)
        } catch {
            throw .unpackFailed(link: shown, reason: error.localizedDescription)
        }
        // The archive is no longer needed; free its space before the copy.
        try? FileManager.default.removeItem(at: archive)

        guard let folder = ModelArchive.findModelDirectory(in: unpacked, required: required) else {
            throw .missingModelFiles(link: shown, model: modelName, needed: required)
        }
        progress(.installing(0))
        do {
            try ModelDirectoryInstaller.install(from: folder, to: destination, required: required,
                                                progress: { progress(.installing($0)) }, verify: verify)
        } catch {
            switch error {
            case .filesMissing(let files): throw .missingModelFiles(link: shown, model: modelName, needed: files)
            case .copyFailed(let reason): throw .installFailed(link: shown, reason: reason)
            }
        }
    }

    private static func error(_ failure: ModelArchive.Failure, link: String, limits: ModelArchive.Limits) -> ModelLinkError {
        switch failure {
        case .unsafeEntry(let entry): .unsafeEntry(link: link, entry: entry)
        case .tooLarge: .tooLarge(link: link, limit: limits.maxBytes)
        case .tooManyEntries: .tooManyEntries(link: link, limit: limits.maxEntries)
        case .toolFailed(let reason): .unpackFailed(link: link, reason: reason)
        }
    }

    // MARK: Fetching

    /// Downloads a web link (or copies a file link) to `target`, reporting 0…1 when the size is
    /// known. A web link must answer 2xx.
    public static func fetch(_ link: ModelLink, to target: URL, configuration: URLSessionConfiguration = .default,
                             progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws(ModelLinkError) {
        switch link.location {
        case .file(let url):
            try copy(url, to: target, shown: link.display, progress: progress)
        case .web(let url):
            let delegate = DownloadDelegate(target: target, progress: progress)
            let session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
            defer { session.finishTasksAndInvalidate() }
            var request = URLRequest(url: url)
            request.setValue("*/*", forHTTPHeaderField: "Accept")
            let task = session.downloadTask(with: request)
            let outcome: Result<Void, any Error> = await withTaskCancellationHandler {
                await withCheckedContinuation { continuation in
                    delegate.begin(continuation)
                    task.resume()
                }
            } onCancel: {
                task.cancel()
            }
            switch outcome {
            case .success:
                progress(1)
            case .failure(let failure as DownloadDelegate.HTTPStatus):
                throw .downloadFailed(link: link.display, reason: "HTTP \(failure.code)")
            case .failure(let failure):
                if (failure as? URLError)?.code == .cancelled || Task.isCancelled {
                    throw .downloadFailed(link: link.display, reason: "Cancelled.")
                }
                throw .downloadFailed(link: link.display, reason: ModelLinkError.describe(failure))
            }
        }
    }

    /// Copies an archive from this Mac or a mounted share in 4 MB chunks, for progress.
    private static func copy(_ source: URL, to target: URL, shown: String, progress: (Double) -> Void) throws(ModelLinkError) {
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: source.plainPath, isDirectory: &isDirectory) else {
            throw .downloadFailed(link: shown, reason: "There's no file there.")
        }
        if isDirectory.boolValue { throw .isFolder(link: shown) }
        do {
            let total = max(1, (try FileManager.default.attributesOfItem(atPath: source.plainPath)[.size] as? NSNumber)?.int64Value ?? 1)
            let input = try FileHandle(forReadingFrom: source)
            defer { try? input.close() }
            FileManager.default.createFile(atPath: target.plainPath, contents: nil)
            let output = try FileHandle(forWritingTo: target)
            defer { try? output.close() }
            var copied: Int64 = 0
            while let chunk = try input.read(upToCount: 4 << 20), !chunk.isEmpty {
                if Task.isCancelled { throw CancellationError() }
                try output.write(contentsOf: chunk)
                copied += Int64(chunk.count)
                progress(min(1, Double(copied) / Double(total)))
            }
        } catch is CancellationError {
            throw .downloadFailed(link: shown, reason: "Cancelled.")
        } catch {
            throw .downloadFailed(link: shown, reason: error.localizedDescription)
        }
    }

    /// Removes a work folder even when the archive unpacked read-only folders into it.
    static func removeTree(_ url: URL) {
        let fm = FileManager.default
        guard fm.fileExists(atPath: url.plainPath) else { return }
        if (try? fm.removeItem(at: url)) != nil { return }
        if let walker = fm.enumerator(at: url, includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey]) {
            for case let item as URL in walker {
                let values = try? item.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
                if values?.isDirectory == true, values?.isSymbolicLink != true {
                    try? fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: item.plainPath)
                }
            }
        }
        try? fm.removeItem(at: url)
    }
}

/// Receives one download task's progress and file, and finishes its continuation once.
private final class DownloadDelegate: NSObject, URLSessionDownloadDelegate, Sendable {
    struct HTTPStatus: Error {
        var code: Int
    }

    private struct State {
        var continuation: CheckedContinuation<Result<Void, any Error>, Never>?
        var failure: (any Error)?
    }

    let target: URL
    let progress: @Sendable (Double) -> Void
    private let state = Mutex(State())

    init(target: URL, progress: @escaping @Sendable (Double) -> Void) {
        self.target = target
        self.progress = progress
    }

    func begin(_ continuation: CheckedContinuation<Result<Void, any Error>, Never>) {
        state.withLock { $0.continuation = continuation }
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData bytesWritten: Int64,
                    totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64) {
        guard totalBytesExpectedToWrite > 0 else { return }
        progress(min(1, Double(totalBytesWritten) / Double(totalBytesExpectedToWrite)))
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        // The file at `location` is deleted when this returns, so move it now.
        if let http = downloadTask.response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            state.withLock { $0.failure = HTTPStatus(code: http.statusCode) }
            return
        }
        do {
            try? FileManager.default.removeItem(at: target)
            try FileManager.default.moveItem(at: location, to: target)
        } catch {
            state.withLock { $0.failure = error }
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: (any Error)?) {
        let (continuation, recorded) = state.withLock { s in
            defer { s.continuation = nil }
            return (s.continuation, s.failure)
        }
        if let http = task.response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            continuation?.resume(returning: .failure(HTTPStatus(code: http.statusCode)))
        } else if let failure = error ?? recorded {
            continuation?.resume(returning: .failure(failure))
        } else if !FileManager.default.fileExists(atPath: target.path(percentEncoded: false)) {
            continuation?.resume(returning: .failure(URLError(.cannotCreateFile)))
        } else {
            continuation?.resume(returning: .success(()))
        }
    }
}
