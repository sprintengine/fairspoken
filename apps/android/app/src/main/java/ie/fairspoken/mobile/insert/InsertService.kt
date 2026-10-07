package ie.fairspoken.mobile.insert

import android.accessibilityservice.AccessibilityService
import android.content.ClipData
import android.content.ClipboardManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import ie.fairspoken.mobile.core.TextJoin
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Types the dock's transcript into the focused field of the app underneath,
 * and tells the dock when a keyboard is on screen. It reads only the focused
 * editable field, only when inserting.
 */
class InsertService : AccessibilityService() {
    private val handler = Handler(Looper.getMainLooper())
    private val recheck = Runnable { _keyboardOpen.value = computeKeyboardOpen() }

    override fun onServiceConnected() {
        instance = this
        _connected.value = true
        recheck.run()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        // Window events arrive in bursts; look once they settle.
        handler.removeCallbacks(recheck)
        handler.postDelayed(recheck, 90)
    }

    override fun onInterrupt() = Unit

    override fun onDestroy() {
        instance = null
        _connected.value = false
        _keyboardOpen.value = false
        super.onDestroy()
    }

    private fun computeKeyboardOpen(): Boolean = runCatching {
        windows.any { window ->
            window.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD &&
                // Our own voice keyboard already has a mic; no dock on top of it.
                window.root?.packageName?.toString() != packageName
        }
    }.getOrDefault(false)

    enum class Result { Inserted, Pasted, NoField }

    fun insert(text: String): Result {
        val focused = findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
            ?.let { if (it.isEditable) it else focusedEditableIn(it) ?: it }
        val node = focused
            ?.takeIf { it.isEditable && !it.isPassword }
            ?: return Result.NoField
        val current = if (node.isShowingHintText) "" else node.text?.toString().orEmpty()
        var start = node.textSelectionStart
        var end = node.textSelectionEnd
        if (start !in 0..current.length || end !in 0..current.length) {
            start = current.length
            end = current.length
        }
        if (start > end) start = end.also { end = start }
        val insertion = TextJoin.insertion(current.substring(0, start), text, current.substring(end))
        val updated = current.substring(0, start) + insertion + current.substring(end)

        // Web editors apply SET_TEXT late, or not at all for rich editors, so a
        // web field is pasted into and a native one is set (synchronously).
        // Never both: a late SET_TEXT plus a paste would insert twice.
        if (!isWebContent(node)) {
            val setText = Bundle().apply {
                putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, updated)
            }
            if (node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, setText)) {
                val caret = start + insertion.length
                node.performAction(
                    AccessibilityNodeInfo.ACTION_SET_SELECTION,
                    Bundle().apply {
                        putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, caret)
                        putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, caret)
                    },
                )
                return Result.Inserted
            }
        }
        getSystemService(ClipboardManager::class.java)
            .setPrimaryClip(ClipData.newPlainText("Fairspoken", insertion))
        val pasted = node.performAction(AccessibilityNodeInfo.ACTION_PASTE)
        return if (pasted) Result.Pasted else Result.NoField
    }

    private fun isWebContent(node: AccessibilityNodeInfo): Boolean {
        var current: AccessibilityNodeInfo? = node
        repeat(40) {
            val n = current ?: return false
            if (n.className?.contains("WebView") == true) return true
            current = n.parent
        }
        return false
    }

    /** Web views report their own view as focused; the field is a focused editable node inside it. */
    private fun focusedEditableIn(root: AccessibilityNodeInfo): AccessibilityNodeInfo? {
        val queue = ArrayDeque<AccessibilityNodeInfo>().apply { add(root) }
        var visited = 0
        while (queue.isNotEmpty() && visited < 2_000) {
            val node = queue.removeFirst()
            visited++
            if (node.isEditable && node.isFocused) return node
            for (i in 0 until node.childCount) node.getChild(i)?.let(queue::addLast)
        }
        return null
    }

    companion object {
        @Volatile var instance: InsertService? = null
            private set

        private val _connected = MutableStateFlow(false)
        val connected: StateFlow<Boolean> = _connected.asStateFlow()

        private val _keyboardOpen = MutableStateFlow(false)
        val keyboardOpen: StateFlow<Boolean> = _keyboardOpen.asStateFlow()
    }
}
