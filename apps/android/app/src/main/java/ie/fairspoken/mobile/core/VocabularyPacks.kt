package ie.fairspoken.mobile.core

/**
 * The bundled packs' always-on terms (src-tauri/packs/<id>.json), sent to the
 * host as vocabulary hints. The desktop app also retrieves terms by sound;
 * the phone keeps to the always-on list until packs sync from the host.
 */
object VocabularyPacks {
    data class Pack(val id: String, val name: String, val terms: List<String>)

    val all = listOf(
        Pack(
            "software-engineering", "Software engineering",
            listOf(
                "kubectl", "Kubernetes", "PostgreSQL", "nginx", "OAuth", "GraphQL", "TypeScript", "JavaScript",
                "JSON", "YAML", "npm", "pnpm", "Tauri", "Vite", "Cargo", "GitHub", "Docker", "Terraform", "Redis",
                "SQLite", "Next.js", "Node.js", "Claude Code", "MCP", "CI/CD", "API", "SSH", "localhost", "PR", "README",
            ),
        ),
        Pack(
            "ie-general-practice", "GP practice",
            listOf(
                "Healthlink", "Healthmail", "HbA1c", "eGFR", "atorvastatin", "levothyroxine", "esomeprazole",
                "colecalciferol", "bisoprolol", "salbutamol", "rosuvastatin", "pantoprazole", "amlodipine",
                "co-codamol", "ramipril", "lansoprazole", "lercanidipine", "sertraline", "escitalopram", "apixaban",
                "mirtazapine", "pregabalin", "GMS", "CDM", "ICGP", "PCRS", "MED1", "Socrates",
                "Helix Practice Manager", "CompleteGP",
            ),
        ),
    )

    fun hintsFor(packId: String?): List<String> = all.firstOrNull { it.id == packId }?.terms.orEmpty()
}
