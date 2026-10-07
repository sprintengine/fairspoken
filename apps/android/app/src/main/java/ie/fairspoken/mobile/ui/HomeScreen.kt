package ie.fairspoken.mobile.ui

import android.os.Build
import android.provider.Settings
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.windowInsetsTopHeight
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.kyant.backdrop.Backdrop
import com.kyant.backdrop.backdrops.layerBackdrop
import com.kyant.backdrop.backdrops.rememberLayerBackdrop
import ie.fairspoken.mobile.FairspokenApp
import ie.fairspoken.mobile.core.DictationState
import ie.fairspoken.mobile.core.FoundHost
import ie.fairspoken.mobile.core.PairOutcome
import ie.fairspoken.mobile.core.SavedHost
import ie.fairspoken.mobile.core.Tailnet
import ie.fairspoken.mobile.core.VocabularyPacks
import kotlinx.coroutines.launch

/** What the system has granted, re-read whenever the screen resumes. */
data class Readiness(
    val mic: Boolean = false,
    val keyboardEnabled: Boolean = false,
    val keyboardSelected: Boolean = false,
    val overlay: Boolean = false,
    val insertService: Boolean = false,
    val dockRunning: Boolean = false,
)

class HomeActions(
    val requestMic: () -> Unit,
    val openKeyboardSettings: () -> Unit,
    val pickKeyboard: () -> Unit,
    val openOverlaySettings: () -> Unit,
    val openAccessibilitySettings: () -> Unit,
    val setDock: (Boolean) -> Unit,
)

@Composable
fun HomeScreen(app: FairspokenApp, readiness: Readiness, refreshTick: Int, actions: HomeActions) {
    val backdrop = rememberLayerBackdrop()
    Box(Modifier.fillMaxSize()) {
        Aurora(Modifier.fillMaxSize().layerBackdrop(backdrop))
        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .statusBarsPadding()
                .navigationBarsPadding()
                .imePadding()
                .padding(horizontal = 18.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Header()
            HostsCard(app, backdrop, refreshTick)
            TryCard(app, backdrop, readiness, actions)
            EverywhereCard(app, backdrop, readiness, actions)
            OptionsCard(app, backdrop)
            Spacer(Modifier.height(24.dp))
        }
        // Content scrolls under the status bar; fade it so the clock stays readable.
        Column(Modifier.fillMaxWidth()) {
            Box(Modifier.fillMaxWidth().windowInsetsTopHeight(WindowInsets.statusBars).background(Color(0xF2F1F4F3)))
            Box(
                Modifier
                    .fillMaxWidth()
                    .height(20.dp)
                    .background(Brush.verticalGradient(listOf(Color(0xF2F1F4F3), Color(0x00F1F4F3)))),
            )
        }
    }
}

@Composable
private fun Header() {
    Column(Modifier.padding(start = 6.dp, top = 12.dp, bottom = 4.dp)) {
        BasicText("Fairspoken", style = Type.title)
        BasicText("Your own transcription server, in every app on this phone.", style = Type.body)
    }
}

