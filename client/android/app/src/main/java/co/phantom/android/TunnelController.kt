package co.phantom.android

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Process-wide tunnel state, shared by the UI and the [PhantomVpnService].
 *
 * The service outlives the UI (it is a foreground service, and the user
 * routinely switches away mid-session), so neither "did the user ask for a
 * tunnel?" nor "what is the core actually doing?" can live in a ViewModel.
 * Everything the dashboard renders is published here by the service, which is
 * the only component that keeps running:
 *
 *  * intent — [desiredRunning], [reconnect], [networkChangedAt], written by the
 *    UI and the service's own reconnect logic;
 *  * observations — [status], [stats], written only by the service.
 *
 * The UI therefore *subscribes* instead of polling. That matters for battery:
 * a screen that is off must not produce a single periodic wake, and the 200 ms
 * status / 1 s stats / 500 ms log loops the dashboard used to run did exactly
 * that whenever an activity happened to be alive.
 */
object TunnelController {

    /** Reconnect bookkeeping rendered by the "网络已切换" banner. */
    data class ReconnectState(
        /** Seconds until the next automatic attempt; 0 when none is pending. */
        val inSeconds: Int = 0,
        val attempts: Int = 0,
        /** True once the back-off schedule is used up; the user must retry. */
        val exhausted: Boolean = false,
    )

    /**
     * A sample of what the core last reported.
     *
     * [sampledAtMs] is payload rather than metadata on purpose: two consecutive
     * samples of a healthy tunnel differ in nothing else, and a `StateFlow`
     * conflates equal values away — the UI would never see the 1 Hz heartbeat
     * that keeps the uptime readout and the banner expiry ticking.
     */
    data class TunnelStatus(
        val code: Int = STATUS_IDLE,
        val error: String = "",
        val sampledAtMs: Long = 0L,
    )

    /** Traffic counters as of the last sample. */
    data class TunnelStats(
        val down: Long = 0,
        val up: Long = 0,
        val udpDown: Long = 0,
        val udpUp: Long = 0,
        val routeProxy: Long = 0,
        val routeDirect: Long = 0,
    )

    private val _desiredRunning = MutableStateFlow(false)

    /** True while the user wants a tunnel, regardless of its current health. */
    val desiredRunning: StateFlow<Boolean> = _desiredRunning.asStateFlow()

    private val _reconnect = MutableStateFlow(ReconnectState())
    val reconnect: StateFlow<ReconnectState> = _reconnect.asStateFlow()

    /** Wall-clock time of the last radio handover, for the transient banner. */
    private val _networkChangedAt = MutableStateFlow(0L)
    val networkChangedAt: StateFlow<Long> = _networkChangedAt.asStateFlow()

    private val _status = MutableStateFlow(TunnelStatus())
    val status: StateFlow<TunnelStatus> = _status.asStateFlow()

    private val _stats = MutableStateFlow(TunnelStats())
    val stats: StateFlow<TunnelStats> = _stats.asStateFlow()

    fun setDesired(running: Boolean) {
        _desiredRunning.value = running
        if (!running) {
            // The core is about to be torn down; do not leave the UI rendering
            // a "connected" frame that no longer exists.
            _status.value = TunnelStatus()
            _stats.value = TunnelStats()
        }
    }

    fun noteNetworkChange(atMs: Long) {
        _networkChangedAt.value = atMs
    }

    fun clearNetworkChange() {
        _networkChangedAt.value = 0L
    }

    fun updateReconnect(state: ReconnectState) {
        _reconnect.value = state
    }

    fun resetReconnect() {
        _reconnect.value = ReconnectState()
    }

    /**
     * Publish one sample read from the core.
     *
     * Called by [PhantomVpnService] on every transition and once per second
     * while it is watching, so the UI never has to ask.
     */
    fun publishStatus(code: Int, error: String, atMs: Long) {
        _status.value = TunnelStatus(code = code, error = error, sampledAtMs = atMs)
    }

    /** Publish the counters that go with the current status sample. */
    fun publishStats(stats: TunnelStats) {
        _stats.value = stats
    }
}

/** Back-off between automatic reconnect attempts, mirroring the HarmonyOS client. */
val RECONNECT_DELAYS_MS: LongArray = longArrayOf(3_000, 10_000, 30_000)

/** How long the "network changed" banner stays on screen. */
const val NET_CHANGE_BANNER_MS: Long = 15_000

/** Lines rendered in the log pane (the mirrored file keeps far more). */
const val LOG_VIEW_LINES: Int = 200
