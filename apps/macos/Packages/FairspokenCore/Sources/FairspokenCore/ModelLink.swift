import Foundation

/// A download link for one model: `modelLinks[<model id>]` in the client's settings.json and in
/// Fairspoken Server's host-config.json. The link points at an archive (zip, tar or tar.gz) of
/// the model's files, for networks that can't reach the standard download.
///
/// - `http://` or `https://`: downloaded as is, query string included (Artifactory and
///   SharePoint links carry one). A `#fragment` is dropped; a user name or password is refused.
/// - An absolute path, `~/…` or a `file://` URL: an archive file on this Mac or a mounted share.
public struct ModelLink: Equatable, Sendable {
    public enum Location: Equatable, Sendable {
        /// An http(s) URL, without a fragment.
        case web(URL)
        /// A file URL, absolute and standardised.
        case file(URL)
    }

    public let location: Location

    /// The longest link accepted, in characters.
    public static let maxLength = 2048

    public init(_ location: Location) { self.location = location }

    // MARK: Parsing

    /// Parses a link as typed. Leading and trailing whitespace is ignored.
    public static func parse(_ raw: String, homeDirectory: String = NSHomeDirectory()) throws(ModelLinkError) -> ModelLink {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { throw .empty }
        guard text.count <= maxLength else { throw .tooLong(text.count) }
        let lower = text.lowercased()
        if lower.hasPrefix("http://") || lower.hasPrefix("https://") {
            return ModelLink(.web(try webURL(text)))
        }
        if lower.hasPrefix("file://") {
            guard let url = URL(string: text), url.isFileURL else { throw .invalidURL(redacted(text)) }
            if let host = url.host(percentEncoded: false), !host.isEmpty, host.lowercased() != "localhost" {
                throw .remoteFileURL(text)
            }
            let path = url.plainPath
            guard path.hasPrefix("/") else { throw .relativePath(text) }
            return ModelLink(.file(URL(fileURLWithPath: path).standardizedFileURL))
        }
        if let scheme = scheme(of: text) { throw .unsupportedScheme(scheme) }
        if text.hasPrefix("~/") {
            return ModelLink(.file(URL(fileURLWithPath: homeDirectory + String(text.dropFirst())).standardizedFileURL))
        }
        guard text.hasPrefix("/") else { throw .relativePath(text) }
        return ModelLink(.file(URL(fileURLWithPath: text).standardizedFileURL))
    }

