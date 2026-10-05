import Foundation
import FairspokenCore
import OSLog

/// Phase 0 local session: the recording is transcribed in one batch on release
/// (the spike measured 40–80 ms for anything up to ~15 s on the ANE).
nonisolated final class LocalBatchSession: TranscriptionSession, @unchecked Sendable {
    private let engine: any SpeechEngine
    private let options: TranscriptionRequestOptions

    init(engine: any SpeechEngine, options: TranscriptionRequestOptions) {
        self.engine = engine
        self.options = options
    }

    func append(_ samples16k: [Float]) {}

    func finish(allSamples: [Float]) async throws -> TranscriptionOutput {
        try await engine.transcribe(allSamples, options: options)
    }

    func cancel() {}
}

/// Streams the recording to a transcription host while the user speaks
/// (`POST /v1/transcriptions/stream`, framed PCM16 at 16 kHz, chunked body), exactly as
/// `start_remote_streaming_session` in the Rust client: no total timeout while streaming,
/// and `remoteTimeoutSeconds` both as the connect timeout and as the wait for the
/// transcript after the stream ends.
nonisolated final class RemoteStreamSession: TranscriptionSession, @unchecked Sendable {
    enum RemoteError: LocalizedError {
        case timedOut(Int)
        case invalidResponse(String)
        case noSpeech
        case http(String)
        case unreachable(String)

        var errorDescription: String? {
            switch self {
            case .timedOut(let s): "Remote transcription host did not respond within \(s)s of the recording ending"
            case .invalidResponse(let d): "Remote streaming transcription response was invalid: \(d)"
            case .noSpeech: "Remote streaming transcription returned no speech"
            case .http(let m): m
            case .unreachable(let d): "Remote transcription host is unreachable: \(d)"
            }
        }
    }

    private let writer: BodyStreamWriter
    private let responseTask: Task<(Data, URLResponse), Error>
    private let timeoutSeconds: Int
    private let started = ContinuousClock.now

    init(baseURL: URL, options: RemoteProtocol.Options, timeoutSeconds: Int) {
        var input: InputStream?
        var output: OutputStream?
        Stream.getBoundStreams(withBufferSize: 64 * 1024, inputStream: &input, outputStream: &output)
        writer = BodyStreamWriter(stream: output!)
        self.timeoutSeconds = max(5, timeoutSeconds)

        var request = URLRequest(url: RemoteURLPolicy.endpoint(baseURL, RemoteProtocol.streamPath))
        request.httpMethod = "POST"
        request.httpBodyStream = input
        for (k, v) in RemoteProtocol.transcriptionHeaders(options) { request.setValue(v, forHTTPHeaderField: k) }
        request.setValue(StreamFrameCodec.contentType, forHTTPHeaderField: "Content-Type")

        let config = URLSessionConfiguration.ephemeral
        // Idle timeout between packets; audio keeps flowing while the user speaks.
        config.timeoutIntervalForRequest = TimeInterval(max(5, timeoutSeconds))
        config.timeoutIntervalForResource = 3600
        config.waitsForConnectivity = false
        let session = URLSession(configuration: config)
        responseTask = Task.detached {
            defer { session.finishTasksAndInvalidate() }
            return try await session.data(for: request)
        }
        writer.open()
    }

    func append(_ samples16k: [Float]) {
        guard !samples16k.isEmpty else { return }
        writer.enqueue(StreamFrameCodec.encodeFrames(sampleRate: 16_000, samples: PCM.int16(from: samples16k)))
    }

    func finish(allSamples: [Float]) async throws -> TranscriptionOutput {
        writer.close()
        let timeout = timeoutSeconds
        let (data, response) = try await withThrowingTaskGroup(of: (Data, URLResponse).self) { group in
            group.addTask { [responseTask] in
                do { return try await responseTask.value } catch let error as URLError {
                    throw RemoteError.unreachable(error.localizedDescription)
                }
            }
            group.addTask {
                try await Task.sleep(for: .seconds(timeout))
                throw RemoteError.timedOut(timeout)
            }
            defer { group.cancelAll() }
            return try await group.next()!
        }
        guard let http = response as? HTTPURLResponse else { throw RemoteError.invalidResponse("not HTTP") }
        guard (200..<300).contains(http.statusCode) else {
            throw RemoteError.http(RemoteProtocol.errorMessage(statusCode: http.statusCode, body: data))
        }
        let result: RemoteTranscriptionResponse
        do { result = try JSONDecoder().decode(RemoteTranscriptionResponse.self, from: data) } catch {
            throw RemoteError.invalidResponse(error.localizedDescription)
        }
        guard !result.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw RemoteError.noSpeech }
        let ms = Int((ContinuousClock.now - started) / .milliseconds(1))
        return TranscriptionOutput(text: result.text, modelMs: ms, model: result.model, backend: result.backend, placement: .remoteHost)
    }

    func cancel() {
        writer.close()
        responseTask.cancel()
    }
}

/// Feeds a bound `OutputStream` from a serial queue. Writes block briefly when the 64 KB
/// pipe is full (a slow link delays audio rather than dropping it, like the Rust relay).
nonisolated final class BodyStreamWriter: @unchecked Sendable {
    private let stream: OutputStream
    private let queue = DispatchQueue(label: "remote-stream-writer", qos: .userInitiated)
    private var closed = false

    init(stream: OutputStream) { self.stream = stream }

    func open() { queue.async { self.stream.open() } }

    func enqueue(_ data: Data) {
        queue.async {
            guard !self.closed else { return }
            data.withUnsafeBytes { raw in
                guard let base = raw.bindMemory(to: UInt8.self).baseAddress else { return }
                var offset = 0
                var stalls = 0
                while offset < data.count, !self.closed {
                    if self.stream.streamStatus == .error || self.stream.streamStatus == .closed { self.closed = true; return }
                    if !self.stream.hasSpaceAvailable {
                        stalls += 1
                        if stalls > 30_000 { self.closed = true; return } // ~60 s with nothing read: give up
                        usleep(2_000)
                        continue
                    }
                    let n = self.stream.write(base + offset, maxLength: data.count - offset)
                    if n < 0 { self.closed = true; return }
                    offset += n
                    stalls = 0
                }
            }
        }
    }

    func close() {
        queue.async {
            guard !self.closed else { return }
            self.closed = true
            self.stream.close()
        }
    }
}

/// `GET /v1/health` — the Settings "Test connection" button.
nonisolated enum RemoteHostClient {
    static func health(baseURL: String, token: String, timeout: TimeInterval = 8) async throws -> RemoteHealth {
        let base = try RemoteURLPolicy.validateBaseURL(baseURL)
        var request = URLRequest(url: RemoteURLPolicy.endpoint(base, RemoteProtocol.healthPath), timeoutInterval: timeout)
        for (k, v) in RemoteProtocol.authHeaders(token: token) { request.setValue(v, forHTTPHeaderField: k) }
        let (data, response) = try await URLSession.shared.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            throw RemoteStreamSession.RemoteError.http(RemoteProtocol.errorMessage(statusCode: status, body: data))
        }
        return try JSONDecoder().decode(RemoteHealth.self, from: data)
    }
}
