package co.phantom.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import androidx.core.app.NotificationCompat

class PhantomVpnService : VpnService() {

    companion object {
        const val ACTION_CONNECT = "co.phantom.android.CONNECT"
        const val ACTION_DISCONNECT = "co.phantom.android.DISCONNECT"

        const val EXTRA_SERVER_URI = "server_uri"
        const val EXTRA_PROXY_MODE = "proxy_mode"

        private const val NOTIFICATION_ID = 1
        private const val CHANNEL_ID = "phantom_vpn"
    }

    private var pfd: ParcelFileDescriptor? = null

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_DISCONNECT -> {
                stopVpn()
                return START_NOT_STICKY
            }
            ACTION_CONNECT -> {
                val uri = intent.getStringExtra(EXTRA_SERVER_URI) ?: return START_NOT_STICKY
                val mode = intent.getStringExtra(EXTRA_PROXY_MODE) ?: "smart"
                startVpn(uri, mode)
            }
            else -> {
                // No action or unknown action; stop if not running.
                if (pfd == null) {
                    stopSelf()
                    return START_NOT_STICKY
                }
            }
        }
        return START_STICKY
    }

    private fun startVpn(uri: String, mode: String) {
        val builder = Builder()
            .addAddress("10.7.0.2", 24)
            .addRoute("0.0.0.0", 0)
            .addDnsServer("8.8.8.8")
            .addDnsServer("8.8.4.4")
            .setMtu(1500)
            .setSession("Phantom")

        pfd = builder.establish()
        if (pfd == null) {
            stopSelf()
            return
        }

        val fd = pfd!!.detachFd()

        startForeground(NOTIFICATION_ID, buildNotification())

        val rc = RustBridge.startTunnelWithURI(fd, uri, mode)
        if (rc != 0) {
            stopVpn()
        }
    }

    private fun stopVpn() {
        RustBridge.stopTunnel()
        pfd?.close()
        pfd = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        stopVpn()
        super.onDestroy()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                getString(R.string.vpn_notification_channel_name),
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = getString(R.string.vpn_notification_channel_description)
            }
            val notificationManager = getSystemService(NotificationManager::class.java)
            notificationManager.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(): Notification {
        val pendingIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE
        )
        val disconnectIntent = Intent(this, PhantomVpnService::class.java).apply {
            action = ACTION_DISCONNECT
        }
        val disconnectPendingIntent = PendingIntent.getService(
            this,
            0,
            disconnectIntent,
            PendingIntent.FLAG_IMMUTABLE
        )

        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.vpn_notification_title))
            .setContentText(getString(R.string.vpn_notification_text))
            .setSmallIcon(android.R.drawable.ic_menu_mylocation)
            .setContentIntent(pendingIntent)
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "Disconnect", disconnectPendingIntent)
            .setOngoing(true)
            .build()
    }
}
