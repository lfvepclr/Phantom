package co.phantom.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.net.ConnectivityManager
import android.net.IpPrefix
import android.net.Network
import android.net.NetworkCapabilities
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import androidx.core.app.NotificationCompat
import java.io.File
import java.net.InetAddress
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Foreground service that owns the TUN interface.
 *
 * Beyond the one-shot fd hand-off, this class is the only component that
 * survives the user leaving the app, which is why the connectivity watchdog
 * lives here rather than in the UI: a phone that moves from Wi-Fi to cellular
 * while the screen is off still has to come back on its own.
 */
class PhantomVpnService : VpnService() {

    companion object {
        const val ACTION_CONNECT = "co.phantom.android.CONNECT"
        const val ACTION_DISCONNECT = "co.phantom.android.DISCONNECT"

        const val EXTRA_SERVER_URI = "server_uri"
        const val EXTRA_PROXY_MODE = "proxy_mode"

        private const val NOTIFICATION_ID = 1
        private const val CHANNEL_ID = "phantom_vpn"

        /** ~7.5 s at the 3 s back-off step, long enough to be worth rejecting. */
        private const val DETACH_GRACE_MS = 1_500L

        /**
         * How long after `establish()` the TUN's own route/network callbacks are
         * treated as settling noise rather than a radio handover.
         */
        private const val TUN_SETTLE_MS = 2_000L

        /**
         * Cadence of the service's single periodic read of the core.
         *
         * One timer answers both "is the tunnel still up?" and "how fast is it
         * going?", and its samples are what the UI renders — so the dashboard
         * needs no timer of its own and a sleeping screen costs one wake a
         * second in the service, not three in an activity.
         */
        private const val WATCHDOG_INTERVAL_MS = 1_000L
    }

    private var pfd: ParcelFileDescriptor? = null
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var watchdog: Job? = null

    private var serverUri: String = ""
    private var proxyMode: ProxyMode = ProxyMode.SMART

    /** Attempts already spent on the current outage. */
    private var reconnectAttempts = 0
    private var reconnectJob: Job? = null

    /**
     * Server addresses carved out of the TUN (`/32` per resolved address).
     *
     * The route table hands `0.0.0.0/0` to the TUN for **every** uid, ours
     * included, so the socket the tunnel opens towards the server would be
     * captured by the very tunnel it is trying to bring up and time out in a
     * loop. HarmonyOS solves the same problem with `protectProcessNet`; here
     * the equivalent is excluding the server's route.
     */
    private var serverExclusions: List<IpPrefix> = emptyList()

    private lateinit var connectivity: ConnectivityManager

