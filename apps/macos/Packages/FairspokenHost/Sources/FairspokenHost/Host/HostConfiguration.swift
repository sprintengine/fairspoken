import Foundation

/// Fairspoken Server's host configuration, persisted as `host-config.json`. The first four
/// fields are the Rust host's `PersistedHostConfig` with the same names and meaning, so either
/// host reads the other's file; the rest are restart-only settings the Rust host takes from
/// `FAIRSPOKEN_HOST_*` environment variables.
public struct HostConfiguration: Codable, Sendable, Equatable {
    public static let defaultModel = "parakeet-tdt-0.6b-v3"
    public static let defaultPort = 48173
    public static let maxActiveStreamsRange = 1...32
    public static let maxRecordingSecondsRange = 10...600
    public static let workerCountRange = 1...8
    public static let queueCapacityRange = 1...64

    public var maxActiveStreams = 4
    public var maxRecordingSeconds = 600
    /// Rust: GPU for the speech model. Here: the Apple Neural Engine (off runs on the CPU).
    public var useGpu = true
    /// Four decoders on one shared model: a dictation holds its worker while the client
    /// speaks, so this is how many people can dictate at once without waiting.
    public var workerModels: [String] = Array(repeating: defaultModel, count: 4)
    public var workerCount = 4
    public var queueCapacity = 8
    /// `127.0.0.1` (this Mac only), `0.0.0.0` (every interface) or one address (a tailnet IP).
    public var bindAddress = "127.0.0.1"
    public var port = defaultPort
    /// Bearer token; empty means no authentication.
    public var token = ""
    /// What clients type to get the token from `POST /v1/pair`; empty turns pairing off.
    /// Never returned by a route, logged or sent in an event.
    public var pairingPassword = ""
    /// The name `/v1/hello` shows scanning clients (`name` in the file, as in the Rust host);
    /// empty uses this Mac's name.
    public var displayName = ""
    /// Hold an IOPM assertion against idle sleep while serving.
    public var preventSleep = true

    public init() {}

    enum CodingKeys: String, CodingKey {
        case maxActiveStreams, maxRecordingSeconds, useGpu, workerModels, workerCount, queueCapacity
        case bindAddress, port, token, pairingPassword, preventSleep
        case displayName = "name"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = HostConfiguration()
        maxActiveStreams = try c.decodeIfPresent(Int.self, forKey: .maxActiveStreams) ?? d.maxActiveStreams
        maxRecordingSeconds = try c.decodeIfPresent(Int.self, forKey: .maxRecordingSeconds) ?? d.maxRecordingSeconds
        useGpu = try c.decodeIfPresent(Bool.self, forKey: .useGpu) ?? d.useGpu
        workerCount = try c.decodeIfPresent(Int.self, forKey: .workerCount) ?? d.workerCount
        workerModels = try c.decodeIfPresent([String].self, forKey: .workerModels) ?? Array(repeating: Self.defaultModel, count: workerCount)
        queueCapacity = try c.decodeIfPresent(Int.self, forKey: .queueCapacity) ?? d.queueCapacity
        bindAddress = try c.decodeIfPresent(String.self, forKey: .bindAddress) ?? d.bindAddress
        port = try c.decodeIfPresent(Int.self, forKey: .port) ?? d.port
        token = try c.decodeIfPresent(String.self, forKey: .token) ?? d.token
        pairingPassword = try c.decodeIfPresent(String.self, forKey: .pairingPassword) ?? d.pairingPassword
        displayName = try c.decodeIfPresent(String.self, forKey: .displayName) ?? d.displayName
        preventSleep = try c.decodeIfPresent(Bool.self, forKey: .preventSleep) ?? d.preventSleep
    }

