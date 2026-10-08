package ie.fairspoken.mobile.core

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.SystemClock
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class DictationStats(
    val audioSeconds: Double,
    /** From the user stopping to the text being ready. */
    val latencyMs: Long,
    val engine: String,
)

sealed interface DictationState {
    data object Idle : DictationState
    data class Listening(val startedAt: Long) : DictationState
    data object Transcribing : DictationState
    data class Done(val text: String, val stats: DictationStats) : DictationState
    data class Failed(val message: String) : DictationState
}

/**
 * One dictation at a time, shared by the in-app tester, the voice keyboard
 * and the floating dock. Audio streams to the active host while the user
 * speaks; stopping only closes the upload.
 */
class Dictation(
    private val context: Context,
    private val store: HostStore,
    private val client: HostClient,
    private val scope: CoroutineScope,
) {
    private val _state = MutableStateFlow<DictationState>(DictationState.Idle)
    val state: StateFlow<DictationState> = _state.asStateFlow()

    /** Microphone level 0..1, ten times a second while listening. */
    private val _level = MutableStateFlow(0f)
    val level: StateFlow<Float> = _level.asStateFlow()

    private var capture: AudioCapture? = null
    private var upload: StreamingUpload? = null
    private var deliver: ((String) -> Unit)? = null
    private var startedAt = 0L
    private var settleJob: Job? = null

    val isBusy: Boolean
        get() = _state.value is DictationState.Listening || _state.value is DictationState.Transcribing

    val isListening: Boolean
        get() = _state.value is DictationState.Listening

    /** Starts listening; [onText] gets the transcript if the dictation succeeds. */
    fun start(onText: (String) -> Unit): Boolean {
        if (isBusy) return false
        settleJob?.cancel()
        val host = store.active ?: return fail("Pair a host first")
        if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            return fail("Allow the microphone in Fairspoken")
        }
        val settings = store.settings.value
        val newUpload = client.openStream(host, settings.superMode, VocabularyPacks.hintsFor(settings.packId))
        val tap = DebugAudio.startIfEnabled(context)
        lateinit var newCapture: AudioCapture
        newCapture = AudioCapture(
            replay = DebugAudio.replayFile(context),
            onFrame = { samples, count ->
                newUpload.send(AudioCapture.SAMPLE_RATE, samples, count)
                tap?.write(samples, count)
            },
            onLevel = { _level.value = it },
            onFailure = {
                scope.launch {
                    if (capture !== newCapture) return@launch
                    cancel()
                    fail("The microphone stopped")
                }
            },
        )
        if (!newCapture.start()) {
            newUpload.cancel()
            return fail("The microphone is busy")
        }
        upload = newUpload
        capture = newCapture
        deliver = onText
        startedAt = SystemClock.elapsedRealtime()
        _state.value = DictationState.Listening(startedAt)
        return true
    }

    /** Stops listening and transcribes what was said. */
    fun stop() {
        val currentUpload = upload ?: return
        val currentCapture = capture ?: return
        if (_state.value !is DictationState.Listening) return
        _level.value = 0f
        capture = null
        if (SystemClock.elapsedRealtime() - startedAt < MIN_RECORDING_MS) {
            currentCapture.stop { saveDebugAudio() }
            cancel()
            return
        }
        val stoppedAt = SystemClock.elapsedRealtime()
        _state.value = DictationState.Transcribing
        currentCapture.stop {
            // On the audio thread, after its last frame, so the upload never ends before the tail.
            saveDebugAudio()
            if (currentCapture.heardSignal) {
                currentUpload.finish()
            } else {
                scope.launch {
                    if (upload !== currentUpload) return@launch
                    currentUpload.cancel()
                    upload = null
                    fail("The microphone gave silence. Open Fairspoken to re-arm it")
                }
            }
        }
        currentUpload.onResult { result ->
            scope.launch {
                if (upload !== currentUpload) return@launch
                upload = null
                result.fold(
                    onSuccess = { transcript ->
                        val engine = when (transcript.superMode) {
                            "used" -> "super mode"
                            else -> transcript.backend.ifBlank { "host" }
                        }
                        val stats = DictationStats(transcript.audioSeconds, SystemClock.elapsedRealtime() - stoppedAt, engine)
                        if (transcript.text.isBlank()) {
                            fail("Didn't catch that")
                        } else {
                            _state.value = DictationState.Done(transcript.text, stats)
                            deliver?.invoke(transcript.text)
                            settle()
                        }
                    },
                    onFailure = { fail(it.message ?: "Transcription failed") },
                )
            }
        }
    }

    fun toggle(onText: (String) -> Unit) {
        when (_state.value) {
            is DictationState.Listening -> stop()
            is DictationState.Transcribing -> Unit
            else -> start(onText)
        }
    }

    fun cancel() {
        capture?.stop()
        capture = null
        upload?.cancel()
        upload = null
        _level.value = 0f
        _state.value = DictationState.Idle
    }

    private fun saveDebugAudio() {
        scope.launch(Dispatchers.IO) { DebugAudio.finish(context) }
    }

    private fun fail(message: String): Boolean {
        _state.value = DictationState.Failed(message)
        settle(3_000)
        return false
    }

    /** Results and errors stay on screen briefly, then everything goes back to idle. */
    private fun settle(afterMs: Long = 1_600) {
        settleJob?.cancel()
        settleJob = scope.launch {
            delay(afterMs)
            if (!isBusy) _state.value = DictationState.Idle
        }
    }

    private companion object {
        const val MIN_RECORDING_MS = 300L
    }
}
