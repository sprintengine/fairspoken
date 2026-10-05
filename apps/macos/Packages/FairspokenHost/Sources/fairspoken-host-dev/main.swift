// Development host: the real HTTP server, queue and workers over the stub speech engine
// (no models, no Neural Engine). For protocol work and the conformance suite:
//   FAIRSPOKEN_HOST_ADDR=127.0.0.1:48980 FAIRSPOKEN_HOST_TOKEN=secret swift run fairspoken-host-dev
// The real server is `Fairspoken Server.app/Contents/MacOS/Fairspoken Server --headless`.
import FairspokenHost
import Foundation

let env = ProcessInfo.processInfo.environment
let backend = StubSpeechBackend(transcribeDelay: .milliseconds(Int(env["STUB_TRANSCRIBE_MS"] ?? "40") ?? 40),
                                downloadStepDelay: .milliseconds(400))
let configURL = URL(fileURLWithPath: HostEnvironment.value("CONFIG_PATH", in: env) ?? NSTemporaryDirectory() + "fairspoken-host-dev-config.json")
do {
    let file = try HostConfigurationStore.load(configURL)
    let config = try HostConfigurationStore.resolve(file: file, environment: env, knownModels: Set(backend.catalog.map(\.id)))
    let dashboard = env["DASHBOARD_HTML"].flatMap { try? Data(contentsOf: URL(fileURLWithPath: $0)) }.map { [UInt8]($0) } ?? Array("<html></html>".utf8)
    let host = try await TranscriptionHost.start(configuration: config, configURL: configURL, backend: backend, dashboardHTML: dashboard,
                                                 serverVersion: "0.0.0-dev")
    print("Fairspoken development host (stub engine) listening on http://\(config.bindAddress):\(host.boundPort)")
    signal(SIGINT, SIG_IGN)
    signal(SIGTERM, SIG_IGN)
    let stop = AsyncStream<Void> { continuation in
        for sig in [SIGINT, SIGTERM] {
            let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
            source.setEventHandler { continuation.yield() }
            source.resume()
            nonisolated(unsafe) let keep = source
            continuation.onTermination = { _ in keep.cancel() }
        }
    }
    for await _ in stop { break }
    await host.stop()
} catch {
    FileHandle.standardError.write(Data("\(error.localizedDescription)\n".utf8))
    exit(1)
}
