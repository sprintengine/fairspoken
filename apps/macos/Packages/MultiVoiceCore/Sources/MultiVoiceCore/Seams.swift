import Foundation

// Protocol seams. The app, the future in-app host and tests depend on these, never on
// FluidAudio or URLSession types directly (plan §2.4). Phase 1/2 features plug in here.

public struct TranscriptionOutput: Sendable, Equatable {
    public var text: String
    /// Time the speech model itself spent (ms); for remote hosts the round trip.
    public var modelMs: Int
    public var model: String
    public var backend: String
    public var placement: ComputePlacement

    public init(text: String, modelMs: Int, model: String, backend: String = RemoteProtocol.backendID, placement: ComputePlacement) {
        self.text = text
        self.modelMs = modelMs
        self.model = model
        self.backend = backend
        self.placement = placement
    }
}

public struct TranscriptionRequestOptions: Sendable, Equatable {
    public var language: String
    public var vocabularyHints: [String]
    public init(language: String = "en", vocabularyHints: [String] = []) {
        self.language = language
        self.vocabularyHints = vocabularyHints
    }
}

/// A local speech engine that transcribes whole 16 kHz mono recordings.
public protocol SpeechEngine: Sendable {
    func transcribe(_ samples16k: [Float], options: TranscriptionRequestOptions) async throws -> TranscriptionOutput
}

/// One dictation's worth of transcription. Local sessions buffer and transcribe on
/// `finish`; remote sessions stream frames to the host while the user speaks.
/// Phase 1 adds a chunked local session (live preview) behind the same protocol.
public protocol TranscriptionSession: AnyObject, Sendable {
    /// Called from the audio thread with 16 kHz mono samples; must not block.
    func append(_ samples16k: [Float])
    func finish(allSamples: [Float]) async throws -> TranscriptionOutput
    func cancel()
}

/// Phase 1 seam: deterministic tidy, corrections/snippets and LLM polish run here.
public protocol TranscriptPostProcessor: Sendable {
    func process(_ text: String, context: PostProcessContext) async -> String
}

public struct PostProcessContext: Sendable {
    public var appBundleID: String?
    public var vocabulary: [String]
    public init(appBundleID: String?, vocabulary: [String]) {
        self.appBundleID = appBundleID
        self.vocabulary = vocabulary
    }
}

/// Phase 0 behaviour: trim, then restore the user's casing for vocabulary terms.
/// Phase 1 chains tidy, corrections/snippets and polish in front of this.
public struct BasicPostProcessor: TranscriptPostProcessor {
    public init() {}
    public func process(_ text: String, context: PostProcessContext) async -> String {
        VocabularyCasing.apply(text.trimmingCharacters(in: .whitespacesAndNewlines), vocabulary: context.vocabulary)
    }
}

/// Phase 2 seam: the in-app host ("Serve this Mac"). Its event stream feeds the same
/// `HostLiveState` the Server view uses for remote hosts.
public protocol HostServing: AnyObject, Sendable {
    var isRunning: Bool { get }
    func start(port: Int, token: String) async throws
    func stop() async
    func events() -> AsyncStream<HostEvent>
}

/// Deterministic engine for tests and screenshots (plan's `StubEngine`).
public struct StubSpeechEngine: SpeechEngine {
    public var text: String
    public init(text: String = "The quick brown fox jumps over the lazy dog.") { self.text = text }
    public func transcribe(_ samples16k: [Float], options: TranscriptionRequestOptions) async throws -> TranscriptionOutput {
        TranscriptionOutput(text: text, modelMs: samples16k.count / 1600, model: "stub", placement: .cpu)
    }
}
