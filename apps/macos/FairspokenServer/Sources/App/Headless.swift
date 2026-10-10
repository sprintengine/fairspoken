import FairspokenHost
import FairspokenSpeech
import Foundation
import FairspokenCore

/// `Fairspoken Server --headless`: the same host with no window, menu bar item or Dock icon,
/// for a LaunchAgent (or a LaunchDaemon). Logs one line per job to standard output, never
/// transcript text. Stops cleanly on SIGTERM / SIGINT (what `launchctl` sends).
nonisolated enum HeadlessServer {
    static func run() async -> Int32 {
        let backend = FluidAudioBackend()
        let known = Set(backend.catalog.map(\.id))
        let url = ServerInfo.configURL
        let config: HostConfiguration
        do {
            let file = try HostConfigurationStore.load(url)
            var resolved = try HostConfigurationStore.resolve(file: file, environment: ServerInfo.environment, knownModels: known)
            if file == nil {
                if resolved.token.isEmpty && HostEnvironment.value("TOKEN", in: ServerInfo.environment) == nil {
                    resolved.token = HostConfiguration.generateToken()
                }
                var toSave = resolved
                // Only persist what the environment didn't force for this run.
                if HostEnvironment.value("TOKEN", in: ServerInfo.environment) != nil { toSave.token = "" }
                if HostEnvironment.value("PAIRING_PASSWORD", in: ServerInfo.environment)?.isEmpty == false { toSave.pairingPassword = "" }
                if HostEnvironment.value("NAME", in: ServerInfo.environment) != nil { toSave.displayName = "" }
                if ServerInfo.environment[HostEnvironment.modelLinksVariable]?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false {
                    toSave.modelLinks = [:]
                }
                try HostConfigurationStore.save(toSave, to: url)
                say("Created \(url.path)\(toSave.token.isEmpty ? "" : " with a new token (shown in the app under Connect)")")
            }
            config = resolved
            ParakeetModels.setLinks(config.modelLinks)
        } catch {
            say("error: \(error.localizedDescription)")
            return 78 // EX_CONFIG
        }
        let host: TranscriptionHost
        do {
            host = try await TranscriptionHost.start(configuration: config, configURL: url, backend: backend,
                                                     dashboardHTML: ServerInfo.dashboardHTML, serverVersion: ServerInfo.version)
        } catch {
            say("error: \(error.localizedDescription)")
            return 69 // EX_UNAVAILABLE
        }
        say("\(ServerInfo.displayName) \(ServerInfo.version) listening on http://\(config.bindAddress):\(host.boundPort)"
            + " · \(config.workerCount) worker(s) · \(config.workerModels.joined(separator: ", "))"
            + (host.runtime.authToken == nil ? " · no token (every client is allowed)" : " · token required")
            + " · \"\(host.runtime.displayName)\" pairing " + (host.runtime.pairingEnabled ? "on" : "off"))
        let sleepGuard = SleepGuard()
        if config.preventSleep { sleepGuard.hold(reason: "\(ServerInfo.displayName) is serving transcription requests") }

        let logger = Task.detached { await logEvents(host.runtime.observe().frames) }
        await waitForTermination()
        say("Stopping")
        logger.cancel()
        await host.stop()
        sleepGuard.release()
        return 0
    }

    static func say(_ line: String) {
        let stamp = Date().formatted(.iso8601.dateSeparator(.dash).timeSeparator(.colon))
        FileHandle.standardOutput.write(Data("\(stamp) \(line)\n".utf8))
    }

    /// One line per finished job and worker change. Never transcript text (events carry none).
    static func logEvents(_ frames: AsyncStream<String>) async {
        var parser = SSEParser()
        for await frame in frames {
            for case .event(let e) in parser.feed(frame) {
                if e.type == "pairing" {
                    // Carries no password; the address and the name the client gave.
                    let data = (try? JSONSerialization.jsonObject(with: Data(e.data.utf8))) as? [String: Any] ?? [:]
                    let who = [data["clientName"] as? String, data["client"] as? String].compactMap { $0 }.joined(separator: " at ")
                    say("pairing from \(who.isEmpty ? "unknown client" : who): \(data["ok"] as? Bool == true ? "paired" : "refused")")
                    continue
                }
                guard let event = try? HostEvent.decode(e) else { continue }
                switch event {
                case .jobCompleted(let j):
                    say(String(format: "job %@ done on worker %d: %.1f s of audio from %@, %.0f ms", j.jobId, j.worker + 1, j.audioSeconds,
                               j.client ?? "unknown client", j.processingMs))
                case .jobFailed(let j):
                    say("job \(j.jobId) failed\(j.worker.map { " on worker \($0 + 1)" } ?? ""): \(j.error)")
                case .workerState(let w):
                    say("worker \(w.worker + 1): \(w.state)\(w.model.map { " (\($0))" } ?? "")")
                case .modelDownload(let d) where d.stage == "starting":
                    // Host and path only: a link's query string may carry a token.
                    say("downloading \(d.model)" + (ParakeetModels.link(for: d.model).map { " from \(ModelLink.shortDisplay($0))" } ?? ""))
                case .modelDownload(let d) where d.stage == "ready" || d.stage == "error":
                    say("model \(d.model): \(d.stage)\(d.error.map { " – \($0)" } ?? "")")
                default:
                    break
                }
            }
        }
    }

    static func waitForTermination() async {
        signal(SIGTERM, SIG_IGN)
        signal(SIGINT, SIG_IGN)
        let stream = AsyncStream<Void> { continuation in
            let sources = [SIGTERM, SIGINT].map { sig -> DispatchSourceSignal in
                let source = DispatchSource.makeSignalSource(signal: sig, queue: .global())
                source.setEventHandler { continuation.yield() }
                source.resume()
                return source
            }
            nonisolated(unsafe) let keep = sources
            continuation.onTermination = { _ in keep.forEach { $0.cancel() } }
        }
        for await _ in stream { return }
    }
}
