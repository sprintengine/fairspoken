import Foundation

public enum TranscriptionLocation: String, Codable, Sendable, CaseIterable {
    case local
    case remoteHost = "remote-host"
    case cloud
}

public enum RecordingShortcutMode: String, Codable, Sendable, CaseIterable {
    case toggle
    case pushToTalk = "push-to-talk"
}

public struct TranscriptCorrection: Codable, Equatable, Sendable {
    public var enabled = true
    public var from = ""
    public var to = ""
    public var caseSensitive = false
    public var wholePhrase = true
    public init() {}
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = c.lenient(Bool.self, .enabled) ?? true
        from = c.lenient(String.self, .from) ?? ""
        to = c.lenient(String.self, .to) ?? ""
        caseSensitive = c.lenient(Bool.self, .caseSensitive) ?? false
        wholePhrase = c.lenient(Bool.self, .wholePhrase) ?? true
    }
}

public struct Snippet: Codable, Equatable, Sendable {
    public var enabled = true
    public var trigger = ""
    public var expansion = ""
    public init() {}
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = c.lenient(Bool.self, .enabled) ?? true
        trigger = c.lenient(String.self, .trigger) ?? ""
        expansion = c.lenient(String.self, .expansion) ?? ""
    }
}

/// `settings.json`, field-for-field compatible with `Settings` in `src-tauri/src/settings.rs`
/// (same camelCase names, kebab-case enums, defaults and clamps). Mac-only additions are
/// extra keys the Rust app ignores. Secrets are kept out of the file (Keychain); the JSON
/// keeps empty strings exactly as the plan's §2.8 describes.
public struct AppSettings: Codable, Equatable, Sendable {
    public var transcriptionLocation: TranscriptionLocation = .local
    public var model = SpeechModelCatalog.defaultModelID
    public var remoteUrl = ""
    public var remoteAuthToken = ""
    public var remoteTimeoutSeconds = 60
    public var cloudAuthToken = ""
    public var language = "en"
    public var audioDevice = ""
    public var noiseSuppression = true
    public var echoCancellation = true
    public var inputGain = 2
    public var postProcess = true
    public var vocabularyHints: [String] = []
    public var transcriptCorrections: [TranscriptCorrection] = []
    public var snippets: [Snippet] = []
    public var alwaysOnTop = true
    public var interactionSounds = true
    public var maxRecordingSeconds = 120
    public var noteRetentionMinutes = 0
    public var useGpu = true
    public var recordingShortcut = AcceleratorString.defaultRecording
    public var recordingShortcutMode: RecordingShortcutMode = .toggle
    public var transcriptStackShortcut = AcceleratorString.defaultTranscriptStack
    public var insertAtCursor = true
    public var accessibilityInsert = false
    public var fnPushToTalk = false
    public var polishEnabled = false
    public var polishProvider = "cloud"
    public var polishModel = "speakoflow-mini"
    public var polishTones: [String: String] = [:]
    public var contextAwareness = false

    // Mac-only (ignored by the Rust app).
    /// Put the previous clipboard back after pasting the dictation.
    public var restoreClipboard = true

