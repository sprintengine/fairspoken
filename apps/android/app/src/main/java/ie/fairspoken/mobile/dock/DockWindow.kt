package ie.fairspoken.mobile.dock

import android.annotation.SuppressLint
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.res.Configuration
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.view.ContextThemeWrapper
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.ViewConfiguration
import android.view.ViewGroup
import android.view.Window
import android.view.WindowManager
import android.widget.FrameLayout
import androidx.activity.ComponentDialog
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.platform.ComposeView
import ie.fairspoken.mobile.FairspokenApp
import ie.fairspoken.mobile.R
import ie.fairspoken.mobile.core.DictationState
import ie.fairspoken.mobile.insert.InsertService
import kotlinx.coroutines.Job
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlin.math.abs
import kotlin.math.roundToInt

/**
 * The bubble that floats over other apps. A dialog window rather than a bare
 * overlay view, because only a window can blur what is behind it.
 */
class DockWindow(context: Context, private val app: FairspokenApp) {
    private val themed = ContextThemeWrapper(context, R.style.Theme_Fairspoken_Dock)
    private val dialog = ComponentDialog(themed, R.style.Theme_Fairspoken_Dock)
    private val window: Window = dialog.window!!
    private val windowManager = context.getSystemService(WindowManager::class.java)
    private val density = context.resources.displayMetrics.density
    private val root: DockTouchLayer

    /** True from the dock starting a dictation until its result has been shown. */
    val ownsSession = MutableStateFlow(false)
    val message = mutableStateOf<DockMessage?>(null)
    val levels = mutableStateListOf<Float>().apply { repeat(BARS) { add(0f) } }

    private val scope = MainScope()
    private var holding = false
    private var messageJob: Job? = null
    private var blurListener: ((Boolean) -> Unit)? = null
    private var blurOn = false

    init {
        window.setType(WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY)
        window.addFlags(
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS,
        )
        window.clearFlags(WindowManager.LayoutParams.FLAG_DIM_BEHIND)
        window.setGravity(Gravity.TOP or Gravity.START)

        root = DockTouchLayer(themed)
        root.addView(
            ComposeView(themed).apply { setContent { DockContent(app, this@DockWindow) } },
            FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT),
        )
        dialog.setContentView(root)
        // Docked on the right, grow leftwards so the listening pill stays on screen.
        root.addOnLayoutChangeListener { _, left, _, right, _, oldLeft, _, oldRight, _ ->
            if (oldRight == oldLeft || right - left == oldRight - oldLeft) return@addOnLayoutChangeListener
            val screen = windowManager.currentWindowMetrics.bounds
            val attrs = window.attributes
            val width = right - left
            val oldWidth = oldRight - oldLeft
            if (attrs.x + oldWidth / 2 > screen.width() / 2 || attrs.x + width > screen.width()) {
                attrs.x = (attrs.x + oldWidth - width).coerceIn(0, screen.width() - width)
                window.attributes = attrs
            }
        }
        // The blur lives on the window's decor, which exists only after setContentView.
        applyGlass(Build.VERSION.SDK_INT >= 31 && windowManager.isCrossWindowBlurEnabled)
        if (Build.VERSION.SDK_INT >= 31) {
            val listener: (Boolean) -> Unit = { enabled -> applyGlass(enabled) }
            windowManager.addCrossWindowBlurEnabledListener(context.mainExecutor, listener)
            blurListener = listener
        }
        dialog.setCancelable(false)
        window.setLayout(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT)
        val settings = app.store.settings.value
        val screen = windowManager.currentWindowMetrics.bounds
        window.attributes = window.attributes.apply {
            x = if (settings.dockX != Int.MIN_VALUE) settings.dockX else screen.width() - dp(56 + 12)
            y = if (settings.dockY != Int.MIN_VALUE) settings.dockY else (screen.height() * 0.42f).toInt()
            windowAnimations = 0
        }

