package ie.fairspoken.mobile.ime

import android.inputmethodservice.InputMethodService
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import androidx.compose.ui.platform.ComposeView
import androidx.compose.ui.platform.ViewCompositionStrategy
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import androidx.lifecycle.setViewTreeLifecycleOwner
import androidx.savedstate.SavedStateRegistry
import androidx.savedstate.SavedStateRegistryController
import androidx.savedstate.SavedStateRegistryOwner
import androidx.savedstate.setViewTreeSavedStateRegistryOwner
import ie.fairspoken.mobile.core.TextJoin
import ie.fairspoken.mobile.fairspoken

/**
 * A voice-only keyboard. Opens listening, streams to the host while you
 * speak, and commits the transcript at the cursor of whatever app you're in.
 */
class VoiceKeyboardService : InputMethodService(), LifecycleOwner, SavedStateRegistryOwner {
    private val lifecycleRegistry = LifecycleRegistry(this)
    private val savedState = SavedStateRegistryController.create(this)
    override val lifecycle: Lifecycle get() = lifecycleRegistry
    override val savedStateRegistry: SavedStateRegistry get() = savedState.savedStateRegistry

    private val app get() = fairspoken
    private val panel = KeyboardPanelState()

    override fun onCreate() {
        super.onCreate()
        savedState.performRestore(null)
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_CREATE)
    }

    override fun onCreateInputView(): View {
        // Compose finds its lifecycle on the window's root view.
        window.window?.decorView?.let { root ->
            root.setViewTreeLifecycleOwner(this)
            root.setViewTreeSavedStateRegistryOwner(this)
        }
        return ComposeView(this).apply {
            setViewCompositionStrategy(ViewCompositionStrategy.DisposeOnViewTreeLifecycleDestroyed)
            setContent {
                KeyboardPanel(
                    app = app,
                    panel = panel,
                    onMic = ::toggle,
                    onSwitch = ::switchAway,
                    onPicker = { getSystemService(InputMethodManager::class.java).showInputMethodPicker() },
                    onBackspace = ::backspace,
                    onDeleteWord = ::deleteWord,
                    onSpace = { currentInputConnection?.commitText(" ", 1) },
                    onEnter = ::enter,
                    onUndo = ::undo,
                )
            }
        }
    }

    override fun onStartInputView(info: EditorInfo?, restarting: Boolean) {
        super.onStartInputView(info, restarting)
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_RESUME)
        panel.lastInsert = null
        app.warmUp()
        if (!restarting && app.store.settings.value.autoListen && !app.dictation.isBusy) toggle()
    }

    override fun onFinishInputView(finishingInput: Boolean) {
        // Don't keep the microphone open behind a closed keyboard.
        if (app.dictation.isListening) app.dictation.cancel()
        // Also called from super.onDestroy(), after the lifecycle has ended.
        if (lifecycleRegistry.currentState.isAtLeast(Lifecycle.State.RESUMED)) {
            lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_PAUSE)
        }
        super.onFinishInputView(finishingInput)
    }

    override fun onDestroy() {
        super.onDestroy()
        lifecycleRegistry.handleLifecycleEvent(Lifecycle.Event.ON_DESTROY)
    }

    /** Full-screen extract mode would hide the app; a voice keyboard never needs it. */
    override fun onEvaluateFullscreenMode() = false

    private fun toggle() {
        app.dictation.toggle(::commit)
    }

    private fun commit(text: String) {
        val ic = currentInputConnection ?: return
        val insertion = TextJoin.insertion(ic.getTextBeforeCursor(1, 0), text, ic.getTextAfterCursor(1, 0))
        ic.commitText(insertion, 1)
        panel.lastInsert = insertion
    }

    private fun undo() {
        val ic = currentInputConnection ?: return
        val last = panel.lastInsert ?: return
        if (ic.getTextBeforeCursor(last.length, 0)?.toString() == last) ic.deleteSurroundingText(last.length, 0)
        panel.lastInsert = null
    }

    private fun backspace() {
        val ic = currentInputConnection ?: return
        panel.lastInsert = null
        val selected = ic.getSelectedText(0)
        if (!selected.isNullOrEmpty()) ic.commitText("", 1) else sendDownUpKeyEvents(KeyEvent.KEYCODE_DEL)
    }

    /** Long-press delete: the word before the cursor and the space after it. */
    private fun deleteWord() {
        val ic = currentInputConnection ?: return
        panel.lastInsert = null
        val before = ic.getTextBeforeCursor(64, 0)?.toString().orEmpty()
        val trimmed = before.trimEnd()
        val start = trimmed.lastIndexOfAny(charArrayOf(' ', '\n')) + 1
        ic.deleteSurroundingText(before.length - start, 0)
    }

    private fun enter() {
        val info = currentInputEditorInfo
        val action = info?.imeOptions?.and(EditorInfo.IME_MASK_ACTION) ?: EditorInfo.IME_ACTION_NONE
        val noEnterAction = (info?.imeOptions ?: 0) and EditorInfo.IME_FLAG_NO_ENTER_ACTION != 0
        if (!noEnterAction && action != EditorInfo.IME_ACTION_NONE && action != EditorInfo.IME_ACTION_UNSPECIFIED) {
            currentInputConnection?.performEditorAction(action)
        } else {
            sendDownUpKeyEvents(KeyEvent.KEYCODE_ENTER)
        }
    }

    private fun switchAway() {
        if (app.dictation.isListening) app.dictation.cancel()
        if (!switchToPreviousInputMethod()) {
            getSystemService(InputMethodManager::class.java).showInputMethodPicker()
        }
    }
}
