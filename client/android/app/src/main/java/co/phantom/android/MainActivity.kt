package co.phantom.android

import android.Manifest
import android.content.pm.PackageManager
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.awaitCancellation
import co.phantom.android.ui.DashboardActions
import co.phantom.android.ui.DashboardScreen
import co.phantom.android.ui.InfoSheet
import co.phantom.android.ui.LocalPhantomColors
import co.phantom.android.ui.LogFullScreen
import co.phantom.android.ui.PhantomColors
import co.phantom.android.ui.PhantomTheme
import co.phantom.android.ui.QrShareDialog
import co.phantom.android.ui.ScanScreen
import co.phantom.android.ui.ServerScreen
import co.phantom.android.ui.SettingsSheet
import co.phantom.android.ui.copyToClipboard
import co.phantom.android.ui.phantomColorsFor
import co.phantom.android.ui.shareFile
import java.io.File

/**
 * The app's only activity: it hosts the four screens (dashboard, scanner,
 * server, full-screen log) and owns the two platform permissions.
 *
 * Screens are a plain state machine rather than a navigation library — there
 * are four of them, none nested, and no deep links to restore. Adding a
 * navigation dependency for that would be more moving parts than the problem
 * has, and this keeps the back button behaviour obvious.
 */
class MainActivity : ComponentActivity() {

    private val viewModel: PhantomTunnelViewModel by viewModels()

    private val vpnPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult()
    ) { result ->
        if (result.resultCode == RESULT_OK) {
            viewModel.start()
        } else {
            // Dismissing the consent dialog is not an error to be shouted
            // about; it just means no tunnel was started.
            viewModel.consentDenied()
        }
    }

    private val notificationPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { _ -> }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
                PackageManager.PERMISSION_GRANTED
            ) {
                notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        }

        setContent {
            // Lifecycle-aware: the collection stops below STARTED, so a
            // backgrounded dashboard is not even subscribed, let alone polling.
            val state by viewModel.state.collectAsStateWithLifecycle()
            val systemDark = isSystemInDarkTheme()
            // The status/navigation bars belong to the window, not to Compose,
            // so they are painted from the same three-way decision the theme
            // makes — otherwise a dark app keeps a light system bar. Keyed, so
            // the four window writes happen when the decision changes rather
            // than on every recomposition.
            LaunchedEffect(state.themeMode, systemDark) {
                applySystemBarAppearance(phantomColorsFor(state.themeMode, systemDark))
            }

            // Logs are the only state the service does not publish: draining
            // the core's ring buffer is pointless while nothing can show it, so
            // the pump runs strictly between STARTED and STOPPED.
            val lifecycleOwner = LocalLifecycleOwner.current
            LaunchedEffect(lifecycleOwner) {
                lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                    viewModel.startLogPump()
                    try {
                        awaitCancellation()
                    } finally {
                        viewModel.stopLogPump()
                    }
                }
            }

            PhantomTheme(mode = state.themeMode) {
                PhantomApp(
                    state = state,
                    viewModel = viewModel,
                    onRequestStart = {
                        // prepare() returns an intent only when consent is still
                        // needed; otherwise the system already trusts us.
                        val intent = VpnService.prepare(this@MainActivity)
                        if (intent != null) {
                            vpnPermissionLauncher.launch(intent)
                        } else {
                            viewModel.start()
                        }
                    },
                )
            }
        }
    }
}

private enum class Screen { DASHBOARD, SCAN, SERVER, FULL_LOG }

