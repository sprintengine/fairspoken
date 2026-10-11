import Foundation

/// Why a transcription on the host failed. The messages are the Rust client's.
public enum RemoteTranscriptionError: LocalizedError, Equatable, Sendable {
    case timedOut(Int)
    case invalidResponse(String)
    case noSpeech
    case http(status: Int, message: String)
    case unreachable(String)

    public var errorDescription: String? {
        switch self {
        case .timedOut(let s): "Remote transcription host did not respond within \(s)s of the recording ending"
        case .invalidResponse(let d): "Remote streaming transcription response was invalid: \(d)"
        case .noSpeech: "Remote streaming transcription returned no speech"
        case .http(_, let m): m
        case .unreachable(let d): "Remote transcription host is unreachable: \(d)"
        }
    }

    /// Worth sending the recording again: the host was down, busy, timed out or failed on its
    /// side. A refusal (bad token, too long, bad request) or "no speech" would only repeat.
    public var isRetryable: Bool {
        switch self {
        case .timedOut, .invalidResponse, .unreachable: true
        case .http(let status, _): status == 408 || status == 429 || status >= 500
        case .noSpeech: false
        }
    }

    /// The answer to a stream or batch upload: the transcript, or why there isn't one.
    public static func interpret(status: Int, body: Data, elapsedMs: Int) throws(RemoteTranscriptionError) -> TranscriptionOutput {
        guard (200..<300).contains(status) else {
            throw .http(status: status, message: RemoteProtocol.errorMessage(statusCode: status, body: body))
        }
        let result: RemoteTranscriptionResponse
        do { result = try JSONDecoder().decode(RemoteTranscriptionResponse.self, from: body) } catch {
            throw .invalidResponse(error.localizedDescription)
        }
        guard !result.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw .noSpeech }
        return TranscriptionOutput(text: result.text, modelMs: elapsedMs, model: result.model, backend: result.backend, placement: .remoteHost)
    }
}

/// `POST /v1/transcriptions`: a whole recording as one WAV upload. This is how a kept dictation
/// goes back to the host. A stream can't be resumed, so a retry always sends all of the audio.
public struct RemoteBatchClient: Sendable {
    public var baseURL: URL
    public var options: RemoteProtocol.Options
    public var transport: any HTTPTransport

    public init(baseURL: URL, options: RemoteProtocol.Options, transport: any HTTPTransport = URLSessionTransport()) {
        self.baseURL = baseURL
        self.options = options
        self.transport = transport
    }

    /// `timeout` bounds the whole exchange: upload, the host's queue and the decode.
    public func transcribe(wav: Data, timeout: Duration) async throws -> TranscriptionOutput {
        var request = URLRequest(url: RemoteURLPolicy.endpoint(baseURL, RemoteProtocol.batchPath))
        request.httpMethod = "POST"
        request.httpBody = wav
        request.timeoutInterval = max(1, Double(timeout.components.seconds))
        for (k, v) in RemoteProtocol.transcriptionHeaders(options) { request.setValue(v, forHTTPHeaderField: k) }
        request.setValue("audio/wav", forHTTPHeaderField: "Content-Type")
        let started = ContinuousClock.now
        let seconds = Int(timeout.components.seconds)
        let (data, response) = try await withThrowingTaskGroup(of: (Data, HTTPURLResponse).self) { group in
            group.addTask { [transport, request] in
                do { return try await transport.data(for: request) } catch let error as URLError {
                    throw error.code == .timedOut ? RemoteTranscriptionError.timedOut(seconds)
                        : RemoteTranscriptionError.unreachable(error.localizedDescription)
                }
            }
            group.addTask {
                try await Task.sleep(for: timeout)
                throw RemoteTranscriptionError.timedOut(seconds)
            }
            defer { group.cancelAll() }
            return try await group.next()!
        }
        let ms = Int((ContinuousClock.now - started) / .milliseconds(1))
        return try RemoteTranscriptionError.interpret(status: response.statusCode, body: data, elapsedMs: ms)
    }
}
