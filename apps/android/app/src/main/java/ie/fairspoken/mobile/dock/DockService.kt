package ie.fairspoken.mobile.dock

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import ie.fairspoken.mobile.R
import ie.fairspoken.mobile.fairspoken
import ie.fairspoken.mobile.insert.InsertService
import ie.fairspoken.mobile.ui.MainActivity
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch

/**
 * Keeps the floating dock on screen. A microphone foreground service, so it
 * may record while another app is in front; it has to be started from the
 * Fairspoken screen, which is why it does not restart itself.
 */
class DockService : Service() {
    private val scope = MainScope()
    private var dock: DockWindow? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        startForeground(NOTIFICATION_ID, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
        val window = DockWindow(this, fairspoken)
        dock = window
        _running.value = true
        scope.launch {
            combine(
                fairspoken.store.settings,
                InsertService.connected,
                InsertService.keyboardOpen,
                window.ownsSession,
            ) { settings, insertOn, keyboardOpen, busy ->
                busy || !settings.dockOnlyWhileTyping || !insertOn || keyboardOpen
            }.distinctUntilChanged().collect { window.setVisible(it) }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) stopSelf()
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        dock?.dismiss()
        dock = null
        _running.value = false
        super.onDestroy()
    }

    private fun notification(): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.dock_channel), NotificationManager.IMPORTANCE_LOW),
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, DockService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_mic)
            .setContentTitle("Floating dock is on")
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, "Turn off", stop).build())
            .setOngoing(true)
            .build()
    }

    companion object {
        private const val CHANNEL = "dock"
        private const val NOTIFICATION_ID = 7
        private const val ACTION_STOP = "ie.fairspoken.mobile.dock.STOP"

        private val _running = MutableStateFlow(false)
        val running: StateFlow<Boolean> = _running.asStateFlow()

        fun start(context: Context) {
            context.startForegroundService(Intent(context, DockService::class.java))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, DockService::class.java))
        }
    }
}
