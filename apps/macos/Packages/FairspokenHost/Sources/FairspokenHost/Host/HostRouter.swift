import Foundation

/// The HTTP side of the host: authentication, routing and the endpoint handlers of
/// PROTOCOL.md, mirroring `handle_request` and friends in src-tauri/src/host/mod.rs.
public final class HostRouter: Sendable {
    public let runtime: HostRuntime
    private let token: String?
    private let dashboardHTML: [UInt8]
    let heartbeat: Duration

    static let maxConfigBodyBytes = 4 * 1024
    static let maxBatchHeaderBytes = 64 * 1024
    static let maxBatchBytesPerSecond = 192_000 * 2

    public init(runtime: HostRuntime, dashboardHTML: [UInt8], heartbeat: Duration = .seconds(15)) {
        self.runtime = runtime
        self.token = runtime.configuration.authToken
        self.dashboardHTML = dashboardHTML
        self.heartbeat = heartbeat
    }

    public func handle(_ request: HTTPServerRequest) async {
        let head = request.head
        let method = head.method
        let path = head.path
        // The dashboard page holds no secrets and authenticates its own calls.
        if method == "GET" && path == "/" {
            await request.respond(status: 200, contentType: "text/html; charset=utf-8", body: dashboardHTML)
            return
        }
        if method == "GET" && path == "/favicon.ico" {
            await request.respond(status: 204)
            return
        }
        guard authorized(head) else {
            await request.respondJSON(401, .object([("error", .string("unauthorized"))]))
            return
        }
        switch (method, path) {
        case ("GET", "/v1/health"):
            await request.respondJSON(200, .object([
                ("ok", .bool(true)), ("mode", .string("standalone-host")), ("backend", .string(HostRuntime.backendID)),
                ("serverVersion", .string(runtime.serverVersion)),
            ]))
        case ("GET", "/v1/stats"):
            await request.respondJSON(200, runtime.statsJSON())
        case ("GET", "/v1/events"):
            await events(request)
        case ("POST", "/v1/config"):
            await config(request)
        case ("POST", "/v1/models/download"):
            await download(request)
        case ("POST", "/v1/transcriptions"):
            await batch(request)
        case ("POST", "/v1/transcriptions/stream"):
            await stream(request)
        default:
            await request.respondJSON(404, .object([("error", .string("not_found"))]))
        }
    }

    // MARK: Auth and client identity

    /// `Authorization: Bearer <token>` on any method, or `?token=` on GET only.
    func authorized(_ head: HTTPRequestHead) -> Bool {
        guard let token else { return true }
        if let auth = head.header("authorization"), auth.hasPrefix("Bearer "),
           Self.constantTimeEqual(Array(auth.dropFirst(7).utf8), Array(token.utf8)) {
            return true
        }
        if head.method == "GET", let q = head.queryParameter("token"), Self.constantTimeEqual(Array(q.utf8), Array(token.utf8)) {
            return true
        }
        return false
    }

    public static func constantTimeEqual(_ a: [UInt8], _ b: [UInt8]) -> Bool {
        guard a.count == b.count else { return false }
        var diff: UInt8 = 0
        for i in 0..<a.count { diff |= a[i] ^ b[i] }
        return diff == 0
    }

    /// The peer IP; behind `tailscale serve` (loopback), the first `X-Forwarded-For` or
    /// `Tailscale-User-Login` value, which are trusted only from loopback.
    public static func clientAddress(peer: String?, head: HTTPRequestHead) -> String? {
        guard let peer else { return nil }
        if isLoopback(peer) {
            for name in ["x-forwarded-for", "tailscale-user-login"] {
                if let value = head.header(name)?.split(separator: ",", omittingEmptySubsequences: false).first?
                    .trimmingCharacters(in: .whitespaces), !value.isEmpty {
                    return value
                }
            }
        }
        return peer
    }

    static func isLoopback(_ address: String) -> Bool {
        address == "::1" || address.hasPrefix("127.")
    }

    // MARK: Events

