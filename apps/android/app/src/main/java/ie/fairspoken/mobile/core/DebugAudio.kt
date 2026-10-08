package ie.fairspoken.mobile.core

import android.content.Context
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Off unless a file named `debug-audio` exists in the app's external files
 * folder (`adb shell touch /sdcard/Android/data/ie.fairspoken.mobile/files/debug-audio`).
 * Then the last recording, exactly as streamed, is kept there as `last.wav`,
 * and a `debug-input.wav` there (16 kHz mono PCM16) replaces the microphone,
 * so the whole path can be tested without speaking.
 */
object DebugAudio {
    @Volatile private var buffer: ByteArrayOutputStream? = null

    class Tap(private val out: ByteArrayOutputStream) {
        fun write(samples: ShortArray, count: Int) {
            val bytes = ByteBuffer.allocate(count * 2).order(ByteOrder.LITTLE_ENDIAN)
            for (i in 0 until count) bytes.putShort(samples[i])
            synchronized(out) { out.write(bytes.array()) }
        }
    }

    fun startIfEnabled(context: Context): Tap? {
        val dir = context.getExternalFilesDir(null) ?: return null
        if (!File(dir, "debug-audio").exists()) return null
        return ByteArrayOutputStream().also { buffer = it }.let(::Tap)
    }

    fun replayFile(context: Context): File? {
        val dir = context.getExternalFilesDir(null) ?: return null
        if (!File(dir, "debug-audio").exists()) return null
        return File(dir, "debug-input.wav").takeIf { it.exists() }
    }

    /** Writes `last.wav`; file I/O, so call it off the main thread. */
    fun finish(context: Context) {
        val out = buffer ?: return
        buffer = null
        val pcm = synchronized(out) { out.toByteArray() }
        val header = ByteBuffer.allocate(44).order(ByteOrder.LITTLE_ENDIAN).apply {
            put("RIFF".toByteArray()); putInt(36 + pcm.size); put("WAVE".toByteArray())
            put("fmt ".toByteArray()); putInt(16); putShort(1); putShort(1)
            putInt(AudioCapture.SAMPLE_RATE); putInt(AudioCapture.SAMPLE_RATE * 2); putShort(2); putShort(16)
            put("data".toByteArray()); putInt(pcm.size)
        }
        File(context.getExternalFilesDir(null), "last.wav").writeBytes(header.array() + pcm)
    }
}