@Composable
private fun PhantomApp(
    state: PhantomUiState,
    viewModel: PhantomTunnelViewModel,
    onRequestStart: () -> Unit,
) {
    var screen by remember { mutableStateOf(Screen.DASHBOARD) }
    var showInfo by remember { mutableStateOf(false) }
    var showSettings by remember { mutableStateOf(false) }
    var showQr by remember { mutableStateOf(false) }
    val context = androidx.compose.ui.platform.LocalContext.current
    val colors = LocalPhantomColors.current

    BackHandler(enabled = screen != Screen.DASHBOARD) {
        screen = Screen.DASHBOARD
    }

    Box(modifier = Modifier.fillMaxSize().background(colors.canvas)) {
        when (screen) {
            Screen.DASHBOARD -> DashboardScreen(
                state = state,
                actions = DashboardActions(
                    onToggle = {
                        if (state.isRunning || state.busy) viewModel.stop() else onRequestStart()
                    },
                    onModeChange = viewModel::setMode,
                    onUriChange = viewModel::onUriChanged,
                    onAdoptUri = viewModel::adoptUri,
                    onRemoveHistory = viewModel::removeHistory,
                    onOpenInfo = { showInfo = true },
                    onOpenSettings = { showSettings = true },
                    onOpenServer = { screen = Screen.SERVER },
                    onOpenScan = { screen = Screen.SCAN },
                    onOpenFullLog = { screen = Screen.FULL_LOG },
                    onTogglePause = { viewModel.setLogPaused(!state.logPaused) },
                    onToggleDirectLogs = viewModel::setShowDirectLogs,
                    onClearLogs = viewModel::clearLogs,
                ),
            )

            Screen.SCAN -> ScanScreen(
                onClose = { screen = Screen.DASHBOARD },
                onScanned = { uri ->
                    viewModel.adoptUri(uri)
                    // Importing a link is not the same as trusting it: the user
                    // still presses start.
                    screen = Screen.DASHBOARD
                },
            )

            Screen.SERVER -> ServerScreen(onBack = { screen = Screen.DASHBOARD })

            Screen.FULL_LOG -> LogFullScreen(
                lines = state.logLines,
                paused = state.logPaused,
                showDirect = state.showDirectLogs,
                onTogglePause = { viewModel.setLogPaused(!state.logPaused) },
                onToggleDirect = viewModel::setShowDirectLogs,
                onClear = viewModel::clearLogs,
                onClose = { screen = Screen.DASHBOARD },
            )
        }
    }

    if (showInfo) {
        InfoSheet(
            state = state,
            onDismiss = { showInfo = false },
            onMeasureLatency = viewModel::measureLatency,
            onMeasureSpeed = viewModel::measureSpeed,
            onCopyUri = { copyToClipboard(context, state.serverUri) },
            onShareQr = {
                showInfo = false
                showQr = true
            },
        )
    }

    if (showSettings) {
        SettingsSheet(
            state = state,
            onDismiss = { showSettings = false },
            onThemeChange = viewModel::setThemeMode,
            onTunTraceChange = viewModel::setTunTrace,
            onExportLogs = {
                // Prefer the session log; the TUN trace only exists when the
                // user opted in, and it is far larger.
                val session = File(context.filesDir, LOG_FILE)
                val trace = File(context.filesDir, TRACE_FILE)
                when {
                    session.exists() -> shareFile(context, session, "text/plain", "Phantom 日志")
                    trace.exists() -> shareFile(context, trace, "text/plain", "Phantom TUN 追踪")
                }
            },
            onClearHistory = viewModel::clearHistory,
            onUserRulesChange = viewModel::setUserRules,
        )
    }

    if (showQr) {
        QrShareDialog(uri = state.serverUri, onDismiss = { showQr = false })
    }
}

/**
 * Paint the two system bars from the app palette.
 *
 * Compose only owns the content area; without this the bars follow the
 * platform theme and a dark app ends up framed by a light status bar.
 */
private fun ComponentActivity.applySystemBarAppearance(colors: PhantomColors) {
    val canvas = colors.canvas.toArgb()
    window.statusBarColor = canvas
    window.navigationBarColor = canvas
    val dark = colors.canvas.luminance() < 0.5f
    val insets = WindowCompat.getInsetsController(window, window.decorView)
    insets.isAppearanceLightStatusBars = !dark
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        insets.isAppearanceLightNavigationBars = !dark
    }
}
