package ie.fairspoken.mobile.ui

import android.os.Build
import android.provider.Settings
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kyant.backdrop.Backdrop
import ie.fairspoken.mobile.FairspokenApp
import ie.fairspoken.mobile.core.HostException
import ie.fairspoken.mobile.core.PairOutcome
import ie.fairspoken.mobile.core.SavedHost
import ie.fairspoken.mobile.core.TailnetStatus
import kotlinx.coroutines.launch

/**
 * One screen to reach a host: its address (machine name, `name:port`, IP or
 * URL; the default port is implied), the pairing password, Connect.
 */
@Composable
fun ConnectScreen(
    app: FairspokenApp,
    backdrop: Backdrop,
    tailnet: TailnetStatus,
    openTailscale: (() -> Unit)?,
    onBack: (() -> Unit)?,
    onConnected: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val keyboard = LocalSoftwareKeyboardController.current
    var address by rememberSaveable { mutableStateOf("") }
    // Not saveable: a saved-state bundle is no place for a secret.
    var password by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun connect() {
        val target = address.trim()
        if (target.isEmpty() || busy) return
        keyboard?.hide()
        busy = true
        error = null
        scope.launch {
            val found = try {
                app.client.find(target, tailnet.domain)
            } catch (e: HostException) {
                // Off the tailnet, the real fix is turning Tailscale on.
                error = if (tailnet.connected) e.message else "Tailscale is off"
                busy = false
                return@launch
            }
            error = when {
                found == null -> if (tailnet.connected) "No Fairspoken host answered at $target" else "Tailscale is off"
                found.hello.auth == "none" -> {
                    app.store.save(SavedHost(found.url, found.hello.name, null))
                    null
                }
                found.hello.auth != "password" -> "Set a pairing password on ${found.hello.name} first"
                password.isEmpty() -> "Enter the pairing password"
                else -> {
                    val device = Settings.Global.getString(context.contentResolver, "device_name") ?: Build.MODEL
                    when (val outcome = app.client.pair(found, password, device)) {
                        is PairOutcome.Paired -> {
                            app.store.save(outcome.host)
                            null
                        }
                        is PairOutcome.Failed -> outcome.message
                    }
                }
            }
            busy = false
            if (error == null) {
                password = ""
                address = ""
                onConnected()
            }
        }
    }

    Column(
        Modifier
            .fillMaxSize()
            .statusBarsPadding()
            .navigationBarsPadding()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp),
    ) {
        Box(Modifier.fillMaxWidth().height(64.dp), contentAlignment = Alignment.CenterStart) {
            if (onBack != null) GlassIconButton("Back", backdrop, onClick = onBack) { backGlyph(it) }
        }
        Spacer(Modifier.height(40.dp))
        BasicText("Connect to your host", style = Type.largeTitle)
        Spacer(Modifier.height(8.dp))
        BasicText("The computer running Fairspoken on your tailnet.", style = Type.body)
        Spacer(Modifier.height(32.dp))

        GlassCard(backdrop, Modifier.fillMaxWidth(), padding = PaddingValues(horizontal = 18.dp), spacing = 0.dp) {
            FieldRow("Host") {
                Field(
                    address, { address = it; error = null }, "name, IP or URL",
                    Modifier.weight(1f), mono = true, imeAction = ImeAction.Next,
                )
            }
            Hairline()
            FieldRow("Password") {
                Field(
                    password, { password = it; error = null }, "Pairing password",
                    Modifier.weight(1f), password = true, imeAction = ImeAction.Go, onAction = ::connect,
                )
            }
        }

        Box(Modifier.fillMaxWidth().heightIn(min = 52.dp).padding(horizontal = 4.dp), contentAlignment = Alignment.CenterStart) {
            val c = Crystal.colors
            when {
                error != null -> Row(verticalAlignment = Alignment.CenterVertically) {
                    BasicText(
                        error!!,
                        style = Type.body.copy(color = if (error == "Tailscale is off") c.warn else c.error),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                    if (error == "Tailscale is off" && openTailscale != null) {
                        Spacer(Modifier.width(12.dp))
                        PillButton("Open", onClick = openTailscale)
                    }
                }
                !tailnet.connected -> Row(verticalAlignment = Alignment.CenterVertically) {
                    Dot(c.warn)
                    Spacer(Modifier.width(8.dp))
                    BasicText("Tailscale is off", style = Type.body.copy(color = c.warn), modifier = Modifier.weight(1f))
                    if (openTailscale != null) PillButton("Open", onClick = openTailscale)
                }
            }
        }

        GlassButton(
            "Connect",
            backdrop,
            Modifier.fillMaxWidth(),
            primary = true,
            enabled = address.isNotBlank(),
            busy = busy,
            onClick = ::connect,
        )
        Spacer(Modifier.height(24.dp))
    }
}

@Composable
private fun FieldRow(label: String, field: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit) {
    Row(Modifier.fillMaxWidth().heightIn(min = 54.dp), verticalAlignment = Alignment.CenterVertically) {
        BasicText(label, style = Type.label, modifier = Modifier.width(92.dp))
        field()
    }
}
