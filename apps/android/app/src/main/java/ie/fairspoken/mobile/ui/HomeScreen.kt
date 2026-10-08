package ie.fairspoken.mobile.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.windowInsetsTopHeight
import androidx.compose.ui.graphics.Brush
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshots.SnapshotStateMap
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.kyant.backdrop.Backdrop
import com.kyant.backdrop.backdrops.layerBackdrop
import com.kyant.backdrop.backdrops.rememberLayerBackdrop
import com.kyant.shapes.Capsule
import ie.fairspoken.mobile.FairspokenApp
import ie.fairspoken.mobile.core.DictationState
import ie.fairspoken.mobile.core.SavedHost
import ie.fairspoken.mobile.core.Tailnet
import ie.fairspoken.mobile.core.TailnetStatus
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
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
    /** Null when the Tailscale app isn't installed. */
    val openTailscale: (() -> Unit)?,
)

private enum class Route { Home, Settings, Connect }

/** The last dictation made on this screen, kept while you look around. */
private data class LastResult(val text: String, val audioSeconds: Double, val latencyMs: Long) : java.io.Serializable

/** How the host answered its last health check: null while checking. */
typealias HostHealth = SnapshotStateMap<String, Result<Long>>

fun Result<Long>.rejected() = exceptionOrNull()?.message?.startsWith("Token rejected") == true

@Composable
fun AppRoot(app: FairspokenApp, readiness: Readiness, refreshTick: Int, actions: HomeActions) {
    CrystalTheme {
        val context = LocalContext.current
        val backdrop = rememberLayerBackdrop()
        val hosts by app.store.hosts.collectAsState()
        val tailnet = remember(refreshTick) { Tailnet.status(context) }
        // A small back stack, saved across rotation and theme changes.
        var stack by rememberSaveable { mutableStateOf(Route.Home.name) }
        val routes = stack.split(',').map(Route::valueOf)
        fun push(route: Route) { stack = (routes + route).joinToString(",") { it.name } }
        fun pop() { stack = routes.dropLast(1).ifEmpty { listOf(Route.Home) }.joinToString(",") { it.name } }
        val firstRun = hosts.isEmpty()
        val screen = if (firstRun) Route.Connect else routes.last()

        val health = remember { mutableStateMapOf<String, Result<Long>>() }
        val lifecycle = LocalLifecycleOwner.current.lifecycle
        LaunchedEffect(hosts, refreshTick, lifecycle) {
            // Only while the screen is visible, and one round at a time so a slow host never stacks pings.
            lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                while (true) {
                    coroutineScope { hosts.forEach { host -> launch { health[host.url] = app.client.ping(host) } } }
                    delay(20_000)
                }
            }
        }

        BackHandler(enabled = !firstRun && routes.size > 1) { pop() }

        Box(Modifier.fillMaxSize()) {
            CrystalBackdrop(Modifier.fillMaxSize().layerBackdrop(backdrop))
            Crossfade(screen, animationSpec = tween(220), label = "route") { route ->
                when (route) {
                    Route.Home -> HomeScreen(app, backdrop, readiness, actions, health, tailnet) { push(Route.Settings) }
                    Route.Settings -> SettingsScreen(
                        app, backdrop, readiness, actions, health,
                        onBack = ::pop,
                        onAddHost = { push(Route.Connect) },
                    )
                    Route.Connect -> ConnectScreen(
                        app, backdrop, tailnet, actions.openTailscale,
                        onBack = if (firstRun) null else ::pop,
                        onConnected = { stack = Route.Home.name },
                    )
                }
            }
        }
    }
}

