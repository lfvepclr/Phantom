package co.phantom.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import co.phantom.android.PhantomUiState
import co.phantom.android.cipherLabel
import co.phantom.android.formatDuration
import co.phantom.android.formatRate
import co.phantom.android.formatSize
import co.phantom.android.linkTitle
import co.phantom.android.shortFingerprint

/**
 * Connection details: what the tunnel is, where it goes, and how it is doing.
 *
 * The raw connection string is deliberately absent — it is long, contains the
 * PSK, and tells the user nothing the rows below do not say better. Sharing it
 * is a separate, explicit action.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun InfoSheet(
    state: PhantomUiState,
    onDismiss: () -> Unit,
    onMeasureLatency: () -> Unit,
    onMeasureSpeed: () -> Unit,
    onCopyUri: () -> Unit,
    onShareQr: () -> Unit,
) {
    val colors = LocalPhantomColors.current
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                text = if (state.link.valid) linkTitle(state.link) else "未配置",
                color = colors.textPrimary,
                fontSize = 20.sp,
                fontWeight = FontWeight.Bold,
            )
            Text(state.statusText, color = colors.textSecondary, fontSize = 13.sp)
            PhantomDivider()

            KeyValueRow("地址", if (state.link.valid) "${state.link.host}:${state.link.port}" else "—")
            KeyValueRow("传输协议", if (state.link.valid) state.link.proto.uppercase() else "—")
            KeyValueRow("加密套件", if (state.link.valid) cipherLabel(state.link.cipher) else "—")
            KeyValueRow(
                "服务器密钥",
                if (state.link.valid) shortFingerprint(state.link.key) else "—",
                mono = true,
            )
            KeyValueRow(
                "预共享密钥",
                if (state.link.psk.isNotEmpty()) shortFingerprint(state.link.psk) else "—",
                mono = true,
            )
            KeyValueRow(
                "连接时长",
                if (state.isRunning) formatDuration(state.uptimeSeconds) else "—",
            )
            KeyValueRow(
                "实时速率",
                if (state.isRunning) {
                    "↓ ${formatRate(state.downRate)}   ↑ ${formatRate(state.upRate)}"
                } else {
                    "—"
                },
            )
            KeyValueRow(
                "本次流量",
                if (state.isRunning) {
                    "↓ ${formatSize(state.totalDown)}   ↑ ${formatSize(state.totalUp)}"
                } else {
                    "—"
                },
            )
            KeyValueRow("分流统计", "隧道 ${state.proxiedFlows} 条 · 直连 ${state.directFlows} 条")
            if (state.latency.isNotEmpty()) {
                KeyValueRow("链路延迟", state.latency)
            }
            if (state.speedResult.isNotEmpty()) {
                KeyValueRow("测速结果", state.speedResult)
            }

            PhantomDivider()

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = onMeasureLatency,
                    enabled = state.isRunning && !state.latencyBusy,
                    modifier = Modifier.weight(1f),
                ) {
                    Text(if (state.latencyBusy) "测延迟…" else "测延迟", fontSize = 13.sp)
                }
                OutlinedButton(
                    onClick = onMeasureSpeed,
                    enabled = state.isRunning && !state.speedBusy,
                    modifier = Modifier.weight(1f),
                ) {
                    Text(if (state.speedBusy) "测速中…" else "测速", fontSize = 13.sp)
                }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = onCopyUri,
                    enabled = state.link.valid,
                    modifier = Modifier.weight(1f).height(44.dp),
                    colors = ButtonDefaults.buttonColors(containerColor = colors.surfaceAlt),
                ) {
                    Text("复制连接串", fontSize = 13.sp, color = colors.textPrimary)
                }
                Button(
                    onClick = onShareQr,
                    enabled = state.link.valid,
                    modifier = Modifier.weight(1f).height(44.dp),
                ) {
                    Text("分享二维码", fontSize = 13.sp)
                }
            }
        }
    }
}
