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
 * speaks; stopping only closes the upload. The surface that starts a
 * dictation owns it: only it can stop or cancel it.
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
    private var owner: Any? = null
    private var deliver: ((String) -> Unit)? = null
    private var startedAt = 0L
    private var stoppedAt = 0L
    private var settleJob: Job? = null
    private var limitJob: Job? = null

    val isBusy: Boolean
        get() = _state.value is DictationState.Listening || _state.value is DictationState.Transcribing

    val isListening: Boolean
        get() = _state.value is DictationState.Listening

    fun isListeningFor(owner: Any): Boolean = isListening && this.owner === owner

    /** Starts listening for [owner]; [onText] gets the transcript if the dictation succeeds. */
    fun start(owner: Any, onText: (String) -> Unit): Boolean {
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
                    cancelSession()
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
        this.owner = owner
        deliver = onText
        startedAt = SystemClock.elapsedRealtime()
        _state.value = DictationState.Listening(startedAt)
        // Heard from the start: the host may refuse (busy, token, too long) while the user still speaks.
        newUpload.onResult { result -> scope.launch { onResult(newUpload, result) } }
        // Stop just short of the host's limit, so what was said is transcribed instead of refused.
        client.recordingLimitSeconds(host.url)?.let { limit ->
            limitJob = scope.launch {
                delay(limit * 1_000L - LIMIT_MARGIN_MS)
                if (upload === newUpload) finishListening()
            }
        }
        return true
    }

    /** Stops listening and transcribes what was said, if [owner] started it. */
    fun stop(owner: Any) {
        if (this.owner === owner) finishListening()
    }

    fun toggle(owner: Any, onText: (String) -> Unit) {
        when (_state.value) {
            is DictationState.Listening -> stop(owner)
            is DictationState.Transcribing -> Unit
            else -> start(owner, onText)
        }
    }

    /** Drops the dictation [owner] started, listening or transcribing. */
    fun cancel(owner: Any) {
        if (this.owner === owner && isBusy) cancelSession()
    }

    private fun finishListening() {
        val currentUpload = upload ?: return
        val currentCapture = capture ?: return
        if (_state.value !is DictationState.Listening) return
        limitJob?.cancel()
        _level.value = 0f
        capture = null
        if (SystemClock.elapsedRealtime() - startedAt < MIN_RECORDING_MS) {
            currentCapture.stop { saveDebugAudio() }
            cancelSession()
            return
        }
        stoppedAt = SystemClock.elapsedRealtime()
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
    }

    private fun onResult(from: StreamingUpload, result: Result<Transcript>) {
        if (upload !== from) return
        upload = null
        // Answered while still listening: the host refused mid-recording, so the microphone stops now.
        capture?.let { early ->
            capture = null
            _level.value = 0f
            stoppedAt = SystemClock.elapsedRealtime()
            early.stop { saveDebugAudio() }
        }
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
                    val onText = deliver
                    endSession()
                    _state.value = DictationState.Done(transcript.text, stats)
                    onText?.invoke(transcript.text)
                    settle()
                }
            },
            onFailure = { fail(it.message ?: "Transcription failed") },
        )
    }

    private fun cancelSession() {
        capture?.stop()
        capture = null
        upload?.cancel()
        upload = null
        _level.value = 0f
        endSession()
        _state.value = DictationState.Idle
    }

    /** The session is over: nobody owns what comes next, and no stale callback is kept. */
    private fun endSession() {
        limitJob?.cancel()
        owner = null
        deliver = null
    }

    private fun saveDebugAudio() {
        scope.launch(Dispatchers.IO) { DebugAudio.finish(context) }
    }

    private fun fail(message: String): Boolean {
        endSession()
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
        /** The host's `maxRecordingSeconds` is at least 10, so this always leaves time to talk. */
        const val LIMIT_MARGIN_MS = 1_000L
    }
}
