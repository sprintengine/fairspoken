import FairspokenSpeech
import AVFoundation
import Foundation
import FairspokenCore

/// Headless end-to-end check of the real pipeline without the microphone or TCC:
///
///   Fairspoken --selftest <file.wav> [--model parakeet-ultra] [--host URL --token T]
///
/// 1. Loads the model on the Neural Engine (timing), transcribes the file through the same
///    `LocalBatchSession` + post-processor the hotkey path uses.
/// 2. With `--host`: streams the file through `RemoteStreamSession` (framed PCM16, chunked,
///    paced at 4× real time) while subscribed to `/v1/events`, and reports both.
@MainActor
enum SelfTest {
    static func runIfRequested() -> Bool {
        let args = AppInfo.arguments
        guard let i = args.firstIndex(of: "--selftest"), i + 1 < args.count else { return false }
        let file = URL(fileURLWithPath: args[i + 1])
        func value(_ flag: String) -> String? {
            args.firstIndex(of: flag).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil }
        }
        Task {
            await run(file: file, model: value("--model") ?? SpeechModelCatalog.defaultModelID,
                      host: value("--host"), token: value("--token") ?? "")
            exit(0)
        }
        return true
    }

    private static func say(_ s: String) { FileHandle.standardOutput.write(Data((s + "\n").utf8)) }

    private static func run(file: URL, model: String, host: String?, token: String) async {
        guard let samples = try? load16k(file) else { say("FAIL: cannot read \(file.path)"); return }
        let seconds = Double(samples.count) / 16_000
        say(String(format: "audio: %@ (%.2f s)", file.lastPathComponent, seconds))
        let gate = RecordingGate.evaluate(AudioStats(samples: samples, sampleRate: 16_000))
        say("gate: \(gate)")

        // Local, Neural Engine.
        let engine = FluidAudioEngine()
        let t0 = ContinuousClock.now
        do {
            try await engine.load(model) { stage in
                if case .downloading(let f) = stage, Int(f * 100) % 10 == 0 { print("download \(Int(f * 100))%") }
            }
            let loadMs = (ContinuousClock.now - t0) / .milliseconds(1)
            say(String(format: "local: loaded + warmed %@ in %.0f ms", model, loadMs))
            for run in 1...3 {
                let session = LocalBatchSession(engine: engine, options: .init())
                let t = ContinuousClock.now
                let out = try await session.finish(allSamples: samples)
                let text = await BasicPostProcessor().process(out.text, context: .init(appBundleID: nil, vocabulary: ["X-ray", "Parakeet"]))
                let ms = (ContinuousClock.now - t) / .milliseconds(1)
                say(String(format: "local run %d: %.0f ms (model %d ms, %.0f× real time): %@", run, ms, out.modelMs, seconds * 1000 / max(1, ms), text))
            }
        } catch {
            say("FAIL local: \(error.localizedDescription)")
        }

        // Remote host, streamed, with the SSE feed watched alongside.
        guard let host else { return }
        do {
            let base = try RemoteURLPolicy.validateBaseURL(host)
            let health = try await RemoteHostClient.health(baseURL: host, token: token)
            say("remote: health ok=\(health.ok) backend=\(health.backend) version=\(health.serverVersion ?? "?")")
            let events = Task.detached { await watchEvents(base: base, token: token, seconds: 3 + seconds / 4 + 6) }
            try? await Task.sleep(for: .milliseconds(400))
            let session = RemoteStreamSession(base: base, token: token)
            let chunk = 1_600
            var i = 0
            let t = ContinuousClock.now
            while i < samples.count {
                session.append(Array(samples[i..<min(i + chunk, samples.count)]))
                i += chunk
                try? await Task.sleep(for: .milliseconds(25)) // 100 ms of audio every 25 ms = 4× real time
            }
            let released = ContinuousClock.now
            let out = try await session.finish(allSamples: samples)
            say(String(format: "remote: streamed in %.0f ms, release → text %.0f ms, model=%@: %@",
                       (released - t) / .milliseconds(1), (ContinuousClock.now - released) / .milliseconds(1), out.model, out.text))
            let seen = await events.value
            say("events: " + seen.joined(separator: ", "))
        } catch {
            say("FAIL remote: \(error.localizedDescription)")
        }
    }

    nonisolated private static func watchEvents(base: URL, token: String, seconds: Double) async -> [String] {
        var request = URLRequest(url: RemoteURLPolicy.endpoint(base, RemoteProtocol.eventsPath))
        for (k, v) in RemoteProtocol.authHeaders(token: token) { request.setValue(v, forHTTPHeaderField: k) }
        var seen: [String] = []
        let deadline = ContinuousClock.now + .seconds(seconds)
        do {
            let (bytes, response) = try await URLSession.shared.bytes(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            guard status == 200 else { return ["HTTP \(status) (would fall back to polling)"] }
            var parser = SSEParser()
            var state = HostLiveState()
            for try await byte in bytes {
                if ContinuousClock.now > deadline { break }
                guard case .event(let e)? = parser.push(byte) else { continue }
                let event = try HostEvent.decode(e)
                state.apply(event)
                seen.append(e.type)
                if case .jobCompleted = event { break }
            }
            seen.append("state: \(state.workers.count) workers, \(state.models.count) models, served \(state.totalTranscriptions)")
        } catch {
            seen.append("error: \(error.localizedDescription)")
        }
        return seen
    }

    private static func load16k(_ url: URL) throws -> [Float] {
        let file = try AVAudioFile(forReading: url)
        let target = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!
        let input = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: AVAudioFrameCount(file.length))!
        try file.read(into: input)
        guard let converter = AVAudioConverter(from: file.processingFormat, to: target) else { return [] }
        let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: AVAudioFrameCount(Double(file.length) * 16_000 / file.processingFormat.sampleRate + 1024))!
        let feed = OneShot(input)
        _ = converter.convert(to: out, error: nil) { _, status in
            guard let buffer = feed.take() else { status.pointee = .endOfStream; return nil }
            status.pointee = .haveData
            return buffer
        }
        return Array(UnsafeBufferPointer(start: out.floatChannelData![0], count: Int(out.frameLength)))
    }
}

/// Converter input handed over once (the converter calls back synchronously).
nonisolated private final class OneShot: @unchecked Sendable {
    private var buffer: AVAudioPCMBuffer?
    init(_ buffer: AVAudioPCMBuffer) { self.buffer = buffer }
    func take() -> AVAudioPCMBuffer? { defer { buffer = nil }; return buffer }
}

private extension RemoteStreamSession {
    convenience init(base: URL, token: String) {
        self.init(baseURL: base, options: .init(token: token, model: SpeechModelCatalog.defaultModelID, language: "en",
                                                vocabularyHints: ["X-ray"]), timeoutSeconds: 30)
    }
}
