package co.phantom.android.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import co.phantom.android.RustBridge
import co.phantom.android.ServerStatus
import kotlinx.coroutines.delay
import java.io.File

/** Cipher presets, in the same order HarmonyOS offers them. */
private val CIPHER_OPTIONS = listOf(
    "auto" to "自动",
    "aes-256-gcm" to "AES-256",
    "chacha20-poly1305" to "ChaCha20",
)

private val PROTO_OPTIONS = listOf(
    "tcp" to "TCP",
    "quic" to "QUIC",
)

/**
 * Default listen port for the phone-as-server mode.
 *
 * Not 443, which is what the desktop and HarmonyOS builds default to: an
 * Android app has no `CAP_NET_BIND_SERVICE`, so binding a privileged port
 * fails with `EACCES` (`Permission denied`). The core's "0 means the default"
 * convention therefore cannot be used here either — it resolves to 443 — so
 * the page always passes a concrete, unprivileged port.
 */
private const val DEFAULT_SERVER_PORT = 8443

/** Ports below this need privileges the app does not have. */
private const val MIN_UNPRIVILEGED_PORT = 1024

/** Refresh cadence while the embedded server is starting or listening. */
private const val SERVER_POLL_MS = 500L

/**
 * Run the Phantom *server* on the phone and hand the generated connection
 * string to other devices on the same network.
 *
 * This is the mirror of the "server" panel on the HarmonyOS client: same
 * fields, same presets, same status contract, and the same QR payload — a link
 * generated here must import on either client.
 */