    /// The optional fields are left out while unset, so a file without pairing reads the same
    /// in a host that validates `pairingPassword` when present.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(maxActiveStreams, forKey: .maxActiveStreams)
        try c.encode(maxRecordingSeconds, forKey: .maxRecordingSeconds)
        try c.encode(useGpu, forKey: .useGpu)
        try c.encode(workerModels, forKey: .workerModels)
        try c.encode(workerCount, forKey: .workerCount)
        try c.encode(queueCapacity, forKey: .queueCapacity)
        try c.encode(bindAddress, forKey: .bindAddress)
        try c.encode(port, forKey: .port)
        try c.encode(token, forKey: .token)
        if !pairingPassword.isEmpty { try c.encode(pairingPassword, forKey: .pairingPassword) }
        if !displayName.isEmpty { try c.encode(displayName, forKey: .displayName) }
        try c.encode(preventSleep, forKey: .preventSleep)
    }

    /// Clamps every number into range and fits the model list to the worker count, the way
    /// the Rust host's `overlay_persisted_config` does (a mismatched list serves its first
    /// model on every worker).
    public func normalized(knownModels: Set<String>) -> HostConfiguration {
        var c = self
        c.maxActiveStreams = c.maxActiveStreams.clamped(to: Self.maxActiveStreamsRange)
        c.maxRecordingSeconds = c.maxRecordingSeconds.clamped(to: Self.maxRecordingSecondsRange)
        c.workerCount = c.workerCount.clamped(to: Self.workerCountRange)
        c.queueCapacity = c.queueCapacity.clamped(to: Self.queueCapacityRange)
        if !(0...65535).contains(c.port) { c.port = Self.defaultPort } // 0 = any free port (tests)
        c.bindAddress = c.bindAddress.trimmingCharacters(in: .whitespaces)
        if c.bindAddress.isEmpty { c.bindAddress = "127.0.0.1" }
        c.displayName = Self.cleanName(c.displayName) ?? ""
        let valid = c.workerModels.map { knownModels.contains($0) ? $0 : Self.defaultModel }
        if valid.count != c.workerCount {
            c.workerModels = Array(repeating: valid.first ?? Self.defaultModel, count: c.workerCount)
        } else {
            c.workerModels = valid
        }
        return c
    }

    /// `host:port` as reported in `/v1/stats` `bindAddr`.
    public var bindAddr: String {
        bindAddress.contains(":") ? "[\(bindAddress)]:\(port)" : "\(bindAddress):\(port)"
    }

    public var authToken: String? {
        let t = token.trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? nil : t
    }

    /// One id when every worker serves the same model, otherwise `mixed` (Rust `model_summary`).
    public static func modelSummary(_ models: [String]) -> String {
        guard let first = models.first else { return defaultModel }
        return models.allSatisfy { $0 == first } ? first : "mixed"
    }

    public var pairingEnabled: Bool { !pairingPassword.isEmpty }

    /// True for one specific non-loopback address (`100.101.102.103`): the host then also
    /// listens on `127.0.0.1` so this Mac's own apps keep working. Loopback and the wildcard
    /// addresses already cover this Mac.
    public static func needsLoopbackCompanion(_ bindAddress: String) -> Bool {
        let address = bindAddress.trimmingCharacters(in: .whitespaces)
        if ["0.0.0.0", "::", "*", "localhost", "::1"].contains(address) { return false }
        if address.hasPrefix("127.") { return false }
        return true
    }

    /// Host and client names as shown: trimmed, without control characters, at most 64
    /// characters; nil when nothing is left (Rust `resolve_host_name`, `PairRequest::client_name`).
    public static func cleanName(_ raw: String, maxLength: Int = 64) -> String? {
        var kept = String.UnicodeScalarView()
        kept.append(contentsOf: raw.trimmingCharacters(in: .whitespacesAndNewlines).unicodeScalars
            .filter { $0.properties.generalCategory != .control }.prefix(maxLength))
        let name = String(kept).trimmingCharacters(in: .whitespacesAndNewlines)
        return name.isEmpty ? nil : name
    }

    /// A pairing password's length in Unicode scalars (Rust `chars().count()`).
    public static let pairingPasswordLength = 6...128

    /// Why `password` can't be a pairing password, or nil if it can. Empty means "off" and is fine.
    public static func pairingPasswordProblem(_ password: String) -> String? {
        guard !password.isEmpty, !pairingPasswordLength.contains(password.unicodeScalars.count) else { return nil }
        return "pairingPassword must be \(pairingPasswordLength.lowerBound) to \(pairingPasswordLength.upperBound) characters"
    }

    /// 32 random bytes, base64url without padding (43 characters), as PROTOCOL.md specifies
    /// for the token a host generates when pairing is turned on.
    public static func generateToken() -> String {
        var rng = SystemRandomNumberGenerator()
        let bytes = (0..<32).map { _ in UInt8.random(in: .min ... .max, using: &rng) }
        return Data(bytes).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
}

extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self { min(max(self, range.lowerBound), range.upperBound) }
}