        scope.launch {
            app.dictation.level.collect { level ->
                if (ownsSession.value && app.dictation.isListening) {
                    levels.removeAt(0)
                    levels.add(level)
                }
            }
        }
        scope.launch {
            app.dictation.state.collect { state ->
                if (!ownsSession.value) return@collect
                when (state) {
                    is DictationState.Failed -> show(DockMessage(state.message, ok = false), 2_600)
                    DictationState.Idle -> if (message.value == null) ownsSession.value = false
                    else -> Unit
                }
            }
        }
    }

    fun setVisible(visible: Boolean) {
        if (visible && !dialog.isShowing) {
            dialog.show()
            app.warmUp()
        } else if (!visible && dialog.isShowing) {
            dialog.hide()
        }
    }

    fun dismiss() {
        if (ownsSession.value && app.dictation.isBusy) app.dictation.cancel()
        if (Build.VERSION.SDK_INT >= 31) blurListener?.let { windowManager.removeCrossWindowBlurEnabledListener(it) }
        dialog.dismiss()
        scope.cancel()
    }

    private fun onTap() {
        when {
            app.dictation.isListening && ownsSession.value -> stop()
            app.dictation.isBusy -> Unit
            message.value != null && message.value?.ok == false -> clearMessage()
            else -> start()
        }
    }

    private fun start() {
        clearMessage()
        levels.indices.forEach { levels[it] = 0f }
        ownsSession.value = true
        root.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
        app.dictation.start(::deliver)
    }

    private fun stop() {
        root.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
        app.dictation.stop()
    }

    private fun deliver(text: String) {
        val result = InsertService.instance?.insert(text) ?: InsertService.Result.NoField
        val note = when (result) {
            InsertService.Result.Inserted -> "Inserted"
            InsertService.Result.Pasted -> "Pasted"
            InsertService.Result.NoField -> {
                window.context.getSystemService(ClipboardManager::class.java)
                    .setPrimaryClip(ClipData.newPlainText("Fairspoken", text))
                "Copied"
            }
        }
        root.performHapticFeedback(HapticFeedbackConstants.CONFIRM)
        show(DockMessage(note, ok = true), 1_400)
    }

    private fun show(note: DockMessage, forMs: Long) {
        message.value = note
        messageJob?.cancel()
        messageJob = scope.launch {
            delay(forMs)
            clearMessage()
        }
    }

    private fun clearMessage() {
        messageJob?.cancel()
        message.value = null
        if (!app.dictation.isBusy) ownsSession.value = false
    }

    /** Crystal glass behind the bubble: silver in light mode, smoked in dark. */
    private fun applyGlass(blur: Boolean) {
        blurOn = blur
        val dark = (root.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
            Configuration.UI_MODE_NIGHT_YES
        val fill = when {
            dark && blur -> 0xB81B1F24.toInt()
            dark -> 0xF21B1F24.toInt()
            blur -> 0xA6F4F6F8.toInt()
            else -> 0xF2EDEFF2.toInt()
        }
        window.setBackgroundDrawable(
            GradientDrawable().apply {
                cornerRadius = dp(28).toFloat()
                setColor(fill)
            },
        )
        if (Build.VERSION.SDK_INT >= 31) window.setBackgroundBlurRadius(if (blur) dp(22) else 0)
    }

    private fun moveBy(dx: Float, dy: Float) {
        window.attributes = window.attributes.apply {
            x += dx.roundToInt()
            y += dy.roundToInt()
        }
    }

    /** Snaps to the nearer side edge and remembers the spot. */
    private fun settle() {
        val screen = windowManager.currentWindowMetrics.bounds
        val attrs = window.attributes
        val width = root.width.takeIf { it > 0 } ?: dp(56)
        val height = root.height.takeIf { it > 0 } ?: dp(56)
        attrs.x = if (attrs.x + width / 2 < screen.width() / 2) dp(12) else screen.width() - width - dp(12)
        attrs.y = attrs.y.coerceIn(dp(40), screen.height() - height - dp(40))
        window.attributes = attrs
        app.store.updateSettings { it.copy(dockX = attrs.x, dockY = attrs.y) }
    }

    private fun dp(value: Int) = (value * density).roundToInt()

    /**
     * Raw touch handling, so the window can follow the finger without the
     * feedback loop of measuring a drag inside the window being dragged.
     * Tap: start or stop. Hold: talk while held. Drag: move.
     */
    @SuppressLint("ViewConstructor")
    private inner class DockTouchLayer(context: Context) : FrameLayout(context) {
        private val slop = ViewConfiguration.get(context).scaledTouchSlop
        private var downX = 0f
        private var downY = 0f
        private var lastX = 0f
        private var lastY = 0f
        private var dragging = false
        private val holdStart = Runnable {
            if (!app.dictation.isBusy) {
                holding = true
                start()
            }
        }

        override fun onInterceptTouchEvent(ev: MotionEvent) = true

        /** Light and dark follow the system, so the glass is redone when it flips. */
        override fun onConfigurationChanged(newConfig: Configuration?) {
            super.onConfigurationChanged(newConfig)
            applyGlass(blurOn)
        }

        @SuppressLint("ClickableViewAccessibility")
        override fun onTouchEvent(ev: MotionEvent): Boolean {
            when (ev.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    downX = ev.rawX; downY = ev.rawY
                    lastX = ev.rawX; lastY = ev.rawY
                    dragging = false
                    holding = false
                    postDelayed(holdStart, HOLD_MS)
                }
                MotionEvent.ACTION_MOVE -> {
                    if (!dragging && !holding && (abs(ev.rawX - downX) > slop || abs(ev.rawY - downY) > slop)) {
                        dragging = true
                        removeCallbacks(holdStart)
                    }
                    if (dragging) moveBy(ev.rawX - lastX, ev.rawY - lastY)
                    lastX = ev.rawX; lastY = ev.rawY
                }
                MotionEvent.ACTION_UP -> {
                    removeCallbacks(holdStart)
                    when {
                        dragging -> settle()
                        holding -> stop()
                        else -> onTap()
                    }
                    holding = false
                }
                MotionEvent.ACTION_CANCEL -> {
                    removeCallbacks(holdStart)
                    if (dragging) settle()
                    if (holding) stop()
                    holding = false
                }
            }
            return true
        }
    }

    companion object {
        const val BARS = 12
        private const val HOLD_MS = 320L
    }
}

data class DockMessage(val text: String, val ok: Boolean)
