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
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import co.phantom.android.PhantomUiState
import co.phantom.android.ThemeMode
import co.phantom.android.ui.WhitelistEditor

/**
 * Settings: the switches that do not belong on the dashboard.
 *
 * Theme, diagnostics and the explanation of what the built-in whitelist does
 * live here so the dashboard stays about the connection (matching the
 * HarmonyOS 7 layout, which puts the same three items behind one gear).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsSheet(
    state: PhantomUiState,
    onDismiss: () -> Unit,
    onThemeChange: (ThemeMode) -> Unit,
    onTunTraceChange: (Boolean) -> Unit,
    onExportLogs: () -> Unit,
    onClearHistory: () -> Unit,
    onUserRulesChange: (List<String>) -> Unit,
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
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text("设置", color = colors.textPrimary, fontSize = 20.sp, fontWeight = FontWeight.Bold)

            SectionLabel("外观")
            SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
                ThemeMode.entries.forEachIndexed { index, mode ->
                    SegmentedButton(
                        selected = state.themeMode == mode,
                        onClick = { onThemeChange(mode) },
                        shape = SegmentedButtonDefaults.itemShape(
                            index = index,
                            count = ThemeMode.entries.size,
                        ),
                    ) {
                        Text(mode.label, fontSize = 13.sp)
                    }
                }
            }

            PhantomDivider()

            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(modifier = Modifier.weight(1f)) {
                    Text("记录 TUN 追踪", color = colors.textPrimary, fontSize = 14.sp)
                    Text(
                        text = "排查 Google 等应用卡住时用；文件写在应用私有目录",
                        color = colors.textTertiary,
                        fontSize = 11.sp,
                    )
                }
                Switch(checked = state.tunTrace, onCheckedChange = onTunTraceChange)
            }

            PhantomDivider()

            Text("分流白名单", color = colors.textPrimary, fontSize = 14.sp)
            WhitelistEditor(
                entries = state.userRules,
                onChanged = onUserRulesChange,
            )

            PhantomDivider()

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = onExportLogs,
                    modifier = Modifier.weight(1f).height(44.dp),
                ) {
                    Text("导出日志", fontSize = 13.sp)
                }
                OutlinedButton(
                    onClick = onClearHistory,
                    modifier = Modifier.weight(1f).height(44.dp),
                ) {
                    Text("清空连接历史", fontSize = 13.sp)
                }
            }

            PhantomDivider()

            Text("关于", color = colors.textPrimary, fontSize = 14.sp)
            KeyValueRow("版本", co.phantom.android.BuildConfig.VERSION_NAME)
            KeyValueRow("隧道核心", "Noise IK · AES-GCM / ChaCha20 / Ascon")
            KeyValueRow("分流", "内置白名单（FST）+ 用户规则")

            Button(
                onClick = onDismiss,
                modifier = Modifier.fillMaxWidth().height(44.dp),
                colors = ButtonDefaults.buttonColors(containerColor = colors.brand),
            ) {
                Text("完成", fontSize = 14.sp)
            }
        }
    }
}
