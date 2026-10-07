package ie.fairspoken.mobile.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kyant.backdrop.Backdrop
import ie.fairspoken.mobile.FairspokenApp
import ie.fairspoken.mobile.core.SavedHost
import ie.fairspoken.mobile.core.VocabularyPacks

/** Everything that isn't the microphone, behind the one Settings button. */
@Composable
fun SettingsScreen(
    app: FairspokenApp,
    backdrop: Backdrop,
    readiness: Readiness,
    actions: HomeActions,
    health: HostHealth,
    onBack: () -> Unit,
    onAddHost: () -> Unit,
) {
    val settings by app.store.settings.collectAsState()
    Box(Modifier.fillMaxSize()) {
        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .statusBarsPadding()
                .navigationBarsPadding()
                .padding(horizontal = 20.dp),
        ) {
            Row(Modifier.fillMaxWidth().height(64.dp), verticalAlignment = Alignment.CenterVertically) {
                GlassIconButton("Back", backdrop, onClick = onBack) { backGlyph(it) }
                Spacer(Modifier.width(14.dp))
                BasicText("Settings", style = Type.title)
            }
            Spacer(Modifier.height(8.dp))

            Section("Host")
            HostsCard(app, backdrop, health, onAddHost)

            Section("Dictation")
            DictationCard(app, backdrop)

            Section("Voice keyboard")
            GlassCard(backdrop, Modifier.fillMaxWidth(), padding = PaddingValues(horizontal = 20.dp, vertical = 8.dp), spacing = 0.dp) {
                SettingRow("Use as a keyboard", Modifier.padding(vertical = 6.dp)) {
                    when {
                        !readiness.keyboardEnabled -> PillButton("Enable", primary = true, onClick = actions.openKeyboardSettings)
                        readiness.keyboardSelected -> StepMark(true)
                        else -> PillButton("Switch", onClick = actions.pickKeyboard)
                    }
                }
                Hairline()
                Box(Modifier.padding(vertical = 6.dp)) {
                    SwitchRow("Listen when it opens", settings.autoListen, { on ->
                        app.store.updateSettings { it.copy(autoListen = on) }
                    })
                }
            }

            Section("Floating dock")
            DockCard(app, backdrop, readiness, actions)

            BasicText(
                versionLine(),
                style = Type.caption.copy(textAlign = TextAlign.Center),
                modifier = Modifier.fillMaxWidth().padding(top = 28.dp, bottom = 20.dp),
            )
        }
        StatusBarScrim()
    }
}

@Composable
private fun Section(title: String) {
    BasicText(title, style = Type.section, modifier = Modifier.padding(start = 4.dp, top = 22.dp, bottom = 8.dp))
}

@Composable
private fun HostsCard(app: FairspokenApp, backdrop: Backdrop, health: HostHealth, onAddHost: () -> Unit) {
    val hosts by app.store.hosts.collectAsState()
    val activeUrl by app.store.activeUrl.collectAsState()
    val active = hosts.firstOrNull { it.url == activeUrl } ?: hosts.firstOrNull()
    GlassCard(backdrop, Modifier.fillMaxWidth(), padding = PaddingValues(8.dp), spacing = 2.dp) {
        hosts.forEach { host ->
            HostRow(host, host.url == active?.url, health[host.url]) { app.store.select(host.url) }
        }
        Row(
            Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 6.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            PillButton("Add host", onClick = onAddHost)
            Spacer(Modifier.weight(1f))
            if (active != null) PillButton("Forget") { app.store.remove(active.url) }
        }
    }
}

@Composable
private fun HostRow(host: SavedHost, active: Boolean, health: Result<Long>?, onSelect: () -> Unit) {
    val c = Crystal.colors
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RowShape)
            .background(if (active) c.tint else androidx.compose.ui.graphics.Color.Transparent)
            .clickable(onClick = onSelect)
            .padding(horizontal = 12.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Dot(
            when {
                health == null -> c.ink3
                health.isSuccess -> c.ok
                health.rejected() -> c.error
                else -> c.warn
            },
        )
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            BasicText(host.name, style = Type.label, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val detail = health?.fold(
                { ms -> "${host.url.substringAfter("://")} · $ms ms" },
                { e -> if (health.rejected()) "Pair again" else e.message ?: "Offline" },
            ) ?: host.url.substringAfter("://")
            BasicText(detail, style = Type.mono, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (active) {
            Spacer(Modifier.width(10.dp))
            val accent = c.accent
            Canvas(Modifier.size(20.dp)) { checkGlyph(accent) }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DictationCard(app: FairspokenApp, backdrop: Backdrop) {
    val settings by app.store.settings.collectAsState()
    GlassCard(backdrop, Modifier.fillMaxWidth(), padding = PaddingValues(horizontal = 20.dp, vertical = 16.dp), spacing = 14.dp) {
        BasicText("Vocabulary", style = Type.label)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Chip("Everyday", settings.packId == null) { app.store.updateSettings { it.copy(packId = null) } }
            VocabularyPacks.all.forEach { pack ->
                Chip(pack.name, settings.packId == pack.id) { app.store.updateSettings { it.copy(packId = pack.id) } }
            }
        }
        Hairline()
        SwitchRow(
            "Super mode",
            settings.superMode,
            { on -> app.store.updateSettings { it.copy(superMode = on) } },
            detail = "Parakeet and Whisper together. Slower.",
        )
    }
}

@Composable
private fun DockCard(app: FairspokenApp, backdrop: Backdrop, readiness: Readiness, actions: HomeActions) {
    val settings by app.store.settings.collectAsState()
    GlassCard(backdrop, Modifier.fillMaxWidth(), padding = PaddingValues(horizontal = 20.dp, vertical = 8.dp), spacing = 0.dp) {
        Box(Modifier.padding(vertical = 6.dp)) {
            SwitchRow(
                "Show the dock",
                readiness.dockRunning,
                { on ->
                    when {
                        on && !readiness.mic -> actions.requestMic()
                        on && !readiness.overlay -> actions.openOverlaySettings()
                        else -> actions.setDock(on)
                    }
                },
                detail = "A bubble over every app. Tap or hold to talk.",
            )
        }
        Hairline()
        Requirement("Show over other apps", readiness.overlay, "Allow", actions.openOverlaySettings)
        Hairline()
        Requirement(
            "Type into apps",
            readiness.insertService,
            "Turn on",
            actions.openAccessibilitySettings,
            detail = if (readiness.insertService) null else "Off: text is copied for you to paste.",
        )
        if (readiness.insertService) {
            Hairline()
            Box(Modifier.padding(vertical = 6.dp)) {
                SwitchRow("Only while typing", settings.dockOnlyWhileTyping, { on ->
                    app.store.updateSettings { it.copy(dockOnlyWhileTyping = on) }
                })
            }
        } else {
            BasicText(
                "Greyed out? App info › ⋮ › Allow restricted settings.",
                style = Type.caption,
                modifier = Modifier.padding(bottom = 12.dp),
            )
        }
    }
}

@Composable
private fun Requirement(label: String, met: Boolean, action: String, onClick: () -> Unit, detail: String? = null) {
    SettingRow(label, Modifier.heightIn(min = 56.dp).padding(vertical = 6.dp), detail = detail) {
        if (met) StepMark(true) else PillButton(action, onClick = onClick)
    }
}

@Composable
private fun versionLine(): String {
    val context = LocalContext.current
    return remember {
        val version = runCatching { context.packageManager.getPackageInfo(context.packageName, 0).versionName }.getOrNull()
        if (version != null) "Fairspoken $version" else "Fairspoken"
    }
}
