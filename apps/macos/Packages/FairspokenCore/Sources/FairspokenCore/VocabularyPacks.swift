import Foundation

/// The bundled vocabulary packs' always-on terms (`src-tauri/packs/<id>.json`), sent to the
/// host as vocabulary hints after the user's own. The Tauri app also retrieves pack terms by
/// sound for polish; this app, like Android, keeps to the always-on lists.
public enum VocabularyPacks {
    public struct Pack: Sendable, Equatable, Identifiable {
        public let id: String
        public let name: String
        public let description: String
        public let terms: [String]
    }

    /// The host header carried at most this many terms before packs existed; it still does
    /// (`REMOTE_HINT_LIMIT` in vocabulary_packs.rs).
    public static let hintLimit = 50

    public static let all: [Pack] = [
        Pack(
            id: "ie-general-practice", name: "Irish general practice",
            description: "Medicines authorised in Ireland, Irish GP systems and schemes, clinical abbreviations, and Irish names and places.",
            terms: [
                "Healthlink", "Healthmail", "HbA1c", "eGFR", "atorvastatin", "levothyroxine", "esomeprazole",
                "colecalciferol", "bisoprolol", "salbutamol", "rosuvastatin", "pantoprazole", "amlodipine",
                "co-codamol", "ramipril", "lansoprazole", "lercanidipine", "sertraline", "escitalopram", "apixaban",
                "mirtazapine", "pregabalin", "GMS", "CDM", "ICGP", "PCRS", "MED1", "Socrates",
                "Helix Practice Manager", "CompleteGP",
            ]),
        Pack(
            id: "software-engineering", name: "Software engineering",
            description: "Languages, frameworks, tools, cloud services, command-line programs, file formats and acronyms.",
            terms: [
                "kubectl", "Kubernetes", "PostgreSQL", "nginx", "OAuth", "GraphQL", "TypeScript", "JavaScript",
                "JSON", "YAML", "npm", "pnpm", "Tauri", "Vite", "Cargo", "GitHub", "Docker", "Terraform", "Redis",
                "SQLite", "Next.js", "Node.js", "Claude Code", "MCP", "CI/CD", "API", "SSH", "localhost", "PR", "README",
            ]),
    ]

    /// The terms sent to the host: the user's own, then each enabled pack's always-on list in
    /// bundled order, skipping duplicates, `hintLimit` in all. Mirrors `remote_hints`.
    public static func hints(userTerms: [String], enabledPacks: [String]) -> [String] {
        var hints = Array(userTerms.prefix(hintLimit))
        for pack in all where enabledPacks.contains(pack.id) {
            for term in pack.terms {
                guard hints.count < hintLimit else { return hints }
                if !hints.contains(where: { $0.caseInsensitiveCompare(term) == .orderedSame }) {
                    hints.append(term)
                }
            }
        }
        return hints
    }

    /// Same as `normalize_enabled_packs` in settings.rs: trimmed, at most 64 characters,
    /// de-duplicated, at most 20. Ids this build does not ship are kept (a downgrade does not
    /// forget them) and ignored at use.
    public static func normalizeEnabled(_ ids: [String]) -> [String] {
        var out: [String] = []
        for id in ids {
            let clean = String(id.trimmingCharacters(in: .whitespacesAndNewlines).prefix(64))
            guard !clean.isEmpty, !out.contains(clean) else { continue }
            out.append(clean)
            if out.count >= 20 { break }
        }
        return out
    }
}
