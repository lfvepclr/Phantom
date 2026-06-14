package co.phantom.android

import android.Manifest
import android.content.pm.PackageManager
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle

class MainActivity : ComponentActivity() {

    private val viewModel: PhantomTunnelViewModel by viewModels()

    private var pendingStart: Pair<String, String>? = null

    private val vpnPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult()
    ) { result ->
        if (result.resultCode == RESULT_OK) {
            pendingStart?.let { (uri, mode) ->
                viewModel.start(uri, mode)
            }
        }
        pendingStart = null
    }

    private val notificationPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { _ -> }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            when (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)) {
                PackageManager.PERMISSION_GRANTED -> {}
                else -> notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        }

        setContent {
            val darkTheme = isSystemInDarkTheme()
            MaterialTheme(
                colorScheme = if (darkTheme) darkColorScheme() else lightColorScheme()
            ) {
                PhantomApp(
                    viewModel = viewModel,
                    onStartVpn = { uri, mode ->
                        val intent = VpnService.prepare(this@MainActivity)
                        if (intent != null) {
                            pendingStart = uri to mode
                            vpnPermissionLauncher.launch(intent)
                        } else {
                            viewModel.start(uri, mode)
                        }
                    }
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PhantomApp(
    viewModel: PhantomTunnelViewModel,
    onStartVpn: (String, String) -> Unit
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val statusText by viewModel.statusText.collectAsStateWithLifecycle()
    val logs by viewModel.logs.collectAsStateWithLifecycle()
    val isRunning by viewModel.isRunning.collectAsStateWithLifecycle()

    var serverURI by remember { mutableStateOf("") }
    var proxyMode by remember { mutableStateOf("smart") }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(
                            modifier = Modifier
                                .size(28.dp)
                                .clip(RoundedCornerShape(6.dp))
                                .background(MaterialTheme.colorScheme.primary),
                            contentAlignment = Alignment.Center
                        ) {
                            Text(
                                text = "P",
                                color = MaterialTheme.colorScheme.onPrimary,
                                style = MaterialTheme.typography.titleMedium
                            )
                        }
                        Spacer(modifier = Modifier.width(10.dp))
                        Text("Phantom")
                    }
                }
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 20.dp, vertical = 16.dp),
            horizontalAlignment = Alignment.CenterHorizontally
        ) {
            StatusPill(state = state)

            Spacer(modifier = Modifier.height(20.dp))

            OutlinedTextField(
                value = serverURI,
                onValueChange = { if (!isRunning) serverURI = it },
                label = { Text("Server URI") },
                placeholder = { Text("phantom://key@host:port") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
                enabled = !isRunning
            )

            Spacer(modifier = Modifier.height(16.dp))

            Text(
                text = "Mode",
                style = MaterialTheme.typography.labelLarge,
                modifier = Modifier.fillMaxWidth()
            )
            Spacer(modifier = Modifier.height(6.dp))
            SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
                listOf("proxy" to "Global", "smart" to "Auto", "direct" to "Direct").forEachIndexed { index, (value, label) ->
                    SegmentedButton(
                        selected = proxyMode == value,
                        onClick = { if (!isRunning) proxyMode = value },
                        shape = SegmentedButtonDefaults.itemShape(index = index, count = 3),
                        enabled = !isRunning
                    ) {
                        Text(label)
                    }
                }
            }

            Spacer(modifier = Modifier.height(20.dp))

            Text(
                text = statusText,
                style = MaterialTheme.typography.bodyLarge,
                fontFamily = FontFamily.Monospace,
                color = when (state) {
                    TunnelState.Error -> MaterialTheme.colorScheme.error
                    TunnelState.Connected -> MaterialTheme.colorScheme.primary
                    else -> LocalContentColor.current
                }
            )

            Spacer(modifier = Modifier.height(16.dp))

            Button(
                onClick = {
                    if (isRunning) {
                        viewModel.stop()
                    } else if (serverURI.isNotBlank()) {
                        onStartVpn(serverURI, proxyMode)
                    }
                },
                modifier = Modifier
                    .fillMaxWidth()
                    .height(50.dp),
                colors = ButtonDefaults.buttonColors(
                    containerColor = if (isRunning) MaterialTheme.colorScheme.error
                    else MaterialTheme.colorScheme.primary
                )
            ) {
                Text(
                    text = if (isRunning) "Stop Tunnel" else "Start Tunnel",
                    style = MaterialTheme.typography.titleMedium
                )
            }

            Spacer(modifier = Modifier.height(20.dp))

            Text(
                text = "Connection Logs",
                style = MaterialTheme.typography.labelLarge,
                modifier = Modifier.fillMaxWidth()
            )
            Spacer(modifier = Modifier.height(6.dp))
            LogView(
                logs = logs,
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f)
                    .clip(RoundedCornerShape(12.dp))
                    .background(MaterialTheme.colorScheme.surfaceVariant)
                    .padding(12.dp)
            )
        }
    }
}

@Composable
private fun StatusPill(state: TunnelState) {
    val (bg, textColor, label) = when (state) {
        TunnelState.Idle -> Triple(
            MaterialTheme.colorScheme.surfaceVariant,
            LocalContentColor.current.copy(alpha = 0.7f),
            "Idle"
        )
        TunnelState.Connecting -> Triple(
            MaterialTheme.colorScheme.secondaryContainer,
            MaterialTheme.colorScheme.onSecondaryContainer,
            "Connecting"
        )
        TunnelState.Connected -> Triple(
            Color(0xFFD1FADF),
            Color(0xFF0F5132),
            "Connected"
        )
        TunnelState.Error -> Triple(
            MaterialTheme.colorScheme.errorContainer,
            MaterialTheme.colorScheme.onErrorContainer,
            "Error"
        )
    }

    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(50))
            .background(bg)
            .padding(horizontal = 16.dp, vertical = 6.dp)
    ) {
        Text(
            text = label,
            color = textColor,
            style = MaterialTheme.typography.labelLarge
        )
    }
}

@Composable
private fun LogView(logs: List<String>, modifier: Modifier = Modifier) {
    val listState = rememberLazyListState()

    LaunchedEffect(logs.size) {
        if (logs.isNotEmpty()) {
            listState.animateScrollToItem(logs.lastIndex)
        }
    }

    LazyColumn(
        state = listState,
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(2.dp)
    ) {
        items(logs) { line ->
            val color = when {
                line.contains("ERROR", ignoreCase = true) -> MaterialTheme.colorScheme.error
                line.contains("WARN", ignoreCase = true) -> Color(0xFFEA9A3E)
                else -> LocalContentColor.current
            }
            Text(
                text = line,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                color = color,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth()
            )
        }
    }
}