@Composable
private fun HomeScreen(
    app: FairspokenApp,
    backdrop: Backdrop,
    readiness: Readiness,
    actions: HomeActions,
    health: HostHealth,
    tailnet: TailnetStatus,
    onSettings: () -> Unit,
) {
    val state by app.dictation.state.collectAsState()
    // Read only while drawing, so the level doesn't recompose the screen.
    val level = app.dictation.level.collectAsState()
    val hosts by app.store.hosts.collectAsState()
    val activeUrl by app.store.activeUrl.collectAsState()
    val active = hosts.firstOrNull { it.url == activeUrl } ?: hosts.firstOrNull()
    var last by rememberSaveable { mutableStateOf<LastResult?>(null) }
    LaunchedEffect(state) {
        (state as? DictationState.Done)?.let { last = LastResult(it.text, it.stats.audioSeconds, it.stats.latencyMs) }
    }

    Column(
        Modifier
            .fillMaxSize()
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(horizontal = 20.dp),
    ) {
        Row(Modifier.fillMaxWidth().padding(top = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            if (active != null) HostPill(active, health[active.url], tailnet, backdrop, onSettings)
            Spacer(Modifier.weight(1f))
            GlassIconButton("Settings", backdrop, onClick = onSettings) { slidersGlyph(it) }
        }

        BoxWithConstraints(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
            val orb = minOf(184.dp, maxHeight * 0.62f, maxWidth * 0.56f)
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                MicOrb(state, { level.value }, backdrop, orb) {
                    if (!readiness.mic) actions.requestMic() else app.dictation.toggle { }
                }
                Spacer(Modifier.height(26.dp))
                StatusLine(state, readiness.mic)
            }
        }

        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            AnimatedVisibility(
                visible = last != null,
                enter = fadeIn() + expandVertically(expandFrom = Alignment.Bottom),
                exit = fadeOut() + shrinkVertically(),
            ) {
                last?.let { TranscriptCard(it, backdrop) }
            }
            SetupCard(readiness, actions, backdrop)
        }
        Spacer(Modifier.height(16.dp))
    }
}

/** The connected host, quietly: a dot, its name, and a word only when something is wrong. */
@Composable
private fun HostPill(host: SavedHost, health: Result<Long>?, tailnet: TailnetStatus, backdrop: Backdrop, onClick: () -> Unit) {
    val c = Crystal.colors
    val (dot, issue) = when {
        health == null -> c.ink3 to null
        health.isSuccess -> c.ok to null
        health.rejected() -> c.error to "Pair again"
        !tailnet.connected -> c.warn to "Tailscale off"
        else -> c.warn to "Offline"
    }
    Row(
        Modifier
            .heightIn(min = 44.dp)
            .glass(backdrop, Capsule(), fill = c.surfaceStrong, blurRadius = 10.dp, refraction = 8.dp, elevated = false)
            .clickable(remember { MutableInteractionSource() }, null, role = Role.Button, onClick = onClick)
            .padding(start = 14.dp, end = 16.dp, top = 10.dp, bottom = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Dot(dot)
        Spacer(Modifier.width(8.dp))
        BasicText(
            host.name,
            style = Type.label.copy(fontSize = Type.body.fontSize),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.widthIn(max = 180.dp),
        )
        if (issue != null) {
            BasicText("  ·  $issue", style = Type.body.copy(color = dot), maxLines = 1)
        }
    }
}

@Composable
private fun StatusLine(state: DictationState, micAllowed: Boolean) {
    val c = Crystal.colors
    Box(Modifier.height(24.dp).fillMaxWidth(0.9f), contentAlignment = Alignment.Center) {
        when (state) {
            is DictationState.Listening -> Row(verticalAlignment = Alignment.CenterVertically) {
                Dot(c.live)
                Spacer(Modifier.width(8.dp))
                BasicText("Listening", style = Type.body.copy(color = c.ink))
                Spacer(Modifier.width(10.dp))
                Elapsed(state.startedAt)
            }
            DictationState.Transcribing -> BasicText("Transcribing", style = Type.body)
            is DictationState.Failed -> BasicText(
                state.message,
                style = Type.body.copy(color = c.error, textAlign = TextAlign.Center),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            is DictationState.Done -> Unit
            DictationState.Idle -> BasicText(if (micAllowed) "Tap to speak" else "Tap to allow the microphone", style = Type.body)
        }
    }
}

@Composable
fun Elapsed(startedAt: Long) {
    var now by remember { mutableLongStateOf(android.os.SystemClock.elapsedRealtime()) }
    LaunchedEffect(startedAt) {
        while (true) {
            now = android.os.SystemClock.elapsedRealtime()
            delay(250)
        }
    }
    val seconds = ((now - startedAt) / 1000).coerceAtLeast(0)
    BasicText("%d:%02d".format(seconds / 60, seconds % 60), style = Type.mono.copy(color = Crystal.colors.ink2))
}

@Composable
private fun TranscriptCard(result: LastResult, backdrop: Backdrop) {
    val context = LocalContext.current
    var copied by remember(result) { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied) {
            delay(1_600)
            copied = false
        }
    }
    GlassCard(backdrop, Modifier.fillMaxWidth(), spacing = 14.dp) {
        Box(Modifier.heightIn(max = 176.dp).verticalScroll(rememberScrollState())) {
            BasicText(result.text, style = Type.transcript)
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            BasicText(
                "%.1f s · %d ms".format(result.audioSeconds, result.latencyMs),
                style = Type.mono,
                modifier = Modifier.weight(1f),
            )
            PillButton(if (copied) "Copied" else "Copy") {
                context.getSystemService(ClipboardManager::class.java)
                    .setPrimaryClip(ClipData.newPlainText("Fairspoken", result.text))
                copied = true
            }
            Spacer(Modifier.width(8.dp))
            PillButton("Share") {
                val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, result.text)
                context.startActivity(Intent.createChooser(send, null))
            }
        }
    }
}