    private func events(_ request: HTTPServerRequest) async {
        let subscription: EventSubscription
        let snapshot: JSONValue
        do {
            (subscription, snapshot) = try runtime.subscribe()
        } catch {
            await request.respondError(503, error.message)
            return
        }
        defer { subscription.cancel() }
        let chunked = !request.head.isHTTP10
        var headers: [(String, String)] = [
            ("Content-Type", "text/event-stream"), ("Cache-Control", "no-cache"), ("Connection", "close"), ("X-Accel-Buffering", "no"),
        ]
        if chunked { headers.append(("Transfer-Encoding", "chunked")) }
        func write(_ frame: String) async throws {
            let bytes = Array(frame.utf8)
            try await request.writeRaw(chunked ? HTTPStatus.chunk(bytes) : bytes)
        }
        // A closed tab is noticed straight away rather than at the next heartbeat.
        let watcher = Task { [request] in
            await request.waitForPeerClose()
            subscription.cancel()
        }
        defer { watcher.cancel() }
        do {
            try await request.startStreaming(status: 200, headers: headers)
            try await write(SSE.frame(event: "snapshot", data: snapshot.serialized))
            loop: while true {
                switch await subscription.next(timeout: heartbeat) {
                case .frame(let frame): try await write(frame)
                case .timeout: try await write(SSE.heartbeat)
                case .closed: break loop
                }
            }
        } catch {
            // A write failed: the client went away. Dropping the subscription frees its slot.
        }
    }

    // MARK: Config and models

    private func config(_ request: HTTPServerRequest) async {
        let body: [UInt8]
        do { body = try await request.readBody(limit: Self.maxConfigBodyBytes) } catch {
            await request.respondError(413, "Transcription request body exceeds host maximum upload size")
            return
        }
        let update: HostConfigUpdate
        do { update = try HostConfigUpdate.parse(body) } catch {
            await request.respondError(400, error.message)
            return
        }
        do {
            let next = try runtime.applyConfigUpdate(update)
            await request.respondJSON(200, next.configResponse)
        } catch {
            switch error {
            case .invalid(let m): await request.respondError(400, m)
            case .notSaved(let m): await request.respondError(500, m)
            }
        }
    }

    private func download(_ request: HTTPServerRequest) async {
        let body: [UInt8]
        do { body = try await request.readBody(limit: Self.maxConfigBodyBytes) } catch {
            await request.respondError(413, "Transcription request body exceeds host maximum upload size")
            return
        }
        guard let object = try? JSONSerialization.jsonObject(with: Data(body)) as? [String: Any] else {
            await request.respondError(400, "Invalid model download request: expected a JSON object with a model field")
            return
        }
        for key in object.keys.sorted() where key != "model" {
            await request.respondError(400, "Invalid model download request: unknown field `\(key)`, expected `model`")
            return
        }
        guard let model = object["model"] as? String else {
            await request.respondError(400, "Invalid model download request: missing field `model`")
            return
        }
        do {
            try runtime.startDownload(model)
            await request.respondJSON(202, .object([("model", .string(model)), ("status", .string("downloading"))]))
        } catch {
            switch error {
            case .unsupported(let m): await request.respondError(400, m)
            case .inProgress: await request.respondError(409, "A model download is already in progress")
            }
        }
    }

    // MARK: Transcription

    struct RequestSettings {
        var language: String
    }

    struct RequestError: Error {
        var message: String
    }

    /// The `x-multivoice-*` headers. The served model is host configuration, so
    /// `x-multivoice-model` is accepted and ignored.
    func settings(_ head: HTTPRequestHead) -> Result<RequestSettings, RequestError> {
        let backend = head.header("x-multivoice-backend") ?? HostRuntime.backendID
        guard backend == "parakeet" || backend == "whisper" else {
            return .failure(RequestError(message: "Unsupported transcription backend: \(backend)"))
        }
        return .success(RequestSettings(language: head.header("x-multivoice-language") ?? "en"))
    }

    private func respond(_ request: HTTPServerRequest, _ outcome: TranscriptionOutcome) async {
        await request.respondJSON(200, .object([
            ("text", .string(outcome.text)), ("durationSeconds", .double(outcome.durationSeconds)),
            ("backend", .string(outcome.backend)), ("model", .string(outcome.model)),
            ("serverVersion", .string(runtime.serverVersion)),
        ]))
    }

    private func respond(_ request: HTTPServerRequest, _ error: HostRuntime.RuntimeError) async {
        switch error {
        case .queueFull(let m): await request.respondError(429, m)
        case .workerFailed(let m): await request.respondError(500, m)
        }
    }

