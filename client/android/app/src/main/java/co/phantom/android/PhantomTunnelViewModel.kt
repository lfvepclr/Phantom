package co.phantom.android

import android.app.Application
import android.content.Intent
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn

enum class TunnelState {
    Idle,
    Connecting,
    Connected,
    Error
}

class PhantomTunnelViewModel(application: Application) : AndroidViewModel(application) {

    private val _state = MutableStateFlow(TunnelState.Idle)
    val state: StateFlow<TunnelState> = _state.asStateFlow()

    private val _statusText = MutableStateFlow("Idle")
    val statusText: StateFlow<String> = _statusText.asStateFlow()

    private val _logs = MutableStateFlow<List<String>>(emptyList())
    val logs: StateFlow<List<String>> = _logs.asStateFlow()

    /** True when the tunnel is fully connected. */
    val isRunning: StateFlow<Boolean> = state
        .map { it == TunnelState.Connected }
        .stateIn(
            scope = viewModelScope,
            started = SharingStarted.WhileSubscribed(5000),
            initialValue = false
        )

    private var statusJob: Job? = null
    private var logJob: Job? = null
    private var logCursor: Long = 0

    init {
        startPolling()
    }

    /** Start polling Rust state and logs. */
    fun startPolling() {
        stopPolling()

        statusJob = viewModelScope.launch {
            while (isActive) {
                pollStatusOnce()
                delay(200)
            }
        }

        logJob = viewModelScope.launch {
            while (isActive) {
                pollLogsOnce()
                delay(500)
            }
        }
    }

    /** Stop polling. */
    fun stopPolling() {
        statusJob?.cancel()
        logJob?.cancel()
        statusJob = null
        logJob = null
    }

    /** Start the VPN service with the supplied URI and proxy mode. */
    fun start(serverURI: String, proxyMode: String) {
        val intent = Intent(getApplication(), PhantomVpnService::class.java).apply {
            action = PhantomVpnService.ACTION_CONNECT
            putExtra(PhantomVpnService.EXTRA_SERVER_URI, serverURI)
            putExtra(PhantomVpnService.EXTRA_PROXY_MODE, proxyMode)
        }
        getApplication<Application>().startService(intent)
    }

    /** Request the VPN service to disconnect. */
    fun stop() {
        val intent = Intent(getApplication(), PhantomVpnService::class.java).apply {
            action = PhantomVpnService.ACTION_DISCONNECT
        }
        getApplication<Application>().startService(intent)
    }

    private fun pollStatusOnce() {
        val code = RustBridge.getStatus()
        val newState = when (code) {
            1 -> TunnelState.Connecting
            2 -> TunnelState.Connected
            3 -> TunnelState.Error
            else -> TunnelState.Idle
        }
        _state.value = newState
        _statusText.value = when (newState) {
            TunnelState.Idle -> "Idle"
            TunnelState.Connecting -> "Connecting..."
            TunnelState.Connected -> "Connected"
            TunnelState.Error -> RustBridge.getLastError().ifBlank { "Error" }
        }
    }

    private fun pollLogsOnce() {
        val result = RustBridge.getLogs(logCursor)
        logCursor = result.cursor
        if (result.lines.isNotEmpty()) {
            _logs.value = (_logs.value + result.lines).takeLast(200)
        }
    }

    override fun onCleared() {
        super.onCleared()
        stopPolling()
    }
}