@Composable
fun ServerScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val colors = LocalPhantomColors.current
    var port by rememberSaveable { mutableStateOf(DEFAULT_SERVER_PORT.toString()) }
    var cipher by rememberSaveable { mutableStateOf("auto") }
    var proto by rememberSaveable { mutableStateOf("tcp") }
    var status by remember { mutableStateOf(ServerStatus.IDLE) }
    var uri by remember { mutableStateOf("") }
    var lastError by remember { mutableStateOf("") }

    val running = status == ServerStatus.RUNNING || status == ServerStatus.STARTING

    /**
     * Read the listener's state once.
     *
     * The server keeps running after this page is popped, and the URI
     * `serverStart` returned dies with the composition that received it — so
     * the code shown here is always re-derived from Rust instead of being held
     * as the only copy.
     */
    fun refreshServer() {
        val current = RustBridge.serverStatus()
        status = current
        if (current == ServerStatus.ERROR) {
            lastError = RustBridge.serverLastError()
        }
        if (current == ServerStatus.RUNNING) {
            val live = RustBridge.serversUri()
            if (live.isNotEmpty()) uri = live
        } else {
            uri = ""
        }
    }

    LaunchedEffect(Unit) {
        // The server outlives this page, so it may already be up before the
        // first frame. One read on entry is enough to find out.
        refreshServer()
    }

    // ...and then only while there is something that can still change: a
    // steady 2 Hz read of a stopped listener was pure waste, and the entry
    // read above already covers "it was running when I got here".
    LaunchedEffect(running) {
        if (!running) return@LaunchedEffect
        while (true) {
            delay(SERVER_POLL_MS)
            refreshServer()
        }
    }

    /** The port to hand to the core, or `null` when the field is unusable. */
    fun requestedPort(): Int? =
        port.toIntOrNull()?.takeIf { it in MIN_UNPRIVILEGED_PORT..65535 }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(colors.canvas)
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = "内嵌服务器",
                color = colors.textPrimary,
                fontSize = 20.sp,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onBack) {
                Text("返回", fontSize = 13.sp, color = colors.textSecondary)
            }
        }

        Text(
            text = "在本机运行 Phantom 服务器，把生成连接串（含二维码）分享给同一局域网的设备。",
            color = colors.textSecondary,
            fontSize = 12.sp,
        )

        Text(
            text = when (status) {
                ServerStatus.STARTING -> "正在启动…"
                ServerStatus.RUNNING -> "监听中"
                ServerStatus.ERROR -> if (lastError.isEmpty()) "错误" else "错误：$lastError"
                else -> "已停止"
            },
            color = when (status) {
                ServerStatus.RUNNING -> colors.success
                ServerStatus.ERROR -> colors.danger
                else -> colors.textSecondary
            },
            fontSize = 13.sp,
        )

        // HarmonyOS hides the knobs while the listener is up: port, cipher and
        // protocol are baked into the running server, so editing them there
        // would only suggest a change that cannot take effect.
        if (!running) {
            OutlinedTextField(
                value = port,
                onValueChange = { port = it.filter(Char::isDigit).take(5) },
                label = { Text("端口（需 ≥ $MIN_UNPRIVILEGED_PORT）") },
                singleLine = true,
                isError = requestedPort() == null,
                modifier = Modifier.fillMaxWidth(),
            )
            if (requestedPort() == null) {
                Text(
                    // Say why before the start button fails with a bare
                    // "Permission denied" the user cannot act on.
                    text = "端口要 ≥ $MIN_UNPRIVILEGED_PORT；低于此值的端口需要 root 权限，" +
                        "Android 会拒绝绑定",
                    color = colors.danger,
                    fontSize = 11.sp,
                )
            }
            ServerChoiceRow(
                label = "加密套件",
                options = CIPHER_OPTIONS,
                selected = cipher,
                onSelect = { cipher = it },
            )
            ServerChoiceRow(
                label = "协议",
                options = PROTO_OPTIONS,
                selected = proto,
                onSelect = { proto = it },
            )
        }

        Button(
            onClick = {
                if (status == ServerStatus.RUNNING || status == ServerStatus.STARTING) {
                    RustBridge.serverStop()
                    uri = ""
                } else {
                    // Same subdirectory HarmonyOS uses, so a server.toml moved
                    // between the two clients lands in an expected place.
                    val workDir = File(context.filesDir, "server").apply { mkdirs() }
                    val started = RustBridge.serverStart(
                        workDir.absolutePath,
                        requestedPort() ?: DEFAULT_SERVER_PORT,
                        cipher,
                        proto,
                    )
                    if (started.isEmpty()) {
                        lastError = RustBridge.serverLastError()
                    } else {
                        uri = started
                    }
                }
            },
            enabled = running || requestedPort() != null,
            modifier = Modifier.fillMaxWidth().height(48.dp),
            shape = RoundedCornerShape(14.dp),
            colors = ButtonDefaults.buttonColors(
                containerColor = if (status == ServerStatus.RUNNING || status == ServerStatus.STARTING) {
                    colors.danger
                } else {
                    colors.brand
                },
                contentColor = colors.onBrand,
            ),
        ) {
            Text(
                text = when (status) {
                    ServerStatus.RUNNING -> "停止服务器"
                    ServerStatus.STARTING -> "正在启动…"
                    else -> "启动服务器"
                },
                fontSize = 15.sp,
            )
        }

        if (uri.isNotEmpty() && status == ServerStatus.RUNNING) {
            PhantomCard {
                Text("连接串", color = colors.textSecondary, fontSize = 12.sp)
                Text(
                    text = hostOf(uri),
                    color = colors.textPrimary,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.Medium,
                )
                Text(
                    text = uri,
                    color = colors.textSecondary,
                    fontSize = 12.sp,
                    fontFamily = FontFamily.Monospace,
                )
                val qr = remember(uri) { encodeQr(uri, 640) }
                if (qr != null) {
                    Box(modifier = Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                        Image(
                            bitmap = qr,
                            contentDescription = "连接二维码",
                            modifier = Modifier.size(220.dp).clip(RoundedCornerShape(8.dp)),
                        )
                    }
                }
                Spacer(modifier = Modifier.height(4.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(
                        onClick = { copyToClipboard(context, uri) },
                        modifier = Modifier.weight(1f),
                    ) {
                        Text("复制", fontSize = 13.sp)
                    }
                    OutlinedButton(
                        onClick = { shareText(context, uri) },
                        modifier = Modifier.weight(1f),
                    ) {
                        Text("分享", fontSize = 13.sp)
                    }
                }
            }
        }
    }
}

/**
 * A row of mutually exclusive presets.
 *
 * A free-text field would let the user type a cipher the core does not know
 * and only find out from `serverLastError`; presets remove that round trip and
 * match what the HarmonyOS page offers.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ServerChoiceRow(
    label: String,
    options: List<Pair<String, String>>,
    selected: String,
    onSelect: (String) -> Unit,
) {
    val colors = LocalPhantomColors.current
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(label, color = colors.textSecondary, fontSize = 12.sp)
        SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
            options.forEachIndexed { index, (value, text) ->
                SegmentedButton(
                    selected = selected == value,
                    onClick = { onSelect(value) },
                    shape = SegmentedButtonDefaults.itemShape(index = index, count = options.size),
                    colors = SegmentedButtonDefaults.colors(
                        activeContainerColor = colors.brand,
                        activeContentColor = colors.onBrand,
                        inactiveContainerColor = colors.surfaceAlt,
                        inactiveContentColor = colors.textSecondary,
                    ),
                ) {
                    Text(text, fontSize = 12.sp)
                }
            }
        }
    }
}

/** Extract `host:port` from `phantom://key@host:port?...#name`. */
private fun hostOf(uri: String): String {
    val at = uri.indexOf('@')
    if (at < 0) return ""
    val q = uri.indexOf('?', at)
    return if (q > 0) uri.substring(at + 1, q) else uri.substring(at + 1)
}
