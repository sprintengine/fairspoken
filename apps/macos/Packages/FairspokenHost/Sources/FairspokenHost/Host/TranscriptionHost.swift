import Foundation

/// A running transcription host: runtime + router + TCP listener (plus a loopback companion
/// when the host listens on one non-loopback address, such as a tailnet IP).
public final class TranscriptionHost: Sendable {
    public let runtime: HostRuntime
    public let router: HostRouter
    private let listener: HTTPListener
    private let loopback: HTTPListener?

    /// Binds the listener and starts the workers (which preload their models).
    public static func start(configuration: HostConfiguration, configURL: URL?, backend: any HostSpeechBackend,
                             dashboardHTML: [UInt8], serverVersion: String, heartbeat: Duration = .seconds(15),
                             limits: HTTPServerLimits = HTTPServerLimits()) async throws -> TranscriptionHost {
        let runtime = HostRuntime(configuration: configuration, configURL: configURL, backend: backend, serverVersion: serverVersion)
        let router = HostRouter(runtime: runtime, dashboardHTML: dashboardHTML, heartbeat: heartbeat)
        let listener = try HTTPListener(host: runtime.configuration.bindAddress, port: UInt16(runtime.configuration.port),
                                        limits: limits) { request in
            await router.handle(request)
        }
        try await listener.start()
        let loopback = await startLoopbackCompanion(for: runtime.configuration.bindAddress, port: listener.boundPort,
                                                    limits: limits) { request in
            await router.handle(request)
        }
        runtime.startWorkers()
        return TranscriptionHost(runtime: runtime, router: router, listener: listener, loopback: loopback)
    }

    /// A host bound to one non-loopback address (a tailnet IP) also answers on `127.0.0.1` at the
    /// same port, so apps on this Mac reach it whichever address they saved. Best effort: if the
    /// loopback port is taken the host still serves its main address.
    static func startLoopbackCompanion(for bindAddress: String, port: UInt16, limits: HTTPServerLimits,
                                       handler: @escaping @Sendable (HTTPServerRequest) async -> Void) async -> HTTPListener? {
        guard HostConfiguration.needsLoopbackCompanion(bindAddress),
              let companion = try? HTTPListener(host: "127.0.0.1", port: port, limits: limits, handler: handler) else { return nil }
        do {
            try await companion.start()
            return companion
        } catch {
            companion.stop()
            return nil
        }
    }

    private init(runtime: HostRuntime, router: HostRouter, listener: HTTPListener, loopback: HTTPListener?) {
        self.runtime = runtime
        self.router = router
        self.listener = listener
        self.loopback = loopback
    }

    public var boundPort: UInt16 { listener.boundPort }

    /// Whether this Mac's own apps can also reach the host on `127.0.0.1`.
    public var answersOnLoopback: Bool {
        loopback != nil || !HostConfiguration.needsLoopbackCompanion(runtime.configuration.bindAddress)
    }

    public func stop() async {
        listener.stop()
        loopback?.stop()
        await runtime.stop()
    }
}
