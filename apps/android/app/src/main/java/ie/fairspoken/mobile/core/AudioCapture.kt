package ie.fairspoken.mobile.core

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Process
import kotlin.math.log10
import kotlin.math.sqrt

/**
 * 16 kHz mono PCM16 from the microphone on a dedicated audio thread, in
 * 100 ms frames: the format the host's engines want, so nothing is resampled.
 */
class AudioCapture(
    /** Debug only: a 16 kHz mono PCM16 WAV played in real time instead of the microphone. */
    private val replay: java.io.File? = null,
    private val onFrame: (samples: ShortArray, count: Int) -> Unit,
    private val onLevel: (Float) -> Unit,
    /** The microphone stopped delivering mid-recording (another app took it, the service died). */
    private val onFailure: () -> Unit = {},
) {
    @Volatile private var running = false
    private val lock = Any()
    /** False while an audio thread runs; [stop]'s callback waits for it. */
    private var exited = true
    private var onStopped: (() -> Unit)? = null

    /** True once a frame with any signal arrived; all-zero audio means the OS muted us. */
    @Volatile var heardSignal = false
        private set

    @SuppressLint("MissingPermission") // Callers check RECORD_AUDIO first.
    fun start(): Boolean {
        if (replay != null) return startReplay(replay)
        val minBuffer = AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuffer <= 0) return false
        val record = runCatching {
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_RECOGNITION,
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                maxOf(minBuffer, FRAME_SAMPLES * 2 * 4),
            )
        }.getOrNull() ?: return false
        if (record.state != AudioRecord.STATE_INITIALIZED) {
            record.release()
            return false
        }
        // Another app holding the microphone shows up here, not as an exception later.
        val recording = runCatching { record.startRecording() }.isSuccess &&
            record.recordingState == AudioRecord.RECORDSTATE_RECORDING
        if (!recording) {
            runCatching { record.stop() }
            record.release()
            return false
        }
        running = true
        heardSignal = false
        synchronized(lock) { exited = false }
        Thread({
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
            val buffer = ShortArray(FRAME_SAMPLES)
            var failed = false
            try {
                while (running) {
                    val read = record.read(buffer, 0, buffer.size)
                    // Negative is an error code (dead object, invalid operation): it won't recover.
                    if (read < 0) {
                        failed = true
                        break
                    }
                    if (read == 0) continue
                    onFrame(buffer, read)
                    onLevel(level(buffer, read))
                }
            } finally {
                runCatching { record.stop() }
                record.release()
                exit()
            }
            if (failed && running) onFailure()
        }, "fairspoken-mic").apply { start() }
        return true
    }

    private fun startReplay(file: java.io.File): Boolean {
        val bytes = runCatching { file.readBytes() }.getOrNull() ?: return false
        val pcm = java.nio.ByteBuffer.wrap(bytes, 44, bytes.size - 44).order(java.nio.ByteOrder.LITTLE_ENDIAN).asShortBuffer()
        running = true
        heardSignal = false
        synchronized(lock) { exited = false }
        Thread({
            val buffer = ShortArray(FRAME_SAMPLES)
            try {
                while (running) {
                    val count = minOf(FRAME_SAMPLES, pcm.remaining())
                    pcm.get(buffer, 0, count)
                    buffer.fill(0, count, FRAME_SAMPLES)
                    onFrame(buffer, FRAME_SAMPLES)
                    onLevel(level(buffer, FRAME_SAMPLES))
                    Thread.sleep(100)
                }
            } finally {
                exit()
            }
        }, "fairspoken-replay").apply { start() }
        return true
    }

    /**
     * Stops after the frame being read, so the tail of the last word is kept.
     * Doesn't wait: [then] runs on the audio thread once that frame has been
     * handed to `onFrame`, or right away if the thread has already ended.
     */
    fun stop(then: () -> Unit = {}) {
        running = false
        val runNow = synchronized(lock) {
            if (!exited) onStopped = then
            exited
        }
        if (runNow) then()
    }

    private fun exit() {
        val then = synchronized(lock) {
            exited = true
            onStopped.also { onStopped = null }
        }
        then?.invoke()
    }

    private fun level(buffer: ShortArray, count: Int): Float {
        var sum = 0.0
        for (i in 0 until count) {
            val s = buffer[i].toDouble()
            sum += s * s
        }
        val rms = sqrt(sum / count)
        if (rms > 0) heardSignal = true
        val db = 20 * log10((rms / 32768.0).coerceAtLeast(1e-6))
        return ((db + 55) / 45).toFloat().coerceIn(0f, 1f)
    }

    companion object {
        const val SAMPLE_RATE = 16_000
        const val FRAME_SAMPLES = 1_600
    }
}
