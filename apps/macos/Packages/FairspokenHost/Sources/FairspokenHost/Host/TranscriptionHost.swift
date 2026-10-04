import Foundation

/// A running transcription host: runtime + router + TCP listener.
public final class TranscriptionHost: Sendable {
    public let runtime: HostRuntime
    public let router: HostRouter
    private let listener: HTTPListener

    /// Binds the listener and starts the workers (which preload their models).
    public static func start(configuration: HostConfiguration, configURL: URL?, backend: any HostSpeechBackend,
                             dashboardHTML: [UInt8], serverVersion: String, heartbeat: Duration = .seconds(15)) async throws -> TranscriptionHost {
        let runtime = HostRuntime(configuration: configuration, configURL: configURL, backend: backend, serverVersion: serverVersion)
        let router = HostRouter(runtime: runtime, dashboardHTML: dashboardHTML, heartbeat: heartbeat)
        let listener = try HTTPListener(host: runtime.configuration.bindAddress, port: UInt16(runtime.configuration.port)) { request in
            await router.handle(request)
        }
        try await listener.start()
        runtime.startWorkers()
        return TranscriptionHost(runtime: runtime, router: router, listener: listener)
    }

    private init(runtime: HostRuntime, router: HostRouter, listener: HTTPListener) {
        self.runtime = runtime
        self.router = router
        self.listener = listener
    }

    public var boundPort: UInt16 { listener.boundPort }

    public func stop() async {
        listener.stop()
        await runtime.stop()
    }
}
