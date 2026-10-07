package ie.fairspoken.mobile.ui

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.view.inputmethod.InputMethodManager
import android.graphics.Color as AndroidColor
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import ie.fairspoken.mobile.dock.DockService
import ie.fairspoken.mobile.fairspoken
import ie.fairspoken.mobile.insert.InsertService

class MainActivity : ComponentActivity() {
    private var readiness by mutableStateOf(Readiness())
    private var refreshTick by mutableIntStateOf(0)

    private val micPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { refresh() }
    private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {
        DockService.start(this)
        refresh()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        // Light crystal background: dark status and navigation bar icons.
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.light(AndroidColor.TRANSPARENT, AndroidColor.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.light(AndroidColor.TRANSPARENT, AndroidColor.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)
        val actions = HomeActions(
            requestMic = { micPermission.launch(Manifest.permission.RECORD_AUDIO) },
            openKeyboardSettings = { startActivity(Intent(Settings.ACTION_INPUT_METHOD_SETTINGS)) },
            pickKeyboard = { getSystemService(InputMethodManager::class.java).showInputMethodPicker() },
            openOverlaySettings = {
                startActivity(Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION, Uri.parse("package:$packageName")))
            },
            openAccessibilitySettings = { startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)) },
            setDock = { on -> if (on) startDock() else DockService.stop(this) },
        )
        setContent {
            val dockRunning by DockService.running.collectAsState()
            val insertConnected by InsertService.connected.collectAsState()
            HomeScreen(
                fairspoken,
                readiness.copy(dockRunning = dockRunning, insertService = insertConnected || readiness.insertService),
                refreshTick,
                actions,
            )
        }
    }

    override fun onResume() {
        super.onResume()
        refresh()
        fairspoken.warmUp()
    }

    /** The dock's microphone service must start while this screen is in front. */
    private fun startDock() {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            DockService.start(this)
        }
    }

    private fun refresh() {
        val imm = getSystemService(InputMethodManager::class.java)
        val keyboardId = imm.enabledInputMethodList.firstOrNull { it.packageName == packageName }?.id
        val selected = Settings.Secure.getString(contentResolver, Settings.Secure.DEFAULT_INPUT_METHOD)
        val a11y = Settings.Secure.getString(contentResolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES).orEmpty()
        readiness = Readiness(
            mic = checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED,
            keyboardEnabled = keyboardId != null,
            keyboardSelected = keyboardId != null && selected == keyboardId,
            overlay = Settings.canDrawOverlays(this),
            insertService = a11y.contains("$packageName/${InsertService::class.java.name}"),
        )
        refreshTick++
    }
}