    public init() {}

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = AppSettings()
        transcriptionLocation = c.lenient(TranscriptionLocation.self, .transcriptionLocation) ?? d.transcriptionLocation
        model = c.lenient(String.self, .model) ?? d.model
        remoteUrl = c.lenient(String.self, .remoteUrl) ?? d.remoteUrl
        remoteAuthToken = c.lenient(String.self, .remoteAuthToken) ?? d.remoteAuthToken
        remoteTimeoutSeconds = c.lenientInt(.remoteTimeoutSeconds) ?? d.remoteTimeoutSeconds
        cloudAuthToken = c.lenient(String.self, .cloudAuthToken) ?? d.cloudAuthToken
        language = c.lenient(String.self, .language) ?? d.language
        audioDevice = c.lenient(String.self, .audioDevice) ?? d.audioDevice
        noiseSuppression = c.lenient(Bool.self, .noiseSuppression) ?? d.noiseSuppression
        echoCancellation = c.lenient(Bool.self, .echoCancellation) ?? d.echoCancellation
        inputGain = c.lenientInt(.inputGain) ?? d.inputGain
        postProcess = c.lenient(Bool.self, .postProcess) ?? d.postProcess
        vocabularyHints = c.lenient([String].self, .vocabularyHints) ?? d.vocabularyHints
        transcriptCorrections = c.lenient([TranscriptCorrection].self, .transcriptCorrections) ?? d.transcriptCorrections
        snippets = c.lenient([Snippet].self, .snippets) ?? d.snippets
        alwaysOnTop = c.lenient(Bool.self, .alwaysOnTop) ?? d.alwaysOnTop
        interactionSounds = c.lenient(Bool.self, .interactionSounds) ?? d.interactionSounds
        maxRecordingSeconds = c.lenientInt(.maxRecordingSeconds) ?? d.maxRecordingSeconds
        noteRetentionMinutes = c.lenientInt(.noteRetentionMinutes) ?? d.noteRetentionMinutes
        useGpu = c.lenient(Bool.self, .useGpu) ?? d.useGpu
        recordingShortcut = c.lenient(String.self, .recordingShortcut) ?? d.recordingShortcut
        recordingShortcutMode = c.lenient(RecordingShortcutMode.self, .recordingShortcutMode) ?? d.recordingShortcutMode
        transcriptStackShortcut = c.lenient(String.self, .transcriptStackShortcut) ?? d.transcriptStackShortcut
        insertAtCursor = c.lenient(Bool.self, .insertAtCursor) ?? d.insertAtCursor
        accessibilityInsert = c.lenient(Bool.self, .accessibilityInsert) ?? d.accessibilityInsert
        fnPushToTalk = c.lenient(Bool.self, .fnPushToTalk) ?? d.fnPushToTalk
        polishEnabled = c.lenient(Bool.self, .polishEnabled) ?? d.polishEnabled
        polishProvider = c.lenient(String.self, .polishProvider) ?? d.polishProvider
        polishModel = c.lenient(String.self, .polishModel) ?? d.polishModel
        polishTones = c.lenient([String: String].self, .polishTones) ?? d.polishTones
        contextAwareness = c.lenient(Bool.self, .contextAwareness) ?? d.contextAwareness
        restoreClipboard = c.lenient(Bool.self, .restoreClipboard) ?? d.restoreClipboard
        self = normalized()
    }

    /// Same clamps and clean-ups as `normalize` in settings.rs.
    public func normalized() -> AppSettings {
        var s = self
        s.inputGain = min(max(s.inputGain, 1), 6)
        s.maxRecordingSeconds = min(max(s.maxRecordingSeconds, 10), 600)
        s.remoteTimeoutSeconds = min(max(s.remoteTimeoutSeconds, 5), 300)
        s.remoteUrl = s.remoteUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        while s.remoteUrl.hasSuffix("/") { s.remoteUrl.removeLast() }
        s.cloudAuthToken = s.cloudAuthToken.trimmingCharacters(in: .whitespacesAndNewlines)
        s.vocabularyHints = Self.normalizeVocabulary(s.vocabularyHints)
        if s.recordingShortcut.trimmingCharacters(in: .whitespaces).isEmpty { s.recordingShortcut = AcceleratorString.defaultRecording }
        if s.transcriptStackShortcut.trimmingCharacters(in: .whitespaces).isEmpty { s.transcriptStackShortcut = AcceleratorString.defaultTranscriptStack }
        // Unknown ids (e.g. Whisper models from the Tauri app) fall back to the default, as in Rust.
        if SpeechModelCatalog.model(id: s.model) == nil { s.model = SpeechModelCatalog.defaultModelID }
        return s
    }

    /// Trim, collapse whitespace, cap at 100 characters, de-duplicate, keep at most 50.
    public static func normalizeVocabulary(_ hints: [String]) -> [String] {
        var out: [String] = []
        for hint in hints {
            let collapsed = hint.split(whereSeparator: \.isWhitespace).joined(separator: " ")
            let clean = String(collapsed.prefix(100))
            guard !clean.isEmpty, !out.contains(clean) else { continue }
            out.append(clean)
            if out.count >= 50 { break }
        }
        return out
    }
}