/// The host's `FAIRSPOKEN_HOST_*` environment variables. The legacy `MULTIVOICE_HOST_*` prefix
/// is still accepted when the new name is unset.
public enum HostEnvironment {
    static let prefix = "FAIRSPOKEN_HOST_"
    static let legacyPrefix = "MULTIVOICE_HOST_"

    /// `FAIRSPOKEN_HOST_<suffix>`, else `MULTIVOICE_HOST_<suffix>`.
    public static func value(_ suffix: String, in environment: [String: String]) -> String? {
        environment[prefix + suffix] ?? environment[legacyPrefix + suffix]
    }
}

/// Loads, seeds and saves `host-config.json`.
public enum HostConfigurationStore {
    public enum StoreError: Error, LocalizedError {
        case unreadable(String)
        case invalid(String)
        case unwritable(String)
        public var errorDescription: String? {
            switch self {
            case .unreadable(let m), .invalid(let m), .unwritable(let m): m
            }
        }
    }

    /// `FAIRSPOKEN_HOST_CONFIG_PATH`, else `~/Library/Application Support/<bundle id>/host-config.json`.
    public static func defaultURL(bundleID: String, environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let path = HostEnvironment.value("CONFIG_PATH", in: environment), !path.isEmpty { return URL(fileURLWithPath: path) }
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent(bundleID, isDirectory: true).appendingPathComponent("host-config.json")
    }

    /// A missing file is a first boot (nil). An unreadable or invalid file is an error so the
    /// host never silently serves a configuration nobody chose (as the Rust host).
    public static func load(_ url: URL) throws(StoreError) -> HostConfiguration? {
        let data: Data
        do { data = try Data(contentsOf: url) } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return nil
        } catch {
            throw .unreadable("Failed to read host config \(url.path): \(error.localizedDescription)")
        }
        do { return try JSONDecoder().decode(HostConfiguration.self, from: data) } catch {
            throw .invalid("Invalid host config \(url.path) (fix or delete it): \(error.localizedDescription)")
        }
    }

    public static func save(_ config: HostConfiguration, to url: URL) throws(StoreError) {
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
            let data = try encoder.encode(config)
            try data.write(to: url, options: [.atomic])
            // It holds the bearer token: owner read/write only.
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        } catch {
            throw .unwritable("Failed to write host config: \(error.localizedDescription)")
        }
    }

    /// The effective configuration for a run. The file (if any) is the durable configuration
    /// edited in the app and through `POST /v1/config`; without one, `FAIRSPOKEN_HOST_*`
    /// variables seed it. Restart-only settings (address, token, pairing password, name,
    /// workers, queue) set in the environment override the file for this run, as they are what
    /// a LaunchAgent or a test harness passes.
    public static func resolve(file: HostConfiguration?, environment: [String: String], knownModels: Set<String>) throws(StoreError) -> HostConfiguration {
        var config = file ?? HostConfiguration()
        func string(_ suffix: String) -> String? { HostEnvironment.value(suffix, in: environment) }
        func int(_ suffix: String) -> Int? { string(suffix).flatMap { Int($0.trimmingCharacters(in: .whitespaces)) } }
        if let addr = string("ADDR"), !addr.isEmpty {
            guard let (host, port) = parseAddress(addr) else { throw .invalid("FAIRSPOKEN_HOST_ADDR is not host:port: \(addr)") }
            config.bindAddress = host
            config.port = port
        }
        if let token = string("TOKEN") { config.token = token }
        if let name = string("NAME").flatMap({ HostConfiguration.cleanName($0) }) { config.displayName = name }
        if let password = string("PAIRING_PASSWORD"), !password.isEmpty {
            guard HostConfiguration.pairingPasswordProblem(password) == nil else {
                throw .invalid("FAIRSPOKEN_HOST_PAIRING_PASSWORD must be \(HostConfiguration.pairingPasswordLength.lowerBound) to \(HostConfiguration.pairingPasswordLength.upperBound) characters")
            }
            config.pairingPassword = password
        } else if HostConfiguration.pairingPasswordProblem(config.pairingPassword) != nil {
            throw .invalid("The host config's pairingPassword must be \(HostConfiguration.pairingPasswordLength.lowerBound) to \(HostConfiguration.pairingPasswordLength.upperBound) characters (fix or delete it)")
        }
        if let workers = int("WORKERS") { config.workerCount = workers.clamped(to: HostConfiguration.workerCountRange) }
        if let queue = int("QUEUE_CAPACITY") { config.queueCapacity = queue }
        if file == nil {
            if let v = int("MAX_ACTIVE_STREAMS") { config.maxActiveStreams = v }
            if let v = int("MAX_RECORDING_SECONDS") { config.maxRecordingSeconds = v }
            if let v = string("USE_GPU")?.trimmingCharacters(in: .whitespaces).lowercased() {
                if v == "1" || v == "true" { config.useGpu = true } else if v == "0" || v == "false" { config.useGpu = false }
            }
            if let raw = string("MODEL")?.trimmingCharacters(in: .whitespaces), !raw.isEmpty {
                let ids = raw.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
                for id in ids where !knownModels.contains(id) {
                    throw .invalid("FAIRSPOKEN_HOST_MODEL has an unsupported model: \(id)")
                }
                if ids.count == 1 {
                    config.workerModels = Array(repeating: ids[0], count: config.workerCount)
                } else if ids.count == config.workerCount {
                    config.workerModels = ids
                } else {
                    throw .invalid("FAIRSPOKEN_HOST_MODEL lists \(ids.count) models for \(config.workerCount) workers; provide one model or exactly one per worker")
                }
            } else if config.workerModels.count != config.workerCount {
                config.workerModels = Array(repeating: config.workerModels.first ?? HostConfiguration.defaultModel, count: config.workerCount)
            }
        }
        return config.normalized(knownModels: knownModels)
    }

    /// `127.0.0.1:48173`, `[::]:48173`, `0.0.0.0:48173`.
    public static func parseAddress(_ text: String) -> (String, Int)? {
        let t = text.trimmingCharacters(in: .whitespaces)
        if t.hasPrefix("["), let close = t.firstIndex(of: "]") {
            let host = String(t[t.index(after: t.startIndex)..<close])
            let rest = t[t.index(after: close)...]
            guard rest.hasPrefix(":"), let port = Int(rest.dropFirst()), (0...65535).contains(port) else { return nil }
            return (host, port)
        }
        guard let colon = t.lastIndex(of: ":"), let port = Int(t[t.index(after: colon)...]), (0...65535).contains(port) else { return nil }
        let host = String(t[..<colon])
        guard !host.isEmpty, !host.contains(":") else { return nil }
        return (host == "localhost" ? "127.0.0.1" : host, port)
    }
}

