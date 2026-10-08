package ie.fairspoken.mobile.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.withContext
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okio.BufferedSink
import org.json.JSONObject
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/** `GET /v1/hello`: who a host is, before pairing. */
data class HostHello(val name: String, val serverVersion: String, val auth: String)

data class FoundHost(val url: String, val hello: HostHello)

sealed interface PairOutcome {
    data class Paired(val host: SavedHost) : PairOutcome
    data class Failed(val message: String) : PairOutcome
}

data class Transcript(
    val text: String,
    val audioSeconds: Double,
    val backend: String,
    val model: String,
    val superMode: String?,
)

/** [rejected]: the host refused the token, so the fix is to pair again. */
class HostException(message: String, val rejected: Boolean = false) : Exception(message)

/**
 * Speaks the Fairspoken host protocol (src-tauri/src/host/PROTOCOL.md):
 * discovery, pairing, health and streamed transcription.
 */
class HostClient {
    /** One pool for every request, so a warmed-up connection is reused by the next dictation. */
    val http: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(4, TimeUnit.SECONDS)
        .readTimeout(90, TimeUnit.SECONDS)
        .writeTimeout(30, TimeUnit.SECONDS)
        .connectionPool(okhttp3.ConnectionPool(4, 5, TimeUnit.MINUTES))
        .build()

    private val probeHttp: OkHttpClient = http.newBuilder()
        .callTimeout(1500, TimeUnit.MILLISECONDS)
        .build()

    private val recordingLimits = ConcurrentHashMap<String, Int>()

    suspend fun hello(baseUrl: String): HostHello? = withContext(Dispatchers.IO) {
        runCatching {
            probeHttp.newCall(Request.Builder().url("$baseUrl/v1/hello").build()).execute().use { response ->
                if (!response.isSuccessful) return@use null
                val json = JSONObject(response.body?.string().orEmpty())
                if (json.optString("service") != "fairspoken-host") return@use null
                HostHello(
                    name = json.optString("name", baseUrl),
                    serverVersion = json.optString("serverVersion"),
                    auth = json.optString("auth", "token"),
                )
            }
        }.getOrNull()
    }

    /**
     * Finds a host by machine name, `name:port`, IP or URL, probing the URLs
     * the protocol lists in order and keeping the first that answers.
     */
    suspend fun find(input: String, tailnetDomain: String?): FoundHost? {
        val candidates = candidateUrls(input, tailnetDomain)
        if (candidates.isEmpty()) return null
        return coroutineScope {
            val probes = candidates.map { url -> async { hello(url)?.let { FoundHost(url, it) } } }
            probes.firstNotNullOfOrNull { it.await() }
        }
    }

    suspend fun pair(found: FoundHost, password: String, clientName: String): PairOutcome =
        withContext(Dispatchers.IO) {
            val body = JSONObject().put("password", password).put("clientName", clientName)
                .toString().toRequestBody(JSON)
            val request = Request.Builder().url("${found.url}/v1/pair").post(body).build()
            try {
                http.newCall(request).execute().use { response ->
                    val json = runCatching { JSONObject(response.body?.string().orEmpty()) }.getOrNull()
                    when (response.code) {
                        200 -> {
                            val token = json?.takeUnless { it.isNull("token") }?.optString("token")
                            val name = json?.optString("name")?.takeIf { it.isNotBlank() } ?: found.hello.name
                            PairOutcome.Paired(SavedHost(found.url, name, token))
                        }
                        401 -> PairOutcome.Failed("Wrong password")
                        404 -> PairOutcome.Failed("Pairing is turned off on this host")
                        429 -> PairOutcome.Failed(
                            "Too many attempts, try again in ${json?.optInt("retryAfterSeconds")?.takeIf { it > 0 } ?: 60} s"
                        )
                        else -> PairOutcome.Failed(
                            json?.optString("error")?.takeIf { it.isNotBlank() } ?: "Host answered ${response.code}"
                        )
                    }
                }
            } catch (e: IOException) {
                PairOutcome.Failed("Can't reach ${found.hello.name}")
            }
        }

