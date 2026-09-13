package co.phantom.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import co.phantom.android.PhantomUiState
import co.phantom.android.ProxyMode
import co.phantom.android.STATUS_ERROR
import co.phantom.android.STATUS_RUNNING
import co.phantom.android.STATUS_STARTING
import co.phantom.android.formatDuration
import co.phantom.android.formatRate
import co.phantom.android.formatSize
import co.phantom.android.linkSummary
import co.phantom.android.linkTitle
import co.phantom.android.relativeTime

/** Everything the dashboard can ask the host to do. */
data class DashboardActions(
    val onToggle: () -> Unit,
    val onModeChange: (ProxyMode) -> Unit,
    val onUriChange: (String) -> Unit,
    val onAdoptUri: (String) -> Unit,
    val onRemoveHistory: (String) -> Unit,
    val onOpenInfo: () -> Unit,
    val onOpenSettings: () -> Unit,
    val onOpenServer: () -> Unit,
    val onOpenScan: () -> Unit,
    val onOpenFullLog: () -> Unit,
    val onTogglePause: () -> Unit,
    val onToggleDirectLogs: (Boolean) -> Unit,
    val onClearLogs: () -> Unit,
)

/**
 * The dashboard: status, the connection, and the log — in that order.
 *
 * The connection string itself is collapsed once something is configured: the
 * user reads the server, not the opaque key, and the raw string only matters
 * while typing or scanning it in.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DashboardScreen(
    state: PhantomUiState,
    actions: DashboardActions,
    modifier: Modifier = Modifier,
) {
    val colors = LocalPhantomColors.current
    var inputExpanded by remember(state.link.valid) { mutableStateOf(!state.link.valid) }
    var historyOpen by remember { mutableStateOf(false) }

    Column(modifier = modifier.fillMaxSize().background(colors.canvas)) {
        TopAppBar(
            title = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(
                        modifier = Modifier
                            .size(26.dp)
                            .clip(RoundedCornerShape(8.dp))
                            .background(colors.brand),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text("P", color = colors.onBrand, fontSize = 14.sp, fontWeight = FontWeight.Bold)
                    }
                    Spacer(modifier = Modifier.width(10.dp))
                    Text("Phantom", fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
                }
            },
            actions = {
                TextButton(onClick = actions.onOpenServer) {
                    Text("服务端", fontSize = 13.sp, color = colors.textSecondary)
                }
                IconButton(onClick = actions.onOpenSettings) {
                    Icon(Icons.Filled.Settings, contentDescription = "设置", tint = colors.textSecondary)
                }
            },
        )

        Column(
            modifier = Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (state.networkBanner || state.reconnect.attempts > 0) {
                NetworkBanner(state = state)
            }

            ServerCard(state = state, onClick = actions.onOpenInfo)

            Button(
                onClick = actions.onToggle,
                enabled = state.isRunning || state.busy || state.link.valid,
                modifier = Modifier.fillMaxWidth().height(50.dp),
                shape = RoundedCornerShape(14.dp),
                colors = ButtonDefaults.buttonColors(
                    containerColor = if (state.isRunning || state.busy) colors.danger else colors.brand,
                    contentColor = colors.onBrand,
                ),
            ) {
                Text(
                    text = when {
                        state.isRunning -> "断开连接"
                        state.busy -> "正在连接…"
                        else -> "启动连接"
                    },
                    fontSize = 16.sp,
                    fontWeight = FontWeight.Medium,
                )
            }

            ModeSegment(state = state, onModeChange = actions.onModeChange)

            PhantomCard {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(
                        modifier = Modifier
                            .weight(1f)
                            .clickable { inputExpanded = !inputExpanded },
                    ) {
                        Text(
                            text = if (inputExpanded) "连接串" else historyLabel(state),
                            color = colors.textPrimary,
                            fontSize = 15.sp,
                            fontWeight = FontWeight.Medium,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            text = if (inputExpanded) "支持粘贴 / 扫码 / 历史记录" else "手动输入或扫码添加",
                            color = colors.textTertiary,
                            fontSize = 11.sp,
                        )
                    }
                    TextButton(onClick = actions.onOpenScan) {
                        Text("扫码", fontSize = 13.sp, color = colors.brand)
                    }
                    Box {
                        IconButton(onClick = { historyOpen = true }) {
                            Icon(
                                Icons.Filled.KeyboardArrowDown,
                                contentDescription = "历史记录",
                                tint = if (state.history.isEmpty()) colors.textTertiary else colors.textSecondary,
                            )
                        }
                        HistoryMenu(
                            expanded = historyOpen,
                            state = state,
                            onDismiss = { historyOpen = false },
                            onPick = {
                                historyOpen = false
                                actions.onAdoptUri(it)
                            },
                            onRemove = actions.onRemoveHistory,
                        )
                    }
                }

                if (inputExpanded) {
                    OutlinedTextField(
                        value = state.serverUri,
                        onValueChange = actions.onUriChange,
                        enabled = !state.isRunning,
                        singleLine = true,
                        placeholder = { Text("phantom://key@host:port", fontSize = 13.sp) },
                        modifier = Modifier.fillMaxWidth(),
                    )
                    if (state.serverUri.isNotBlank() && !state.link.valid) {
                        Text(
                            text = "连接串格式不正确：需要 phantom://<密钥>@<主机>:<端口>",
                            color = colors.danger,
                            fontSize = 11.sp,
                        )
                    }
                }
            }

            LogPanel(
                lines = state.logLines,
                paused = state.logPaused,
                showDirect = state.showDirectLogs,
                onTogglePause = actions.onTogglePause,
                onToggleDirect = actions.onToggleDirectLogs,
                onClear = actions.onClearLogs,
                onExpand = actions.onOpenFullLog,
                modifier = Modifier.height(220.dp),
            )

            Text(
                text = hintFor(state),
                color = colors.textTertiary,
                fontSize = 11.sp,
                modifier = Modifier.padding(bottom = 12.dp),
            )
        }
    }
}

@Composable
private fun ServerCard(state: PhantomUiState, onClick: () -> Unit) {
    val colors = LocalPhantomColors.current
    PhantomCard(onClick = onClick) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = if (state.link.valid) linkTitle(state.link) else "添加连接",
                color = if (state.link.valid) colors.textPrimary else colors.brand,
                fontSize = 17.sp,
                fontWeight = FontWeight.Medium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            StatusPill(
                label = shortStatus(state),
                foreground = statusForeground(state),
                background = statusBackground(state),
            )
        }
        if (state.link.valid) {
            Text(
                text = linkSummary(state.link),
                color = colors.textSecondary,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (state.isRunning) {
            Row(horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                Text("↓ ${formatRate(state.downRate)}", color = colors.success, fontSize = 12.sp)
                Text("↑ ${formatRate(state.upRate)}", color = colors.success, fontSize = 12.sp)
                Text(
                    text = "隧道 ${state.proxiedFlows} · 直连 ${state.directFlows}",
                    color = colors.textSecondary,
                    fontSize = 12.sp,
                )
            }
        }
    }
}

@Composable
private fun NetworkBanner(state: PhantomUiState) {
    val colors = LocalPhantomColors.current
    val text = when {
        state.reconnect.exhausted -> "自动重连已停止，请手动点击「启动连接」"
        state.reconnect.inSeconds > 0 ->
            "${state.reconnect.inSeconds} 秒后自动重连（第 ${state.reconnect.attempts} 次）"
        state.reconnect.attempts > 0 -> "正在重连（第 ${state.reconnect.attempts} 次）…"
        else -> "网络已切换，连接已重建"
    }
    Text(
        text = text,
        color = colors.warning,
        fontSize = 12.sp,
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(colors.warningSoft)
            .padding(horizontal = 12.dp, vertical = 8.dp),
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ModeSegment(state: PhantomUiState, onModeChange: (ProxyMode) -> Unit) {
    val colors = LocalPhantomColors.current
    SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
        ProxyMode.entries.forEachIndexed { index, mode ->
            SegmentedButton(
                selected = state.mode == mode,
                onClick = { onModeChange(mode) },
                enabled = !state.isRunning,
                shape = SegmentedButtonDefaults.itemShape(index = index, count = ProxyMode.entries.size),
                // Material's default paints the selection lavender, which reads
                // as "disabled" next to the brand-blue start button — and once
                // the tunnel is up the segment *is* disabled, so the default
                // would repaint the live selection grey. HarmonyOS blocks the
                // taps but keeps the brand colour, and the two apps should not
                // disagree about what "connected" looks like.
                colors = SegmentedButtonDefaults.colors(
                    activeContainerColor = colors.brand,
                    activeContentColor = colors.onBrand,
                    inactiveContainerColor = colors.surface,
                    inactiveContentColor = colors.textSecondary,
                    disabledActiveContainerColor = colors.brand,
                    disabledActiveContentColor = colors.onBrand,
                    disabledInactiveContainerColor = colors.surface,
                    disabledInactiveContentColor = colors.textSecondary,
                ),
            ) {
                Text(mode.label, fontSize = 13.sp)
            }
        }
    }
    Text(
        text = state.mode.hint,
        color = colors.textTertiary,
        fontSize = 11.sp,
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun HistoryMenu(
    expanded: Boolean,
    state: PhantomUiState,
    onDismiss: () -> Unit,
    onPick: (String) -> Unit,
    onRemove: (String) -> Unit,
) {
    val colors = LocalPhantomColors.current
    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        if (state.history.isEmpty()) {
            DropdownMenuItem(
                text = { Text("暂无历史记录", fontSize = 13.sp, color = colors.textTertiary) },
                onClick = onDismiss,
            )
            return@DropdownMenu
        }
        val now = System.currentTimeMillis()
        for (entry in state.history) {
            val link = co.phantom.android.parseServerUri(entry.uri)
            DropdownMenuItem(
                text = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        // A tick means "this one actually worked here", not
                        // merely "it was typed once".
                        Icon(
                            imageVector = Icons.Filled.Check,
                            contentDescription = null,
                            tint = if (entry.verifiedMs > 0) colors.success else Color.Transparent,
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(modifier = Modifier.width(6.dp))
                        Column {
                            Text(
                                text = linkTitle(link),
                                fontSize = 14.sp,
                                color = colors.textPrimary,
                            )
                            Text(
                                text = "${linkSummary(link)} · ${relativeTime(entry.lastUsedMs, now)}",
                                fontSize = 11.sp,
                                color = colors.textTertiary,
                            )
                        }
                    }
                },
                trailingIcon = {
                    IconButton(onClick = { onRemove(entry.uri) }, modifier = Modifier.size(28.dp)) {
                        Icon(
                            Icons.Filled.Delete,
                            contentDescription = "删除这条记录",
                            tint = colors.textTertiary,
                            modifier = Modifier.size(16.dp),
                        )
                    }
                },
                onClick = { onPick(entry.uri) },
            )
        }
    }
}

private fun historyLabel(state: PhantomUiState): String =
    if (state.history.isEmpty()) {
        "连接串"
    } else {
        "连接串 · 历史 ${state.history.size} 条"
    }

private fun shortStatus(state: PhantomUiState): String = when (state.statusCode) {
    STATUS_RUNNING -> "已连接"
    STATUS_STARTING -> "连接中"
    STATUS_ERROR -> "错误"
    else -> "未连接"
}

@Composable
private fun statusForeground(state: PhantomUiState): Color {
    val colors = LocalPhantomColors.current
    return when (state.statusCode) {
        STATUS_RUNNING -> colors.success
        STATUS_ERROR -> colors.danger
        STATUS_STARTING -> colors.warning
        else -> colors.textSecondary
    }
}

@Composable
private fun statusBackground(state: PhantomUiState): Color {
    val colors = LocalPhantomColors.current
    return when (state.statusCode) {
        STATUS_RUNNING -> colors.successSoft
        STATUS_ERROR -> colors.dangerSoft
        STATUS_STARTING -> colors.warningSoft
        else -> colors.neutralSoft
    }
}

private fun hintFor(state: PhantomUiState): String = when {
    state.isRunning -> "已连接 ${formatDuration(state.uptimeSeconds)} · 本次 ↓ ${formatSize(state.totalDown)} ↑ ${formatSize(state.totalUp)}"
    state.busy -> "点击启动后，系统会请求 VPN 授权"
    state.link.valid -> "点击「启动连接」建立隧道；智能模式仅白名单域名走服务器"
    else -> "先添加连接串，或从另一台设备扫描二维码导入"
}
