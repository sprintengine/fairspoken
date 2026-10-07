package ie.fairspoken.mobile.core

/** Spacing for text dropped into the middle of what the user already typed. */
object TextJoin {
    private const val OPENERS = "([{\"'“‘\n\t /"

    fun insertion(before: CharSequence?, text: String, after: CharSequence?): String {
        var out = text.trim()
        if (out.isEmpty()) return out
        val prev = before?.lastOrNull()
        if (prev != null && prev !in OPENERS) out = " $out"
        val next = after?.firstOrNull()
        if (next != null && (next.isLetterOrDigit())) out = "$out "
        return out
    }
}