    /** Round trip to an authenticated route; also warms the connection for the next dictation. */
    suspend fun ping(host: SavedHost): Result<Long> = withContext(Dispatchers.IO) {
        runCatching {
            val started = System.nanoTime()
            val request = Request.Builder().url("${host.url}/v1/health").authorized(host).build()
            http.newCall(request).execute().use { response ->
                if (response.code == 401) throw HostException("Token rejected, pair again", rejected = true)
                if (!response.isSuccessful) throw HostException("Host answered ${response.code}")
            }
            (System.nanoTime() - started) / 1_000_000
        }
    }

    /**
     * Reads the host's `maxRecordingSeconds` from `GET /v1/stats`, so a
     * dictation can stop before the host refuses it. Also warms the connection.
     */
    suspend fun learnLimits(host: SavedHost) = withContext(Dispatchers.IO) {
        runCatching {
            val request = Request.Builder().url("${host.url}/v1/stats").authorized(host).build()
            http.newCall(request).execute().use { response ->
                if (!response.isSuccessful) return@use
                val seconds = JSONObject(response.body?.string().orEmpty()).optInt("maxRecordingSeconds")
                if (seconds > 0) recordingLimits[host.url] = seconds
            }
        }
    }

    /** The host's recording limit, once [learnLimits] has read it. */
    fun recordingLimitSeconds(url: String): Int? = recordingLimits[url]

    /**
     * Opens `POST /v1/transcriptions/stream` right away, so the host queues
     * the job and decodes while the user is still speaking.
     */
    fun openStream(host: SavedHost, superMode: Boolean, hints: List<String>): StreamingUpload {
        val upload = StreamingUpload()
        val request = Request.Builder()
            .url("${host.url}/v1/transcriptions/stream")
            .authorized(host)
            .header("x-fairspoken-client", "fairspoken-android")
            .header("x-fairspoken-language", "en")
            .apply {
                if (hints.isNotEmpty()) {
                    val json = org.json.JSONArray(hints).toString()
                    header("x-fairspoken-vocabulary-hints", percentEncode(json))
                }
                if (superMode) header("x-fairspoken-super-mode", "1")
            }
            .post(upload.body)
            .build()
        upload.call = http.newCall(request).also { it.enqueue(upload) }
        return upload
    }

    private fun Request.Builder.authorized(host: SavedHost) = apply {
        host.token?.let { header("Authorization", "Bearer $it") }
    }

    companion object {
        const val DEFAULT_PORT = 48173
        private val JSON = "application/json".toMediaType()
        private val IPV4 = Regex("""\d{1,3}(\.\d{1,3}){3}""")

        fun candidateUrls(input: String, tailnetDomain: String?): List<String> {
            val raw = input.trim().trimEnd('/')
            if (raw.isEmpty()) return emptyList()
            if (raw.startsWith("http://") || raw.startsWith("https://")) return listOf(raw)
            // IPv6 literals need brackets in a URL: `[fd7a::1]:port`, or bare `fd7a::1`.
            if (raw.startsWith("[")) return listOf(if (raw.contains("]:")) "http://$raw" else "http://$raw:$DEFAULT_PORT")
            if (raw.count { it == ':' } > 1) return listOf("http://[$raw]:$DEFAULT_PORT")
            if (raw.contains(':')) return listOf("http://$raw")
            if (IPV4.matches(raw)) return listOf("http://$raw:$DEFAULT_PORT")
            // `tailscale serve` certificates only cover the full *.ts.net name.
            val fqdn = when {
                raw.contains('.') -> raw.trimEnd('.')
                tailnetDomain != null -> "$raw.$tailnetDomain"
                else -> null
            }
            return buildList {
                if (fqdn != null) {
                    add("https://$fqdn")
                    add("http://$fqdn")
                    add("http://$fqdn:$DEFAULT_PORT")
                }
                add("http://$raw")
                add("http://$raw:$DEFAULT_PORT")
            }.distinct()
        }

        /** Same as the desktop client: only RFC 3986 unreserved bytes pass through. */
        fun percentEncode(input: String): String = buildString {
            for (byte in input.toByteArray(Charsets.UTF_8)) {
                val c = byte.toInt() and 0xFF
                if (c in 'A'.code..'Z'.code || c in 'a'.code..'z'.code || c in '0'.code..'9'.code ||
                    c == '-'.code || c == '_'.code || c == '.'.code || c == '~'.code
                ) {
                    append(c.toChar())
                } else {
                    append('%').append("0123456789ABCDEF"[c shr 4]).append("0123456789ABCDEF"[c and 0xF])
                }
            }
        }
    }
}