@Composable
private fun HostsCard(app: FairspokenApp, backdrop: Backdrop, refreshTick: Int) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val hosts by app.store.hosts.collectAsState()
    val activeUrl by app.store.activeUrl.collectAsState()
    val tailnet = remember(refreshTick) { Tailnet.status(context) }
    val latency = remember { mutableStateMapOf<String, Result<Long>>() }
    var query by remember { mutableStateOf("") }
    var searching by remember { mutableStateOf(false) }
    var found by remember { mutableStateOf<FoundHost?>(null) }
    var notFound by remember { mutableStateOf<String?>(null) }
    var password by remember { mutableStateOf("") }
    var pairing by remember { mutableStateOf(false) }
    var pairError by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(hosts, refreshTick) {
        hosts.forEach { host -> launch { latency[host.url] = app.client.ping(host) } }
    }

    fun search() {
        if (query.isBlank() || searching) return
        searching = true
        found = null
        notFound = null
        pairError = null
        scope.launch {
            val result = app.client.find(query, tailnet.domain)
            searching = false
            if (result == null) notFound = "No Fairspoken host answered at \"${query.trim()}\""
            found = result
        }
    }

    fun pair(target: FoundHost) {
        pairing = true
        pairError = null
        scope.launch {
            val deviceName = Settings.Global.getString(context.contentResolver, "device_name") ?: Build.MODEL
            when (val outcome = app.client.pair(target, password, deviceName)) {
                is PairOutcome.Paired -> {
                    app.store.save(outcome.host)
                    found = null
                    query = ""
                    password = ""
                }
                is PairOutcome.Failed -> pairError = outcome.message
            }
            pairing = false
        }
    }

    GlassCard(backdrop, Modifier.fillMaxWidth()) {
        Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                BasicText("Hosts", style = Type.heading, modifier = Modifier.weight(1f))
                Dot(if (tailnet.connected) Ink.ok else Ink.muted)
                Spacer(Modifier.width(6.dp))
                BasicText(if (tailnet.connected) "Tailscale on" else "Tailscale off", style = Type.caption)
            }
            if (!tailnet.connected) {
                BasicText("Turn on Tailscale to reach hosts on your tailnet.", style = Type.caption)
            }
            hosts.forEach { host ->
                HostRow(host, host.url == (activeUrl ?: hosts.firstOrNull()?.url), latency[host.url]) {
                    app.store.select(host.url)
                }
            }
            if (hosts.isEmpty()) {
                BasicText(
                    "Type the machine name of a computer running the Fairspoken host, as it appears in Tailscale.",
                    style = Type.body,
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                GlassField(
                    query, { query = it; notFound = null }, "Machine name, e.g. studio-mac", backdrop,
                    Modifier.weight(1f), onDone = ::search,
                )
                Spacer(Modifier.width(10.dp))
                GlassButton(if (searching) "…" else "Find", backdrop, enabled = !searching, onClick = ::search)
            }
            notFound?.let { BasicText(it, style = Type.caption.copy(color = Ink.warn)) }
            found?.let { target ->
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    BasicText(
                        "Found ${target.hello.name} · v${target.hello.serverVersion}\n${target.url}",
                        style = Type.body,
                    )
                    when (target.hello.auth) {
                        "password" -> Row(verticalAlignment = Alignment.CenterVertically) {
                            GlassField(
                                password, { password = it }, "Pairing password", backdrop,
                                Modifier.weight(1f), password = true, onDone = { pair(target) },
                            )
                            Spacer(Modifier.width(10.dp))
                            GlassButton(
                                if (pairing) "…" else "Pair", backdrop,
                                tint = Ink.accent.copy(alpha = 0.35f), enabled = !pairing && password.length >= 6,
                            ) { pair(target) }
                        }
                        "none" -> GlassButton("Connect", backdrop, tint = Ink.accent.copy(alpha = 0.35f)) {
                            app.store.save(SavedHost(target.url, target.hello.name, null))
                            found = null
                        }
                        else -> BasicText(
                            "This host has a token but no pairing password. Set one in its dashboard's Pairing card.",
                            style = Type.caption.copy(color = Ink.warn),
                        )
                    }
                    pairError?.let { BasicText(it, style = Type.caption.copy(color = Ink.warn)) }
                }
            }
        }
    }
}

@Composable
private fun HostRow(host: SavedHost, active: Boolean, latency: Result<Long>?, onSelect: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onSelect).padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Dot(
            when {
                latency == null -> Ink.faint
                latency.isSuccess -> Ink.ok
                else -> Ink.muted
            },
        )
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            BasicText(host.name, style = Type.label)
            BasicText(
                latency?.fold({ "${it} ms · ${host.url}" }, { it.message ?: "Offline" }) ?: host.url,
                style = Type.caption,
            )
        }
        if (active) BasicText("In use", style = Type.caption.copy(color = Ink.accent))
    }
}

@Composable
private fun TryCard(app: FairspokenApp, backdrop: Backdrop, readiness: Readiness, actions: HomeActions) {
    val state by app.dictation.state.collectAsState()
    val level by app.dictation.level.collectAsState()
    var transcript by remember { mutableStateOf("") }
    var stats by remember { mutableStateOf("") }
    LaunchedEffect(state) {
        (state as? DictationState.Done)?.let {
            stats = "%.1f s of audio · text %d ms after you stopped · %s".format(
                it.stats.audioSeconds, it.stats.latencyMs, it.stats.engine,
            )
        }
    }
    GlassCard(backdrop, Modifier.fillMaxWidth()) {
        Column(
            Modifier.fillMaxWidth(),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            BasicText("Try it", style = Type.heading, modifier = Modifier.fillMaxWidth())
            MicOrb(state, level, backdrop, 116.dp) {
                if (!readiness.mic) actions.requestMic()
                else app.dictation.toggle { text -> transcript = text }
            }
            BasicText(
                when (val s = state) {
                    is DictationState.Listening -> "Listening… tap to finish"
                    DictationState.Transcribing -> "Transcribing on your host"
                    is DictationState.Failed -> s.message
                    else -> if (transcript.isEmpty()) "Tap and speak" else ""
                },
                style = Type.caption.copy(textAlign = TextAlign.Center),
            )
            if (transcript.isNotEmpty()) {
                BasicText(transcript, style = Type.transcript, modifier = Modifier.fillMaxWidth())
                BasicText(stats, style = Type.caption, modifier = Modifier.fillMaxWidth())
            }
        }
    }
}

