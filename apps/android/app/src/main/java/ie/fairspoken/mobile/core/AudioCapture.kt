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
) {
    @Volatile private var running = false
    private var thread: Thread? = null

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
        running = true
        heardSignal = false
        thread = Thread({
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
            val buffer = ShortArray(FRAME_SAMPLES)
            try {
                record.startRecording()
                while (running) {
                    val read = record.read(buffer, 0, buffer.size)
                    if (read <= 0) continue
                    onFrame(buffer, read)
                    onLevel(level(buffer, read))
                }
            } finally {
                runCatching { record.stop() }
                record.release()
            }
        }, "fairspoken-mic").apply { start() }
        return true
    }

    private fun startReplay(file: java.io.File): Boolean {
        val bytes = runCatching { file.readBytes() }.getOrNull() ?: return false
        val pcm = java.nio.ByteBuffer.wrap(bytes, 44, bytes.size - 44).order(java.nio.ByteOrder.LITTLE_ENDIAN).asShortBuffer()
        running = true
        heardSignal = false
        thread = Thread({
            val buffer = ShortArray(FRAME_SAMPLES)
            while (running) {
                val count = minOf(FRAME_SAMPLES, pcm.remaining())
                pcm.get(buffer, 0, count)
                buffer.fill(0, count, FRAME_SAMPLES)
                onFrame(buffer, FRAME_SAMPLES)
                onLevel(level(buffer, FRAME_SAMPLES))
                Thread.sleep(100)
            }
        }, "fairspoken-replay").apply { start() }
        return true
    }

    /** Stops after the frame being read, so the tail of the last word is kept. */
    fun stop() {
        running = false
        thread?.join(400)
        thread = null
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