    /// The link as saved: trimmed, and an http(s) URL without its fragment. Throws when the text
    /// can't be a link.
    public static func normalize(_ raw: String) throws(ModelLinkError) -> String {
        let link = try parse(raw)
        if case .web(let url) = link.location { return url.absoluteString }
        return raw.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Why `raw` can't be a link, or nil when it can.
    public static func problem(_ raw: String) -> String? {
        do {
            _ = try parse(raw)
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    /// A `modelLinks` map as stored: values trimmed (and normalised when valid), blank entries
    /// removed. A value that isn't a link is kept, trimmed, so the download reports why.
    public static func normalizeMap(_ links: [String: String]) -> [String: String] {
        var out: [String: String] = [:]
        for (id, raw) in links {
            let id = id.trimmingCharacters(in: .whitespacesAndNewlines)
            let text = raw.replacingOccurrences(of: "\0", with: "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !id.isEmpty, !text.isEmpty else { continue }
            out[id] = (try? normalize(text)) ?? text
        }
        return out
    }

    private static func webURL(_ text: String) throws(ModelLinkError) -> URL {
        guard var components = URLComponents(string: text), let host = components.host, !host.isEmpty else {
            throw .invalidURL(redacted(text))
        }
        if components.user != nil || components.password != nil { throw .credentials(host) }
        components.fragment = nil
        guard let url = components.url else { throw .invalidURL(redacted(text)) }
        return url
    }

    /// `smb` for `smb://server/share`. Only a scheme followed by `://` counts, so a file name
    /// with a colon in it is still a path.
    private static func scheme(of text: String) -> String? {
        guard let range = text.range(of: "://") else { return nil }
        let scheme = text[..<range.lowerBound]
        guard let first = scheme.first, first.isLetter,
              scheme.allSatisfy({ $0.isLetter || $0.isNumber || "+-.".contains($0) }) else { return nil }
        return String(scheme)
    }

    /// Text up to the first `?` or `#`, for messages about a link that didn't parse.
    static func redacted(_ text: String) -> String {
        String(text.prefix { $0 != "?" && $0 != "#" })
    }

    // MARK: Showing

    public var isFile: Bool {
        if case .file = location { return true }
        return false
    }

    /// The URL to fetch (a web link keeps its query string).
    public var url: URL {
        switch location {
        case .web(let url), .file(let url): url
        }
    }

    /// For messages: `scheme://host[:port]/path` for a web link (never the query string, which
    /// may carry a token), the path for a file.
    public var display: String {
        switch location {
        case .web(let url):
            let scheme = url.scheme?.lowercased() ?? "https"
            return "\(scheme)://\(hostAndPort(url))\(url.path(percentEncoded: false))"
        case .file(let url):
            return url.plainPath
        }
    }

    /// For a card or a log line: `host[:port]/path` for a web link, the path for a file.
    public var shortDisplay: String {
        switch location {
        case .web(let url): hostAndPort(url) + url.path(percentEncoded: false)
        case .file(let url): url.plainPath
        }
    }

    private func hostAndPort(_ url: URL) -> String {
        let host = url.host(percentEncoded: false) ?? ""
        let shown = host.contains(":") ? "[\(host)]" : host
        return url.port.map { "\(shown):\($0)" } ?? shown
    }

    /// `display` for a stored value, or the value up to any query when it doesn't parse.
    public static func display(_ raw: String) -> String {
        (try? parse(raw))?.display ?? redacted(raw.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    /// `shortDisplay` for a stored value, or the value up to any query when it doesn't parse.
    public static func shortDisplay(_ raw: String) -> String {
        (try? parse(raw))?.shortDisplay ?? redacted(raw.trimmingCharacters(in: .whitespacesAndNewlines))
    }
}

/// What went wrong with a link or with installing from it. Every message names the link as
/// `ModelLink.display` shows it (never a query string or credentials).
public enum ModelLinkError: Error, Equatable, Sendable, LocalizedError {
    case empty
    case tooLong(Int)
    case unsupportedScheme(String)
    /// The host of a URL that carried a user name or password (never the credentials).
    case credentials(String)
    case invalidURL(String)
    case relativePath(String)
    case remoteFileURL(String)
    case downloadFailed(link: String, reason: String)
    case isFolder(link: String)
    /// The download isn't a zip or tar archive; `webPage` when it looks like HTML (a sign-in page).
    case notAnArchive(link: String, webPage: Bool)
    case unpackFailed(link: String, reason: String)
    case unsafeEntry(link: String, entry: String)
    case tooLarge(link: String, limit: Int64)
    case tooManyEntries(link: String, limit: Int)
    case missingModelFiles(link: String, model: String, needed: [String])
    case installFailed(link: String, reason: String)

    public var errorDescription: String? {
        switch self {
        case .empty:
            "Enter a link to the model's .zip file, or the path to one."
        case .tooLong(let n):
            "The link is \(n) characters long; the most is \(ModelLink.maxLength)."
        case .unsupportedScheme(let scheme):
            "A model link can't be a \(scheme):// address. Use an http:// or https:// link, or the path to a file."
        case .credentials(let host):
            "The link to \(host) contains a user name or password. Leave them out of the link."
        case .invalidURL(let text):
            "“\(text)” isn't a valid link."
        case .relativePath(let text):
            "“\(text)” is a relative path. Use a full path such as /Volumes/Models/parakeet.zip, or an https:// link."
        case .remoteFileURL(let text):
            "“\(text)” names another computer. Mount the share and use the file's path (/Volumes/…)."
        case .downloadFailed(let link, let reason):
            "Couldn't download \(link): \(reason)"
        case .isFolder(let link):
            "\(link) is a folder. Use a .zip of the model instead."
        case .notAnArchive(let link, let webPage):
            webPage
                ? "\(link) isn't a zip or tar archive: it returned a web page (perhaps a sign-in page). Use a link that downloads the file itself."
                : "\(link) isn't a zip or tar archive."
        case .unpackFailed(let link, let reason):
            "Couldn't unpack \(link): \(reason)"
        case .unsafeEntry(let link, let entry):
            "\(link) has an entry that points outside the archive (“\(entry)”), so it wasn't installed."
        case .tooLarge(let link, let limit):
            "\(link) unpacks to more than \(limit >> 30) GiB, so it wasn't installed."
        case .tooManyEntries(let link, let limit):
            "\(link) holds more than \(limit.formatted(.number.grouping(.automatic).locale(Locale(identifier: "en_US")))) files, so it wasn't installed."
        case .missingModelFiles(let link, let model, let needed):
            "The download from \(link) doesn't contain the \(model) Core ML files. It needs \(needed.joined(separator: ", "))."
        case .installFailed(let link, let reason):
            "Couldn't install the model from \(link): \(reason)"
        }
    }

    /// A network error in words. macOS allows plain http:// only to local addresses (loopback,
    /// private addresses, `.local` and single-label names), so say so rather than "the resource
    /// could not be loaded".
    public static func describe(_ error: Error) -> String {
        if let urlError = error as? URLError, urlError.code == .appTransportSecurityRequiresSecureConnection {
            return "macOS allows plain http:// only on the local network. Use the server's https:// address."
        }
        return error.localizedDescription
    }
}

extension URL {
    /// The file path, decoded and without a trailing slash (`/` stays), for messages and checks.
    var plainPath: String {
        var p = path(percentEncoded: false)
        while p.count > 1, p.hasSuffix("/") { p.removeLast() }
        return p
    }
}