private data class Step(val label: String, val done: Boolean, val action: String, val run: () -> Unit)

/** Shown only while something is missing; gone once everything is set. */
@Composable
private fun SetupCard(readiness: Readiness, actions: HomeActions, backdrop: Backdrop) {
    val steps = listOf(
        Step("Microphone", readiness.mic, "Allow", actions.requestMic),
        Step(
            "Voice keyboard",
            readiness.keyboardEnabled || readiness.dockRunning,
            "Enable",
            actions.openKeyboardSettings,
        ),
    )
    val remaining = steps.count { !it.done }
    var expanded by rememberSaveable { mutableStateOf(true) }
    AnimatedVisibility(remaining > 0, enter = fadeIn(), exit = fadeOut() + shrinkVertically()) {
        GlassCard(backdrop, Modifier.fillMaxWidth(), padding = PaddingValues(horizontal = 20.dp, vertical = 6.dp), spacing = 0.dp) {
            Row(
                Modifier
                    .fillMaxWidth()
                    .heightIn(min = 52.dp)
                    .clickable(remember { MutableInteractionSource() }, null, role = Role.Button) { expanded = !expanded },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                BasicText("Finish setup", style = Type.heading, modifier = Modifier.weight(1f))
                BasicText("${steps.size - remaining} of ${steps.size}", style = Type.caption)
                Spacer(Modifier.width(8.dp))
                val ink2 = Crystal.colors.ink2
                Canvas(Modifier.size(16.dp)) { if (expanded) chevronUpGlyph(ink2) else chevronDownGlyph(ink2) }
            }
            AnimatedVisibility(expanded) {
                Column {
                    var firstOpen = true
                    steps.forEach { step ->
                        Hairline()
                        Row(Modifier.fillMaxWidth().heightIn(min = 56.dp), verticalAlignment = Alignment.CenterVertically) {
                            StepMark(step.done)
                            Spacer(Modifier.width(14.dp))
                            BasicText(
                                step.label,
                                style = if (step.done) Type.label.copy(color = Crystal.colors.ink3) else Type.label,
                                modifier = Modifier.weight(1f),
                            )
                            if (!step.done) {
                                PillButton(step.action, primary = firstOpen, onClick = step.run)
                                firstOpen = false
                            }
                        }
                    }
                }
            }
        }
    }
}

/** For screens that scroll under the status bar: page colour behind the clock. */
@Composable
fun StatusBarScrim() {
    val top = Crystal.colors.pageSheen.copy(alpha = 0.94f)
    Column(Modifier.fillMaxWidth()) {
        Box(Modifier.fillMaxWidth().windowInsetsTopHeight(WindowInsets.statusBars).background(top))
        Box(Modifier.fillMaxWidth().height(18.dp).background(Brush.verticalGradient(listOf(top, Color.Transparent))))
    }
}