@Composable
private fun EverywhereCard(app: FairspokenApp, backdrop: Backdrop, readiness: Readiness, actions: HomeActions) {
    val settings by app.store.settings.collectAsState()
    GlassCard(backdrop, Modifier.fillMaxWidth()) {
        Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
            BasicText("Use it in every app", style = Type.heading)

            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        BasicText("Voice keyboard", style = Type.label)
                        BasicText(
                            "Switch to it with the keyboard button, speak, and it types. Needs nothing else.",
                            style = Type.caption,
                        )
                    }
                    Spacer(Modifier.width(10.dp))
                    if (!readiness.keyboardEnabled) {
                        GlassButton("Enable", backdrop, onClick = actions.openKeyboardSettings)
                    } else {
                        GlassButton(if (readiness.keyboardSelected) "In use" else "Switch", backdrop, onClick = actions.pickKeyboard)
                    }
                }
            }

            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        BasicText("Floating dock", style = Type.label)
                        BasicText(
                            "Keep your own keyboard. Tap the bubble, or hold to talk, and the text lands where your cursor is.",
                            style = Type.caption,
                        )
                    }
                    Spacer(Modifier.width(10.dp))
                    GlassSwitch(readiness.dockRunning, { on ->
                        if (on && !readiness.mic) actions.requestMic()
                        else if (on && !readiness.overlay) actions.openOverlaySettings()
                        else actions.setDock(on)
                    })
                }
                Requirement("Show over other apps", readiness.overlay, "Allow", backdrop, actions.openOverlaySettings)
                Requirement(
                    "Type into other apps (accessibility)", readiness.insertService, "Turn on", backdrop,
                    actions.openAccessibilitySettings,
                )
                if (!readiness.insertService) {
                    BasicText(
                        "Without it the dock copies the text for you to paste. Sideloaded? First open App info › ⋮ › Allow restricted settings.",
                        style = Type.caption,
                    )
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    BasicText("Only while a keyboard is open", style = Type.body, modifier = Modifier.weight(1f))
                    GlassSwitch(settings.dockOnlyWhileTyping, { on ->
                        app.store.updateSettings { it.copy(dockOnlyWhileTyping = on) }
                    })
                }
            }
        }
    }
}

@Composable
private fun Requirement(label: String, met: Boolean, action: String, backdrop: Backdrop, onClick: () -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Dot(if (met) Ink.ok else Ink.muted)
        Spacer(Modifier.width(10.dp))
        BasicText(label, style = Type.body, modifier = Modifier.weight(1f))
        if (!met) GlassButton(action, backdrop, onClick = onClick)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun OptionsCard(app: FairspokenApp, backdrop: Backdrop) {
    val settings by app.store.settings.collectAsState()
    GlassCard(backdrop, Modifier.fillMaxWidth()) {
        Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
            BasicText("Vocabulary", style = Type.heading)
            BasicText("Terms the host should spell your way.", style = Type.caption)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                GlassChip("Everyday", settings.packId == null, backdrop) {
                    app.store.updateSettings { it.copy(packId = null) }
                }
                VocabularyPacks.all.forEach { pack ->
                    GlassChip(pack.name, settings.packId == pack.id, backdrop) {
                        app.store.updateSettings { it.copy(packId = pack.id) }
                    }
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    BasicText("Super mode", style = Type.label)
                    BasicText("Ask the host to run Parakeet and Whisper together when it has room.", style = Type.caption)
                }
                GlassSwitch(settings.superMode, { on -> app.store.updateSettings { it.copy(superMode = on) } })
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    BasicText("Listen when the keyboard opens", style = Type.label)
                    BasicText("The voice keyboard starts recording straight away.", style = Type.caption)
                }
                GlassSwitch(settings.autoListen, { on -> app.store.updateSettings { it.copy(autoListen = on) } })
            }
        }
    }
}
