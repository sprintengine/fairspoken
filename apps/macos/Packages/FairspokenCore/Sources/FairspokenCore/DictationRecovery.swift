import Foundation

/// The ways to transcribe a kept recording, in the order they are tried.
public struct TranscriptionRoutes: Sendable {
    /// Sends the whole recording (a WAV file's bytes) to the host, within the timeout.
    /// Nil when no host is in use.
    public var remote: (@Sendable (_ wav: Data, _ timeout: Duration) async throws -> TranscriptionOutput)?
    /// The on-device engine. Nil when no model is installed.
    public var local: (@Sendable (_ samples16k: [Float]) async throws -> TranscriptionOutput)?

    public init(remote: (@Sendable (Data, Duration) async throws -> TranscriptionOutput)? = nil,
                local: (@Sendable ([Float]) async throws -> TranscriptionOutput)? = nil) {
        self.remote = remote
        self.local = local
    }
}

/// How hard to try before keeping a dictation for later.
public struct DictationRetryPolicy: Sendable, Equatable {
    /// The wait before each re-send to the host.
    public var delays: [Duration]
    /// The longest all the re-sends may take together before falling back to this Mac.
    public var budget: Duration
    /// The longest one re-send may take.
    public var attemptTimeout: Duration

    public init(delays: [Duration], budget: Duration, attemptTimeout: Duration) {
        self.delays = delays
        self.budget = budget
        self.attemptTimeout = attemptTimeout
    }

    /// Right after the live attempt failed, while the user waits: two quick re-sends (a host
    /// that is restarting or briefly busy answers within seconds), then this Mac.
    public static func live(timeoutSeconds: Int) -> DictationRetryPolicy {
        DictationRetryPolicy(delays: [.seconds(1), .seconds(3)], budget: .seconds(30), attemptTimeout: .seconds(max(5, timeoutSeconds)))
    }

    /// A kept dictation, retried in the background or with the Retry button: one re-send.
    public static func later(timeoutSeconds: Int) -> DictationRetryPolicy {
        DictationRetryPolicy(delays: [.zero], budget: .seconds(max(5, timeoutSeconds)), attemptTimeout: .seconds(max(5, timeoutSeconds)))
    }

    /// When a kept dictation is retried on its own, after `attempts` failures: 30 s, 1, 2, 5, 10,
    /// then every 30 minutes.
    public static func backoff(afterAttempts attempts: Int) -> Duration {
        let steps: [Duration] = [.seconds(30), .seconds(60), .seconds(120), .seconds(300), .seconds(600), .seconds(1800)]
        return steps[min(max(0, attempts - 1), steps.count - 1)]
    }

    public static func isDue(_ item: PendingDictation, now: Date = Date()) -> Bool {
        guard item.state == .failed else { return false }
        guard let last = item.lastAttemptDate else { return true }
        return now.timeIntervalSince(last) >= Double(backoff(afterAttempts: item.attempts).components.seconds)
    }

    /// Whether sending the same audio again could succeed.
    public static func isRetryable(_ error: Error) -> Bool {
        switch error {
        case let error as RemoteTranscriptionError: error.isRetryable
        case is RemoteURLPolicy.ValidationError: false
        default: true
        }
    }
}

/// What became of a dictation.
public enum DictationOutcome: Sendable, Equatable {
    /// Transcribed and delivered; the audio is deleted.
    case delivered(TranscriptionOutput)
    /// The host or the engine heard no speech; the audio is deleted, as for a silent recording.
    case noSpeech
    /// Every attempt failed; the audio is kept for a later retry.
    case kept(PendingDictation)
}

/// Gets a kept recording transcribed without ever losing it: the live attempt (the stream that
/// ran while the user spoke), then re-sends of the whole recording from disk, then the
/// on-device engine. The audio is deleted only after `deliver` has the transcript; if
/// everything fails it stays on disk as a failed dictation.
public struct DictationRecovery: Sendable {
    public enum Progress: Sendable, Equatable {
        /// Re-sending to the host, `attempt` of `of`.
        case retrying(attempt: Int, of: Int)
        case transcribingOnThisMac
    }

    public var store: PendingDictationStore
    var sleep: @Sendable (Duration) async throws -> Void

    public init(store: PendingDictationStore, sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) {
        self.store = store
        self.sleep = sleep
    }

    /// `live` is the attempt already under way, nil when retrying a kept dictation.
    public func run(_ id: String,
                    live: (@Sendable () async throws -> TranscriptionOutput)?,
                    routes: TranscriptionRoutes,
                    policy: DictationRetryPolicy,
                    progress: @Sendable (Progress) -> Void = { _ in },
                    deliver: @Sendable (TranscriptionOutput) async -> Void) async -> DictationOutcome {
        store.update(id) { $0.state = .transcribing }
        var failures = 0
        var hostFailure: String?
        var localFailure: String?
        var hostRefused = false

        func finish(_ output: TranscriptionOutput) async -> DictationOutcome {
            guard !output.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                store.remove(id)
                return .noSpeech
            }
            await deliver(output)
            store.remove(id)
            return .delivered(output)
        }

        /// Nil to keep going, an outcome when there is nothing more to try.
        func failed(_ error: Error, onThisMac: Bool = false) -> DictationOutcome? {
            if case .noSpeech? = error as? RemoteTranscriptionError {
                store.remove(id)
                return .noSpeech
            }
            failures += 1
            if onThisMac {
                localFailure = error.localizedDescription
            } else {
                // The first answer is the telling one (later ones tend to be "unreachable").
                hostFailure = hostFailure ?? error.localizedDescription
                if !DictationRetryPolicy.isRetryable(error) { hostRefused = true }
            }
            return nil
        }

        if let live {
            do { return await finish(try await live()) } catch {
                if let outcome = failed(error) { return outcome }
            }
        }

        if let remote = routes.remote, !hostRefused, !Task.isCancelled {
            do {
                let wav = try store.wav(id)
                let deadline = ContinuousClock.now + policy.budget
                for (i, delay) in policy.delays.enumerated() {
                    if delay > .zero { try await sleep(delay) }
                    let left = deadline - ContinuousClock.now
                    guard left > .zero else { break }
                    progress(.retrying(attempt: i + 1, of: policy.delays.count))
                    do { return await finish(try await remote(wav, min(policy.attemptTimeout, left))) } catch {
                        if let outcome = failed(error) { return outcome }
                        if hostRefused { break }
                    }
                }
            } catch is CancellationError {
                // Fall through: the recording is kept below.
            } catch {
                _ = failed(error)
            }
        }

        if let local = routes.local, !Task.isCancelled {
            progress(.transcribingOnThisMac)
            do { return await finish(try await local(try store.samples(id))) } catch {
                if let outcome = failed(error, onThisMac: true) { return outcome }
            }
        }

        // Nothing worked: keep it, saying what the host and this Mac answered.
        let message = [hostFailure, localFailure.map { "On this Mac: \($0)" }].compactMap(\.self).joined(separator: " · ")
        store.update(id) {
            $0.state = .failed
            $0.attempts += max(1, failures)
            $0.lastError = message.isEmpty ? "Not transcribed yet" : message
            $0.lastAttemptAt = Int64(Date().timeIntervalSince1970 * 1000)
        }
        return .kept(store.item(id) ?? PendingDictation(id: id, createdAt: Date()))
    }
}