/// A `POST /v1/config` body. Every field is optional; unknown fields are rejected
/// (serde's `deny_unknown_fields` in the Rust host).
public struct HostConfigUpdate: Sendable, Equatable {
    public var maxActiveStreams: Int?
    public var maxRecordingSeconds: Int?
    public var useGpu: Bool?
    public var model: String?
    public var workerModels: [String]?
    /// nil leaves pairing as it is; `""` (or JSON `null`) turns it off.
    public var pairingPassword: String?

    public init(maxActiveStreams: Int? = nil, maxRecordingSeconds: Int? = nil, useGpu: Bool? = nil, model: String? = nil, workerModels: [String]? = nil,
                pairingPassword: String? = nil) {
        self.maxActiveStreams = maxActiveStreams
        self.maxRecordingSeconds = maxRecordingSeconds
        self.useGpu = useGpu
        self.model = model
        self.workerModels = workerModels
        self.pairingPassword = pairingPassword
    }

    static let fields = ["maxActiveStreams", "maxRecordingSeconds", "useGpu", "model", "workerModels", "pairingPassword"]

    /// Parses the JSON body with the Rust host's typing rules (`u32`, `u16`, `bool`, strings).
    public static func parse(_ body: [UInt8]) throws(ParseError) -> HostConfigUpdate {
        let object: Any
        do { object = try JSONSerialization.jsonObject(with: Data(body), options: [.fragmentsAllowed]) } catch {
            throw .invalid("Invalid config update: \(body.isEmpty ? "EOF while parsing a value" : "expected a JSON object")")
        }
        guard let dict = object as? [String: Any] else {
            throw .invalid("Invalid config update: invalid type, expected struct HostConfigUpdate")
        }
        for key in dict.keys.sorted() where !fields.contains(key) {
            throw .invalid("Invalid config update: unknown field `\(key)`, expected one of `maxActiveStreams`, `maxRecordingSeconds`, `useGpu`, `model`, `workerModels`, `pairingPassword`")
        }
        var update = HostConfigUpdate()
        func present(_ key: String) -> Any? {
            guard let v = dict[key], !(v is NSNull) else { return nil }
            return v
        }
        if let v = present("maxActiveStreams") {
            guard let n = JSONInput.unsignedInteger(v, max: Int64(UInt32.max)) else {
                throw .invalid("Invalid config update: maxActiveStreams must be an unsigned 32-bit integer")
            }
            update.maxActiveStreams = Int(n)
        }
        if let v = present("maxRecordingSeconds") {
            guard let n = JSONInput.unsignedInteger(v, max: Int64(UInt16.max)) else {
                throw .invalid("Invalid config update: maxRecordingSeconds must be an unsigned 16-bit integer")
            }
            update.maxRecordingSeconds = Int(n)
        }
        if let v = present("useGpu") {
            guard JSONInput.isBool(v), let b = v as? Bool else { throw .invalid("Invalid config update: useGpu must be a boolean") }
            update.useGpu = b
        }
        if let v = present("model") {
            guard let s = v as? String else { throw .invalid("Invalid config update: model must be a string") }
            update.model = s
        }
        if let v = present("workerModels") {
            guard let list = v as? [Any], let strings = list as? [String], strings.count == list.count else {
                throw .invalid("Invalid config update: workerModels must be an array of strings")
            }
            update.workerModels = strings
        }
        if let v = dict["pairingPassword"] {
            // `null` is meaningful here: it turns pairing off, like "".
            if v is NSNull {
                update.pairingPassword = ""
            } else if let s = v as? String {
                update.pairingPassword = s
            } else {
                throw .invalid("Invalid config update: pairingPassword must be a string or null")
            }
        }
        return update
    }