    private func batch(_ request: HTTPServerRequest) async {
        let client = Self.clientAddress(peer: request.peerAddress, head: request.head)
        runtime.metrics.clientRequest(client)
        let limit = Self.maxBatchHeaderBytes + Self.maxBatchBytesPerSecond * runtime.liveSettings.maxRecordingSeconds
        let body: [UInt8]
        do { body = try await request.readBody(limit: limit) } catch {
            runtime.metrics.rejectJob(client: client)
            await request.respondError(413, error is HTTPServerRequest.BodyError
                ? "Transcription request body exceeds host maximum upload size"
                : "Failed to read transcription request body: \(error.localizedDescription)")
            return
        }
        let settings: RequestSettings
        switch self.settings(request.head) {
        case .success(let s): settings = s
        case .failure(let e):
            await request.respondError(400, e.message)
            return
        }
        let recording: HostRecording
        do { recording = try WAVDecoder.decode(body) } catch {
            await request.respondError(400, error.message)
            return
        }
        if let message = runtime.validateDuration(recording.durationSeconds) {
            runtime.metrics.rejectJob(client: client)
            await request.respondError(413, message)
            return
        }
        switch runtime.enqueue(.recording(recording), language: settings.language, source: "batch", client: client,
                               audioSeconds: recording.durationSeconds) {
        case .failure(let error):
            await respond(request, error)
        case .success(let result):
            switch await result.wait() {
            case .success(let outcome): await respond(request, outcome)
            case .failure(let error): await respond(request, error)
            }
        }
    }

    private func stream(_ request: HTTPServerRequest) async {
        let client = Self.clientAddress(peer: request.peerAddress, head: request.head)
        runtime.metrics.clientRequest(client)
        let settings: RequestSettings
        switch self.settings(request.head) {
        case .success(let s): settings = s
        case .failure(let e):
            await request.respondError(400, e.message)
            return
        }
        guard let streamID = runtime.metrics.tryBeginStream(client: client, maxActiveStreams: runtime.liveSettings.maxActiveStreams) else {
            await request.respondError(429, "Server is at active stream capacity")
            return
        }
        // Queue the job before reading the body so a worker decodes while the client speaks.
        let feed = StreamFeed()
        let result: JobResult
        switch runtime.enqueue(.stream(feed), language: settings.language, source: "stream", client: client, audioSeconds: 0) {
        case .failure(let error):
            await respond(request, error)
            runtime.metrics.finishStream(streamID, audioSeconds: 0)
            return
        case .success(let r):
            result = r
        }

        let maxSeconds = runtime.liveSettings.maxRecordingSeconds
        var reader = StreamFrameReader()
        var samples = 0
        var rate: Int?
        var failure: String?
        reading: do {
            while let chunk = try await request.readBodyChunk() {
                reader.feed(chunk)
                while let frame = try reader.next() {
                    if frame.samples.isEmpty { continue }
                    if let r = rate, r != frame.sampleRate {
                        failure = "Remote stream sample rate changed during recording"
                        break reading
                    }
                    rate = frame.sampleRate
                    samples += frame.samples.count
                    if samples > frame.sampleRate * maxSeconds {
                        failure = "Recording exceeds host maximum of \(maxSeconds) seconds"
                        break reading
                    }
                    feed.send(.frame(frame.samples, sampleRate: frame.sampleRate))
                }
            }
            try reader.finish()
            if samples == 0 || rate == nil { failure = "No audio samples were captured" }
        } catch let error as StreamFrameReader.ReadError {
            failure = error == .truncated ? "Remote stream ended mid-frame" : error.message
        } catch {
            failure = "Failed to read remote stream: \(error.localizedDescription)"
        }

        if let failure {
            feed.send(.abort)
            feed.finish()
            runtime.metrics.rejectJob(client: client)
            await request.respondError(failure == "No audio samples were captured" ? 400 : 413, failure)
            runtime.metrics.finishStream(streamID, audioSeconds: 0)
            return
        }
        let audioSeconds = Double(samples) / Double(rate ?? 16_000)
        // The stream's end is published before the worker can finish, so `stream_finished`
        // always precedes the job's terminal event.
        runtime.metrics.finishStream(streamID, audioSeconds: audioSeconds)
        feed.finish()
        switch await result.wait() {
        case .success(let outcome): await respond(request, outcome)
        case .failure(let error): await respond(request, error)
        }
    }
}
