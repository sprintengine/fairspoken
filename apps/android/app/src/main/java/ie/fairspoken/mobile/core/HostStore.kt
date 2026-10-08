package ie.fairspoken.mobile.core

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import org.json.JSONArray
import org.json.JSONObject

/** A paired transcription host. `token` is null for a host without auth. */
data class SavedHost(val url: String, val name: String, val token: String?)

data class MobileSettings(
    /**
     * Ask the host for super mode (Parakeet + Whisper merged). Off by default
     * on the phone: it waits for Whisper, seconds instead of under one.
     */
    val superMode: Boolean = false,
    /** Vocabulary pack whose always-on terms go to the host as hints. */
    val packId: String? = null,
    /** The voice keyboard starts listening as soon as it opens. */
    val autoListen: Boolean = true,
    /** With the insert service on, show the dock only while a keyboard is open. */
    val dockOnlyWhileTyping: Boolean = true,
    val dockX: Int = Int.MIN_VALUE,
    val dockY: Int = Int.MIN_VALUE,
)

/** Hosts and settings, kept in private app preferences. */
class HostStore(context: Context) {
    private val prefs = context.getSharedPreferences("fairspoken", Context.MODE_PRIVATE)

    private val _hosts = MutableStateFlow(loadHosts())
    val hosts: StateFlow<List<SavedHost>> = _hosts.asStateFlow()

    private val _activeUrl = MutableStateFlow(prefs.getString("activeUrl", null))

    private val _settings = MutableStateFlow(loadSettings())
    val settings: StateFlow<MobileSettings> = _settings.asStateFlow()

    /** The selected host, or the first one when the selection is gone. */
    private val _activeHost = MutableStateFlow(resolveActive())
    val activeHost: StateFlow<SavedHost?> = _activeHost.asStateFlow()

    val active: SavedHost?
        get() = _activeHost.value

    fun save(host: SavedHost) {
        _hosts.update { list -> list.filterNot { it.url == host.url } + host }
        persistHosts()
        select(host.url)
    }

    fun remove(url: String) {
        _hosts.update { list -> list.filterNot { it.url == url } }
        persistHosts()
        if (_activeUrl.value == url) select(_hosts.value.firstOrNull()?.url) else _activeHost.value = resolveActive()
    }

    fun select(url: String?) {
        _activeUrl.value = url
        _activeHost.value = resolveActive()
        prefs.edit().putString("activeUrl", url).apply()
    }

    private fun resolveActive(): SavedHost? =
        _hosts.value.firstOrNull { it.url == _activeUrl.value } ?: _hosts.value.firstOrNull()

    fun updateSettings(transform: (MobileSettings) -> MobileSettings) {
        val next = transform(_settings.value)
        _settings.value = next
        prefs.edit()
            .putBoolean("superMode", next.superMode)
            .putString("packId", next.packId)
            .putBoolean("autoListen", next.autoListen)
            .putBoolean("dockOnlyWhileTyping", next.dockOnlyWhileTyping)
            .putInt("dockX", next.dockX)
            .putInt("dockY", next.dockY)
            .apply()
    }

    private fun loadSettings() = MobileSettings(
        superMode = prefs.getBoolean("superMode", false),
        packId = prefs.getString("packId", null),
        autoListen = prefs.getBoolean("autoListen", true),
        dockOnlyWhileTyping = prefs.getBoolean("dockOnlyWhileTyping", true),
        dockX = prefs.getInt("dockX", Int.MIN_VALUE),
        dockY = prefs.getInt("dockY", Int.MIN_VALUE),
    )

    private fun loadHosts(): List<SavedHost> {
        val raw = prefs.getString("hosts", null) ?: return emptyList()
        return runCatching {
            val array = JSONArray(raw)
            (0 until array.length()).map { i ->
                val o = array.getJSONObject(i)
                SavedHost(
                    url = o.getString("url"),
                    name = o.optString("name", o.getString("url")),
                    token = if (o.isNull("token")) null else o.getString("token"),
                )
            }
        }.getOrDefault(emptyList())
    }

    private fun persistHosts() {
        val array = JSONArray()
        _hosts.value.forEach { host ->
            array.put(
                JSONObject()
                    .put("url", host.url)
                    .put("name", host.name)
                    .put("token", host.token ?: JSONObject.NULL)
            )
        }
        prefs.edit().putString("hosts", array.toString()).apply()
    }
}