    public enum ParseError: Error, Equatable {
        case invalid(String)
        public var message: String { if case .invalid(let m) = self { m } else { "" } }
    }
}

/// The settings `POST /v1/config` may change while the host runs.
public struct HostLiveSettings: Sendable, Equatable {
    public var maxActiveStreams: Int
    public var maxRecordingSeconds: Int
    public var useGpu: Bool
    public var workerModels: [String]

    public var modelSummary: String { HostConfiguration.modelSummary(workerModels) }

    /// Validates every field before applying any (Rust `apply_config_update`).
    public func applying(_ update: HostConfigUpdate, knownModels: Set<String>) throws(HostConfigUpdate.ParseError) -> HostLiveSettings {
        if let v = update.maxActiveStreams, !HostConfiguration.maxActiveStreamsRange.contains(v) {
            throw .invalid("maxActiveStreams must be between 1 and 32")
        }
        if let v = update.maxRecordingSeconds, !HostConfiguration.maxRecordingSecondsRange.contains(v) {
            throw .invalid("maxRecordingSeconds must be between 10 and 600")
        }
        let count = workerModels.count
        var nextModels: [String]?
        switch (update.model, update.workerModels) {
        case (.some, .some):
            throw .invalid("Provide either model or workerModels, not both")
        case (.some(let id), nil):
            guard knownModels.contains(id) else { throw .invalid("Unsupported model: \(id)") }
            nextModels = Array(repeating: id, count: count)
        case (nil, .some(let ids)):
            guard ids.count == count else { throw .invalid("workerModels must list exactly \(count) models (one per worker)") }
            for id in ids where !knownModels.contains(id) { throw .invalid("Unsupported model: \(id)") }
            nextModels = ids
        case (nil, nil):
            break
        }
        var next = self
        if let v = update.maxActiveStreams { next.maxActiveStreams = v }
        if let v = update.maxRecordingSeconds { next.maxRecordingSeconds = v }
        if let v = update.useGpu { next.useGpu = v }
        if let m = nextModels { next.workerModels = m }
        return next
    }

    /// The `POST /v1/config` answer. It reports whether pairing is on, never the password.
    func configResponse(pairingEnabled: Bool) -> HostJSON {
        .object([
            ("maxActiveStreams", .int(maxActiveStreams)),
            ("maxRecordingSeconds", .int(maxRecordingSeconds)),
            ("useGpu", .bool(useGpu)),
            ("model", .string(modelSummary)),
            ("workerModels", .array(workerModels.map { .string($0) })),
            ("pairingEnabled", .bool(pairingEnabled)),
        ])
    }
}
