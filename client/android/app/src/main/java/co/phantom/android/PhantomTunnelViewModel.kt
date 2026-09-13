package co.phantom.android

import android.app.Application
import android.content.Intent
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Everything the dashboard renders.
 *
 * A single state object rather than a dozen flows: the dashboard shows status,
 * rates, counters and logs together, and they are all projections of the same
 * service samples — splitting them would only let the screen render a
 * half-updated frame.
 */
data class PhantomUiState(
    val statusCode: Int = STATUS_IDLE,
    val statusText: String = "未连接",
    val statusError: String = "",
    val isRunning: Boolean = false,
    val busy: Boolean = false,
    /** Shown while the system's VPN consent dialog is up. */
    val awaitingConsent: Boolean = false,
    val serverUri: String = "",
    val link: ServerLink = ServerLink(),
    val mode: ProxyMode = ProxyMode.SMART,
    val themeMode: ThemeMode = ThemeMode.SYSTEM,
    val history: List<ServerHistoryEntry> = emptyList(),
    val logLines: List<LogEntry> = emptyList(),
    val logPaused: Boolean = false,
    val showDirectLogs: Boolean = false,
    val tunTrace: Boolean = false,
    /** User whitelist rules, wire format (see [RuleFormat]). */
    val userRules: List<String> = emptyList(),
    val downRate: Long = 0,
    val upRate: Long = 0,
    val totalDown: Long = 0,
    val totalUp: Long = 0,
    val proxiedFlows: Long = 0,
    val directFlows: Long = 0,
    val uptimeSeconds: Long = 0,
    val latency: String = "",
    val latencyBusy: Boolean = false,
    val speedResult: String = "",
    val speedBusy: Boolean = false,
    val reconnect: TunnelController.ReconnectState = TunnelController.ReconnectState(),
    val networkBanner: Boolean = false,
)

class PhantomTunnelViewModel(application: Application) : AndroidViewModel(application) {

    private val prefs = Prefs(application)
    private val _state = MutableStateFlow(PhantomUiState())
    val state: StateFlow<PhantomUiState> = _state.asStateFlow()

    private val stampFormat = SimpleDateFormat("HH:mm:ss", Locale.US)

    private var logJob: Job? = null
    private var saveJob: Job? = null

    private var logCursor: Long = 0

    /** Ids are only ever handed out here, so they stay unique and ordered. */
    private var nextLogId: Long = 0
    private var rawLogs: MutableList<LogEntry> = mutableListOf()

    private var lastStatsAt: Long = 0
    private var lastDown: Long = 0
    private var lastUp: Long = 0
    private var runningSince: Long = 0

    /** Debounces URI edits so typing does not write to disk on every key. */
    private var pendingUri: String = ""