    /**
     * Fires when the default network changes: Wi-Fi ⇄ cellular, or a different
     * Wi-Fi network. Everything the tunnel established is bound to the old
     * source address, so the flows are invalidated and the link is rebuilt.
     */
    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            // `registerDefaultNetworkCallback` reports the *current* default
            // network as soon as it is registered. That is a snapshot, not a
            // change, so the first one is discarded.
            if (!networkCallbackPrimed) {
                networkCallbackPrimed = true
                return
            }
            if (settling()) return
            onRadioChanged()
        }

        override fun onLost(network: Network) {
            // Losing the default network is the handover in progress; the
            // matching onAvailable either follows (Wi-Fi → cellular) or the
            // watchdog picks the outage up. Losing *our own* TUN is not a radio
            // event at all — that is `onRevoke`/`stopVpn` tearing it down.
            if (isVpn(network)) return
            if (settling()) return
            onRadioChanged()
        }
    }

    /** False until the initial `onAvailable` snapshot has been discarded. */
    private var networkCallbackPrimed = false

    /** Uptime of the current TUN, used to ignore its own establishment noise. */
    private var tunEstablishedAt = 0L

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        connectivity = getSystemService(ConnectivityManager::class.java)
        RustBridge.attachService(this)
        // Mirror the session log next to the TUN trace: the ring buffer alone
        // is gone the moment the process is.
        RustBridge.setLogPath(File(filesDir, LOG_FILE).absolutePath)
        applyTraceSetting()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_DISCONNECT -> {
                stopVpn(userInitiated = true)
                return START_NOT_STICKY
            }

            ACTION_CONNECT -> {
                val uri = intent.getStringExtra(EXTRA_SERVER_URI) ?: return START_NOT_STICKY
                val mode = intent.getStringExtra(EXTRA_PROXY_MODE) ?: ProxyMode.SMART.key
                serverUri = uri
                proxyMode = ProxyMode.from(mode)
                reconnectAttempts = 0
                TunnelController.setDesired(true)
                TunnelController.resetReconnect()
                // Say "connecting" now rather than when the first watchdog tick
                // lands: `establish()` is asynchronous, and the user pressed a
                // button, so the button must not still read "启动连接".
                TunnelController.publishStatus(STATUS_STARTING, "", System.currentTimeMillis())
                launchVpn()
                registerNetworkCallback()
                startWatchdog()
            }

            else -> {
                // Restarted by the system (START_STICKY) without an intent:
                // resume the session the user asked for, if any.
                if (pfd == null) {
                    if (serverUri.isEmpty() || !TunnelController.desiredRunning.value) {
                        stopSelf()
                        return START_NOT_STICKY
                    }
                    launchVpn()
                    registerNetworkCallback()
                    startWatchdog()
                }
            }
        }
        return START_STICKY
    }

    /**
     * Exempt a socket from the tunnel (`VpnService.protect`).
     *
     * Called from Rust through [RustBridge.protectFd] for the direct-DNS
     * socket, which must reach the physical network while `0.0.0.0/0` is routed
     * into the TUN.
     */
    fun protectSocket(fd: Int): Boolean = protect(fd)

    /** Re-apply the opt-in TUN trace when the setting changed. */
    fun applyTraceSetting() {
        val trace = File(filesDir, TRACE_FILE)
        val enabled = Prefs(this).tunTrace
        RustBridge.setTracePath(if (enabled) trace.absolutePath else null)
        applyUserRules()
    }

    /**
     * Hand the user's whitelist rules to the core before a start.
     *
     * Rules are read from preferences rather than from the caller, so a restart
     * (including a system-started one, which arrives with no intent) applies the
     * same set the editor last wrote. Routing state is built once per start,
     * which is why this must happen before `startTunnelWithURI`.
     */
    private fun applyUserRules() {
        val rules = Prefs(this).userRules
        if (rules.isEmpty()) return
        RustBridge.setUserRules(rules.joinToString("\n"))
    }

    /**
     * Resolve the server, then build the TUN.
     *
     * Resolution has to happen off the main thread (a hostname would need a
     * real DNS round trip), and it has to happen *before* the TUN is up, so
     * the exclusions can be baked into the route table in one go.
     */
    private fun launchVpn() {
        scope.launch {
            serverExclusions = withContext(Dispatchers.IO) { resolveServerExclusions() }
            startVpn()
        }
    }

    private fun resolveServerExclusions(): List<IpPrefix> {
        val link = parseServerUri(serverUri)
        if (!link.valid || link.host.isEmpty()) return emptyList()
        return try {
            InetAddress.getAllByName(link.host)
                .filter { it.address.size == 4 || it.address.size == 16 }
                .map { IpPrefix(it, it.address.size * 8) }
        } catch (e: Exception) {
            android.util.Log.w(TAG, "resolving ${link.host} for route exclusion failed", e)
            emptyList()
        }
    }

    private fun startVpn() {
        val builder = Builder()
            .addAddress("10.7.0.2", 24)
            .addRoute("0.0.0.0", 0)
            .addDnsServer("8.8.8.8")
            .setMtu(1500)
            .setSession("Phantom")

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            // The direct resolver deliberately bypasses the tunnel; saying so in
            // the route table keeps the JNI protect() from being the only thing
            // standing between a domestic domain and the wrong CDN.
            try {
                builder.excludeRoute(IpPrefix(InetAddress.getByName(DIRECT_DNS), 32))
            } catch (e: Exception) {
                android.util.Log.w(TAG, "excludeRoute for $DIRECT_DNS failed", e)
            }
            // And the server itself, which is what keeps the tunnel's own
            // handshake out of the TUN. Without it the tunnel never comes up
            // and every attempt dies with "Hello verification error".
            for (prefix in serverExclusions) {
                try {
                    builder.excludeRoute(prefix)
                } catch (e: Exception) {
                    android.util.Log.w(TAG, "excludeRoute for $prefix failed", e)
                }
            }
        }

        val descriptor = builder.establish()
        if (descriptor == null) {
            // The user revoked the VPN consent, or another VPN took over.
            RustBridge.setTracePath(null)
            TunnelController.setDesired(false)
            stopSelf()
            return
        }
        pfd = descriptor
        tunEstablishedAt = SystemClock.elapsedRealtime()
        startForeground(NOTIFICATION_ID, buildNotification())

        val rc = RustBridge.startTunnelWithURI(descriptor.detachFd(), serverUri, proxyMode.key)
        if (rc != 0) {
            stopVpn(userInitiated = false)
            return
        }
        // The core is now building the session; publish before the first tick so
        // the state change lands in the same frame as the tap that caused it.
        publishSample(RustBridge.getStatus())
    }

    /** True when [network] is the TUN this service owns. */
    private fun isVpn(network: Network): Boolean =
        connectivity.getNetworkCapabilities(network)
            ?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true

    /**
     * True while the freshly established TUN is still settling into place.
     *
     * Bringing the interface up makes it the default network and demotes the
     * radio network that was default a moment ago. Both show up as callbacks
     * (`onAvailable` for the TUN, `onLost` for the radio), and neither is a
     * handover — treating them as one made every connect flash "网络已切换"
     * and rebuild the tunnel it had just finished building.
     */
    private fun settling(): Boolean =
        SystemClock.elapsedRealtime() - tunEstablishedAt < TUN_SETTLE_MS

    private fun registerNetworkCallback() {
        try {
            // Each registration replays the current default network once, so the
            // "is this a change?" latch has to be re-armed with it.
            networkCallbackPrimed = false
            connectivity.registerDefaultNetworkCallback(networkCallback)
        } catch (e: Exception) {
            android.util.Log.w(TAG, "network callback registration failed", e)
        }
    }

    private fun onRadioChanged() {
        if (pfd == null) return
        TunnelController.noteNetworkChange(System.currentTimeMillis())
        // Drop every flow bound to the vanished source address before the next
        // request rides the new one.
        RustBridge.notifyNetworkChange()
        if (TunnelController.desiredRunning.value) {
            scheduleReconnect(immediate = true)
        }
    }

    /**
     * Watch the tunnel while the user still wants it.
     *
     * Rust reports idle/error when its task ended (server link lost, protocol
     * error), and the UI cannot be trusted to notice — it may not even exist.
     */
    private fun startWatchdog() {
        if (watchdog?.isActive == true) return
        watchdog = scope.launch {
            while (isActive) {
                // Read the intent *before* touching the core. Once the user has
                // asked to stop, the teardown is already in flight and a sample
                // would only flicker the UI back to "connected" for one frame —
                // and the JNI call is the expensive part of this loop, so it is
                // the part that must stay behind the check.
                if (!TunnelController.desiredRunning.value) {
                    delay(WATCHDOG_INTERVAL_MS)
                    continue
                }
                val status = RustBridge.getStatus()
                publishSample(status)
                if (status == STATUS_RUNNING) {
                    if (reconnectAttempts != 0) {
                        reconnectAttempts = 0
                        TunnelController.resetReconnect()
                    }
                } else if (status == STATUS_IDLE || status == STATUS_ERROR) {
                    scheduleReconnect(immediate = false)
                }
                delay(WATCHDOG_INTERVAL_MS)
            }
        }
    }

    /**
     * Publish one observation of the core to [TunnelController].
     *
     * This is the app's only periodic read of the datapath, and it lives in the
     * foreground service rather than in the UI: the service is the one
     * component that keeps running with the screen off, so it is the only place
     * a sample is still meaningful. The UI just collects.
     */
    private fun publishSample(status: Int) {
        TunnelController.publishStatus(
            code = status,
            error = if (status == STATUS_ERROR) RustBridge.getLastError() else "",
            atMs = System.currentTimeMillis(),
        )
        if (status == STATUS_RUNNING) {
            val stats = RustBridge.stats()
            TunnelController.publishStats(
                TunnelController.TunnelStats(
                    down = stats.down,
                    up = stats.up,
                    udpDown = stats.udpDown,
                    udpUp = stats.udpUp,
                    routeProxy = stats.routeProxy,
                    routeDirect = stats.routeDirect,
                )
            )
        }
    }

    /**
     * Restart the tunnel, spaced out so a flapping radio does not turn into a
     * reconnect storm.
     *
     * A radio handover restarts immediately (the old link is provably gone),
     * while an unexplained drop follows the 3 s / 10 s / 30 s schedule before
     * giving up and telling the user.
     */
    private fun scheduleReconnect(immediate: Boolean) {
        if (reconnectJob?.isActive == true) return
        if (reconnectAttempts >= RECONNECT_DELAYS_MS.size) {
            TunnelController.updateReconnect(
                TunnelController.ReconnectState(exhausted = true, attempts = reconnectAttempts)
            )
            TunnelController.setDesired(false)
            return
        }
        val delayMs = if (immediate) 0L else RECONNECT_DELAYS_MS[reconnectAttempts]
        reconnectAttempts++
        reconnectJob = scope.launch {
            var remaining = delayMs
            while (remaining > 0) {
                TunnelController.updateReconnect(
                    TunnelController.ReconnectState(
                        inSeconds = ((remaining + 999) / 1000).toInt(),
                        attempts = reconnectAttempts,
                    )
                )
                val step = minOf(1_000L, remaining)
                delay(step)
                remaining -= step
            }
            TunnelController.updateReconnect(
                TunnelController.ReconnectState(attempts = reconnectAttempts)
            )
            if (!TunnelController.desiredRunning.value) return@launch
            restartTunnel()
        }
    }

    private fun restartTunnel() {
        RustBridge.stopTunnel()
        pfd?.close()
        pfd = null
        TunnelController.publishStatus(STATUS_STARTING, "", System.currentTimeMillis())
        launchVpn()
    }

    private fun stopVpn(userInitiated: Boolean) {
        if (userInitiated) {
            TunnelController.setDesired(false)
            TunnelController.resetReconnect()
        }
        // The TUN is going away either way; a system recycle must not leave the
        // dashboard showing the connection it just lost.
        TunnelController.publishStatus(STATUS_IDLE, "", System.currentTimeMillis())
        reconnectJob?.cancel()
        reconnectJob = null
        RustBridge.stopTunnel()
        pfd?.close()
        pfd = null
        try {
            connectivity.unregisterNetworkCallback(networkCallback)
        } catch (_: Exception) {
            // Not registered (or already unregistered): nothing to undo.
        }
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onRevoke() {
        // Another VPN app took over, or the user cleared ours in Settings.
        stopVpn(userInitiated = true)
        super.onRevoke()
    }

    override fun onDestroy() {
        // A system-initiated destroy (START_STICKY recycle) must not look like
        // a user-requested stop: the intent stays set so the restarted service
        // brings the tunnel back.
        stopVpn(userInitiated = false)
        RustBridge.attachService(null)
        scope.cancel()
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
            .setContentText(
                getString(R.string.vpn_notification_text, linkTitle(parseServerUri(serverUri)))
            )
            .setSmallIcon(android.R.drawable.ic_lock_lock)
            .setContentIntent(pendingIntent)
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "断开", disconnectPendingIntent)
            .setOngoing(true)
            .build()
    }
}

/** Mirrored session log, next to the TUN trace in app-private storage. */
const val LOG_FILE: String = "phantom_vpn.log"

/** Opt-in TUN trace file (`client/src/tun_trace.rs`). */
const val TRACE_FILE: String = "phantom_tun_trace.log"

/** Tunnel status codes reported by Rust. */
const val STATUS_IDLE: Int = 0
const val STATUS_STARTING: Int = 1
const val STATUS_RUNNING: Int = 2
const val STATUS_ERROR: Int = 3

/** Resolver the client uses for direct-routed domains (`client.dns_direct`). */
const val DIRECT_DNS: String = "223.5.5.5"

private const val TAG = "PhantomVpn"