/**
 * The chunked request body of a streamed dictation: frames of
 * `u32 LE sample_rate | u32 LE sample_count | i16 LE samples`, written
 * and flushed as soon as the microphone produces them.
 */
class StreamingUpload : Callback {
    private val frames = LinkedBlockingQueue<ByteArray>()
    private val listeners = mutableListOf<(Result<Transcript>) -> Unit>()
    private var outcome: Result<Transcript>? = null
    @Volatile var call: Call? = null

    val body = object : RequestBody() {
        override fun contentType() = "application/vnd.fairspoken.pcm-stream".toMediaType()
        override fun isOneShot() = true
        override fun writeTo(sink: BufferedSink) {
            while (true) {
                val frame = frames.take()
                if (frame === END) break
                sink.write(frame)
                sink.flush()
            }
        }
    }

    fun send(sampleRate: Int, samples: ShortArray, count: Int) {
        if (count <= 0) return
        val frame = java.nio.ByteBuffer.allocate(8 + count * 2).order(java.nio.ByteOrder.LITTLE_ENDIAN)
        frame.putInt(sampleRate).putInt(count)
        for (i in 0 until count) frame.putShort(samples[i])
        frames.put(frame.array())
    }

    /** Ends the recording; the host answers with the transcript. */
    fun finish() {
        frames.put(END)
    }

    fun cancel() {
        frames.put(END)
        call?.cancel()
    }

    fun onResult(listener: (Result<Transcript>) -> Unit) {
        val done = synchronized(this) {
            outcome ?: run { listeners += listener; null }
        }
        done?.let(listener)
    }

    private fun complete(result: Result<Transcript>) {
        val toNotify = synchronized(this) {
            if (outcome != null) return
            outcome = result
            listeners.toList()
        }
        toNotify.forEach { it(result) }
    }

    override fun onFailure(call: Call, e: IOException) {
        complete(Result.failure(HostException(if (call.isCanceled()) "Cancelled" else "Can't reach the host")))
    }

    override fun onResponse(call: Call, response: Response) {
        response.use {
            val json = runCatching { JSONObject(it.body?.string().orEmpty()) }.getOrNull()
            if (!it.isSuccessful || json == null) {
                val message = when (it.code) {
                    401 -> "Token rejected, pair again"
                    413 -> "Recording too long for this host"
                    429 -> "Host is busy, try again"
                    else -> json?.optString("error")?.takeIf { e -> e.isNotBlank() } ?: "Host answered ${it.code}"
                }
                complete(Result.failure(HostException(message, rejected = it.code == 401)))
                return
            }
            complete(
                Result.success(
                    Transcript(
                        text = json.optString("text").trim(),
                        audioSeconds = json.optDouble("durationSeconds", 0.0),
                        backend = json.optString("backend"),
                        model = json.optString("model"),
                        superMode = json.optString("superMode").takeIf { s -> s.isNotBlank() },
                    )
                )
            )
        }
    }

    private companion object {
        val END = ByteArray(0)
    }
}