    init {
        val savedUri = prefs.serverUri
        var history = prefs.history
        val link = parseServerUri(savedUri)
        // Nothing remembered yet but a URI exists (an older build stored only
        // the string): adopt it so the history list is not empty on first run.
        if (history.isEmpty() && link.valid) {
            history = upsertHistory(emptyList(), savedUri.trim(), System.currentTimeMillis(), 0)
            prefs.history = history
        }
        _state.value = _state.value.copy(
            serverUri = savedUri,
            link = link,
            mode = prefs.proxyMode,
            themeMode = prefs.themeMode,
            history = history,
            showDirectLogs = prefs.showDirectLogs,
            tunTrace = prefs.tunTrace,
            userRules = prefs.userRules,
            statusText = "未连接",
        )
        observeController()
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    /** Start the VPN service with the current URI and mode. */
    fun start() {
        val uri = _state.value.serverUri.trim()
        if (uri.isEmpty()) return
        saveUriNow(uri)
        val history = upsertHistory(_state.value.history, uri, System.currentTimeMillis(), 0)
        prefs.history = history
        _state.value = _state.value.copy(
            history = history,
            busy = true,
            awaitingConsent = true,
            statusText = "正在连接…",
        )
        sendServiceAction(PhantomVpnService.ACTION_CONNECT) {
            putExtra(PhantomVpnService.EXTRA_SERVER_URI, uri)
            putExtra(PhantomVpnService.EXTRA_PROXY_MODE, _state.value.mode.key)
        }
    }

    /** Request the VPN service to disconnect. */
    fun stop() {
        _state.value = _state.value.copy(awaitingConsent = false)
        sendServiceAction(PhantomVpnService.ACTION_DISCONNECT)
    }

    fun toggle() {
        if (_state.value.isRunning || _state.value.busy) stop() else start()
    }

    /** The consent dialog was dismissed; stop claiming a connection is coming. */
    fun consentDenied() {
        _state.value = _state.value.copy(awaitingConsent = false, busy = false)
    }

    fun setMode(mode: ProxyMode) {
        if (_state.value.isRunning) return
        prefs.proxyMode = mode
        _state.value = _state.value.copy(mode = mode)
    }

    fun setThemeMode(mode: ThemeMode) {
        prefs.themeMode = mode
        _state.value = _state.value.copy(themeMode = mode)
    }

    fun setShowDirectLogs(show: Boolean) {
        prefs.showDirectLogs = show
        _state.value = _state.value.copy(
            showDirectLogs = show,
            logLines = visibleLogEntries(rawLogs, show),
        )
    }

    fun setTunTrace(enabled: Boolean) {
        prefs.tunTrace = enabled
        _state.value = _state.value.copy(tunTrace = enabled)
        // The trace path is global state inside the core, so toggling it takes
        // effect on the running tunnel without a restart.
        val trace = java.io.File(getApplication<Application>().filesDir, TRACE_FILE)
        RustBridge.setTracePath(if (enabled) trace.absolutePath else null)
    }

    /**
     * Replace the user whitelist rules.
     *
     * Persisted here and pushed to the core straight away, so the *next* start
     * picks them up even if this process dies before the user reconnects — the
     * service reads the same store before it starts a tunnel.
     */
    fun setUserRules(entries: List<String>) {
        prefs.userRules = entries
        _state.value = _state.value.copy(userRules = entries)
        RustBridge.setUserRules(entries.joinToString("\n"))
    }

    fun setLogPaused(paused: Boolean) {
        _state.value = _state.value.copy(logPaused = paused)
    }

    fun clearLogs() {
        RustBridge.clearLogs()
        logCursor = 0
        rawLogs = mutableListOf()
        _state.value = _state.value.copy(logLines = emptyList())
        // `nextLogId` deliberately keeps climbing across a wipe: reusing an id
        // would collide with rows the pane still has in flight from the buffer
        // that was just discarded.
    }

    /** Persist URI edits as they are typed, debounced. */
    fun onUriChanged(value: String) {
        val link = parseServerUri(value)
        _state.value = _state.value.copy(serverUri = value, link = link)
        pendingUri = value
        saveJob?.cancel()
        saveJob = viewModelScope.launch {
            delay(SAVE_DEBOUNCE_MS)
            saveUriNow(pendingUri)
        }
    }

    /** Adopt a URI that came from the scanner or the history list. */
    fun adoptUri(value: String) {
        saveUriNow(value)
        _state.value = _state.value.copy(serverUri = value, link = parseServerUri(value))
    }

    fun removeHistory(uri: String) {
        val history = removeFromHistory(_state.value.history, uri)
        prefs.history = history
        _state.value = _state.value.copy(history = history)
    }

    fun clearHistory() {
        prefs.history = emptyList()
        _state.value = _state.value.copy(history = emptyList())
    }

    fun measureLatency() {
        if (_state.value.latencyBusy) return
        _state.value = _state.value.copy(latencyBusy = true)
        viewModelScope.launch {
            val result = withContext(Dispatchers.IO) { probeLatency() }
            _state.value = _state.value.copy(
                latencyBusy = false,
                latency = if (result.ok) "${result.milliseconds} ms" else "失败：${result.error}",
            )
        }
    }

    fun measureSpeed() {
        if (_state.value.speedBusy) return
        _state.value = _state.value.copy(speedBusy = true)
        viewModelScope.launch {
            val result = withContext(Dispatchers.IO) { runSpeedTest() }
            _state.value = _state.value.copy(
                speedBusy = false,
                speedResult = if (result.ok) {
                    val mbps = result.bytesPerSecond / (1024.0 * 1024.0) * 8
                    String.format(
                        Locale.US,
                        "%.2f MB/s（%.1f Mbps，%s，%.1fs）",
                        result.bytesPerSecond / (1024.0 * 1024.0),
                        mbps,
                        formatSize(result.bytes),
                        result.elapsedMs / 1000.0,
                    )
                } else {
                    "失败：${result.error}"
                },
            )
        }
    }

    // -----------------------------------------------------------------------
    // State subscription
    // -----------------------------------------------------------------------

    /**
     * Mirror the service's published samples into [state].
     *
     * This ViewModel used to poll the core itself — 200 ms for status, 1 s for
     * stats, 500 ms for logs — for as long as it lived, whether or not anything
     * had changed and whether or not the screen was awake. The service already
     * has to watch the tunnel, so its samples are the single source of truth;
     * the UI only reacts.
     */
    private fun observeController() {
        viewModelScope.launch {
            TunnelController.status.collect { onStatus(it) }
        }
        viewModelScope.launch {
            TunnelController.stats.collect { onStats(it) }
        }
        viewModelScope.launch {
            TunnelController.reconnect.collect { reconnect ->
                _state.value = _state.value.copy(reconnect = reconnect)
            }
        }
        viewModelScope.launch {
            TunnelController.networkChangedAt.collect { at ->
                _state.value = _state.value.copy(
                    networkBanner = at > 0L && bannerWithinWindow(at, System.currentTimeMillis()),
                )
            }
        }
    }

    private fun onStatus(status: TunnelController.TunnelStatus) {
        val now = System.currentTimeMillis()
        val running = status.code == STATUS_RUNNING
        if (running) {
            if (runningSince == 0L) runningSince = now
            markCurrentVerified()
        } else {
            runningSince = 0L
        }
        val text = when (status.code) {
            STATUS_STARTING -> if (_state.value.awaitingConsent) "等待系统 VPN 授权…" else "正在连接…"
            STATUS_RUNNING -> "已连接"
            STATUS_ERROR -> "错误：${status.error.ifBlank { "未知错误" }}"
            else -> if (TunnelController.desiredRunning.value) "连接已中断" else "未连接"
        }
        _state.value = _state.value.copy(
            statusCode = status.code,
            statusText = text,
            statusError = status.error,
            isRunning = running,
            busy = status.code == STATUS_STARTING,
            awaitingConsent = if (running) false else _state.value.awaitingConsent,
            uptimeSeconds = if (running) (now - runningSince) / 1000 else 0L,
            networkBanner = status.sampledAtMs > 0L && bannerActive(now),
            // A stale rate next to a dead tunnel reads as "still transferring".
            downRate = if (running) _state.value.downRate else 0L,
            upRate = if (running) _state.value.upRate else 0L,
        )
    }

    private fun onStats(stats: TunnelController.TunnelStats) {
        val now = System.currentTimeMillis()
        val seconds = (now - lastStatsAt) / 1000.0
        val downDelta = stats.down - lastDown
        val upDelta = stats.up - lastUp
        var downRate = _state.value.downRate
        var upRate = _state.value.upRate
        if (lastStatsAt > 0 && seconds > 0.2 && downDelta >= 0 && upDelta >= 0) {
            // Smooth over one sample so the readout does not flicker.
            downRate = (downRate * 0.4 + downDelta / seconds * 0.6).toLong()
            upRate = (upRate * 0.4 + upDelta / seconds * 0.6).toLong()
        }
        lastStatsAt = now
        lastDown = stats.down
        lastUp = stats.up
        _state.value = _state.value.copy(
            downRate = downRate,
            upRate = upRate,
            totalDown = stats.down + stats.udpDown,
            totalUp = stats.up + stats.udpUp,
            proxiedFlows = stats.routeProxy,
            directFlows = stats.routeDirect,
        )
    }

    /**
     * True while the handover banner is still inside its window.
     *
     * Expiry is checked on the samples that already arrive rather than on a
     * timer of our own: with one sample a second while running, the banner
     * disappears within a second of going stale, at no extra cost.
     */
    private fun bannerActive(now: Long): Boolean {
        val at = TunnelController.networkChangedAt.value
        if (at <= 0L) return false
        if (bannerWithinWindow(at, now)) return true
        TunnelController.clearNetworkChange()
        return false
    }

    private fun bannerWithinWindow(at: Long, now: Long): Boolean = now - at < NET_CHANGE_BANNER_MS

    // -----------------------------------------------------------------------
    // Log pump — running only while the dashboard is on screen
    // -----------------------------------------------------------------------

    /**
     * Start draining new log lines from the core.
     *
     * Logs are the one thing the service does not publish: reading the ring
     * buffer is cheap, but draining it while nothing can display it is pure
     * waste, so the pump runs between the host activity's start and stop.
     */
    fun startLogPump() {
        if (logJob?.isActive == true) return
        logJob = viewModelScope.launch {
            while (isActive) {
                pumpLogsOnce()
                delay(LOG_POLL_MS)
            }
        }
    }

    fun stopLogPump() {
        logJob?.cancel()
        logJob = null
    }

    private fun pumpLogsOnce() {
        val result = RustBridge.getLogs(logCursor)
        logCursor = result.cursor
        if (result.lines.isEmpty()) return
        val stamp = stampFormat.format(Date())
        for (line in result.lines) {
            rawLogs += LogEntry(nextLogId++, "$stamp $line")
        }
        if (rawLogs.size > LOG_RAW_LIMIT) {
            rawLogs = rawLogs.takeLast(LOG_RAW_LIMIT).toMutableList()
        }
        if (_state.value.logPaused) return
        _state.value = _state.value.copy(
            logLines = visibleLogEntries(rawLogs, _state.value.showDirectLogs),
        )
    }

    /**
     * Remember that this URI actually worked on this phone.
     *
     * Only called while running, so a tick in the history list means "proven",
     * not merely "typed once".
     */
    private fun markCurrentVerified() {
        val uri = _state.value.serverUri.trim()
        if (uri.isEmpty()) return
        val entry = _state.value.history.firstOrNull { it.uri == uri } ?: return
        if (entry.verifiedMs > 0) return
        val history = markVerified(_state.value.history, uri, System.currentTimeMillis())
        prefs.history = history
        _state.value = _state.value.copy(history = history)
    }

    private fun saveUriNow(uri: String) {
        val trimmed = uri.trim()
        if (prefs.serverUri == trimmed) return
        prefs.serverUri = trimmed
    }

    private fun sendServiceAction(
        action: String,
        extra: (Intent.() -> Unit)? = null,
    ) {
        val context = getApplication<Application>()
        val intent = Intent(context, PhantomVpnService::class.java).apply {
            this.action = action
            extra?.invoke(this)
        }
        // Starting a tunnel is a foreground service launch; prompting for, and
        // then holding, the VPN needs that contract on Android 8+.
        if (action == PhantomVpnService.ACTION_CONNECT) {
            androidx.core.content.ContextCompat.startForegroundService(context, intent)
        } else {
            context.startService(intent)
        }
    }

    override fun onCleared() {
        super.onCleared()
        stopLogPump()
    }

    private companion object {
        const val SAVE_DEBOUNCE_MS = 600L

        /** Visible log refresh cadence; the pane shows the last 200 lines. */
        const val LOG_POLL_MS = 500L

        /** Backstop only: the pane renders 200, the file keeps everything. */
        const val LOG_RAW_LIMIT = 2_000
    }
}
