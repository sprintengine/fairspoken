import Foundation

/// Restores the user's casing for vocabulary terms the recogniser already got right
/// ("amoxicillin" → "Amoxicillin", "pnpm" stays "pnpm" if that is how the user wrote it).
/// FluidAudio's CTC vocabulary boosting (opt-in, Phase 1) does not fix casing of terms that
/// were recognised anyway, so this pass runs regardless. Whole-word, case-insensitive,
/// longest terms first so multi-word entries win over their parts.
public enum VocabularyCasing {
    public static func apply(_ text: String, vocabulary: [String]) -> String {
        let terms = vocabulary
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty && $0.contains(where: \.isLetter) }
            .sorted { $0.count > $1.count }
        guard !terms.isEmpty, !text.isEmpty else { return text }
        var result = text
        for term in terms {
            let pattern = "(?<![\\p{L}\\p{N}])" + NSRegularExpression.escapedPattern(for: term) + "(?![\\p{L}\\p{N}])"
            guard let regex = try? NSRegularExpression(pattern: pattern, options: [.caseInsensitive]) else { continue }
            let range = NSRange(result.startIndex..., in: result)
            result = regex.stringByReplacingMatches(in: result, range: range,
                                                    withTemplate: NSRegularExpression.escapedTemplate(for: term))
        }
        return result
    }
}
