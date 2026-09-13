package co.phantom.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import co.phantom.android.RuleFormat
import co.phantom.android.RuleKind
import co.phantom.android.RuleParse

/*
 * The editable "分流白名单" area inside the settings sheet.
 *
 * It replaces the paragraph that only *described* the whitelist. The built-in
 * censored-domain list is untouched and always on; what this edits is the set of
 * user entries layered on top of it, and those only take effect on the next
 * tunnel start — the routing state is built once per start, so a live edit would
 * otherwise have to rebuild it mid-connection.
 */

/** One rendered entry: the parsed form of a wire line. */
private data class RuleRow(val kind: RuleKind, val value: String)

@Composable
fun WhitelistEditor(
    entries: List<String>,
    onChanged: (List<String>) -> Unit,
) {
    val colors = LocalPhantomColors.current
    var draft by rememberSaveable { mutableStateOf("") }
    var error by rememberSaveable { mutableStateOf("") }
    var showRange by rememberSaveable { mutableStateOf(false) }
    var rangeFrom by rememberSaveable { mutableStateOf("") }
    var rangeTo by rememberSaveable { mutableStateOf("") }
    var rangeError by rememberSaveable { mutableStateOf("") }

    val rows = parseRows(entries)

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("分流白名单", color = colors.textPrimary, fontSize = 14.sp)
        Text(
            text = "内置被墙域名清单始终生效，这里追加的条目优先于它。" +
                "支持精确域名、通配（*.example.com）、关键字、正则与 IP 网段。",
            color = colors.textTertiary,
            fontSize = 11.sp,
        )

        OutlinedTextField(
            value = draft,
            onValueChange = {
                draft = it
                // Refuse as the user types: a rule the core will drop should say
                // so here, not after a reconnect that quietly ignores it.
                error = when (val parsed = RuleFormat.parse(it)) {
                    is RuleParse.Invalid -> parsed.reason
                    is RuleParse.Ok -> ""
                }
            },
            placeholder = { Text("example.com 或 *.example.com", fontSize = 12.sp) },
            singleLine = true,
            isError = error.isNotEmpty(),
            supportingText = {
                if (error.isNotEmpty()) {
                    Text(error, color = colors.danger, fontSize = 11.sp)
                }
            },
            modifier = Modifier.fillMaxWidth(),
        )

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(
                onClick = {
                    when (val parsed = RuleFormat.parse(draft)) {
                        is RuleParse.Ok -> {
                            onChanged(entries + parsed.wire)
                            draft = ""
                            error = ""
                        }
                        is RuleParse.Invalid -> error = parsed.reason
                    }
                },
                enabled = draft.isNotBlank(),
                modifier = Modifier.weight(1f),
            ) {
                Text("添加", fontSize = 13.sp)
            }
            OutlinedButton(
                onClick = { showRange = !showRange },
                modifier = Modifier.weight(1f),
            ) {
                Text(if (showRange) "收起 IP 段" else "按 IP 段添加", fontSize = 13.sp)
            }
        }

        if (showRange) {
            Column(
                modifier = Modifier.padding(start = 4.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = rangeFrom,
                        onValueChange = { rangeFrom = it.trim() },
                        label = { Text("起始 IP", fontSize = 11.sp) },
                        singleLine = true,
                        modifier = Modifier.weight(1f),
                    )
                    Text("—", color = colors.textTertiary, fontSize = 12.sp)
                    OutlinedTextField(
                        value = rangeTo,
                        onValueChange = { rangeTo = it.trim() },
                        label = { Text("结束 IP", fontSize = 11.sp) },
                        singleLine = true,
                        modifier = Modifier.weight(1f),
                    )
                }
                if (rangeError.isNotEmpty()) {
                    Text(rangeError, color = colors.danger, fontSize = 11.sp)
                }
                OutlinedButton(
                    onClick = {
                        val cidrs = RuleFormat.ipRangeToCidrs(rangeFrom, rangeTo)
                        if (cidrs == null) {
                            rangeError = "请填写合法且顺序正确的 IPv4 起止地址"
                        } else {
                            rangeError = ""
                            onChanged(entries + cidrs.map { RuleFormat.toWire(RuleKind.CIDR, it) })
                            rangeFrom = ""
                            rangeTo = ""
                        }
                    },
                    enabled = rangeFrom.isNotBlank() && rangeTo.isNotBlank(),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("添加网段", fontSize = 13.sp)
                }
            }
        }

        if (rows.isEmpty()) {
            Text(
                text = "还没有自定义条目",
                color = colors.textTertiary,
                fontSize = 12.sp,
                modifier = Modifier.padding(vertical = 4.dp),
            )
        } else {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(max = 220.dp)
                    .verticalScroll(rememberScrollState()),
            ) {
                rows.forEachIndexed { index, row ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            text = row.kind.label,
                            color = colors.brand,
                            fontSize = 11.sp,
                            modifier = Modifier.padding(end = 8.dp),
                        )
                        Text(
                            text = row.value,
                            color = colors.textPrimary,
                            fontSize = 13.sp,
                            fontFamily = FontFamily.Monospace,
                            modifier = Modifier.weight(1f),
                        )
                        IconButton(
                            onClick = { onChanged(entries.filterIndexed { i, _ -> i != index }) },
                            modifier = Modifier.padding(0.dp),
                        ) {
                            Icon(
                                imageVector = Icons.Filled.Close,
                                contentDescription = "删除 ${row.value}",
                                tint = colors.textSecondary,
                            )
                        }
                    }
                }
            }
        }

        Text(
            text = "改动在下次连接时生效",
            color = colors.textTertiary,
            fontSize = 11.sp,
            fontWeight = FontWeight.Medium,
        )
    }
}

/** Parse the stored wire text into display rows, skipping anything unusable. */
private fun parseRows(entries: List<String>): List<RuleRow> = entries.mapNotNull { line ->
    val kind = RuleKind.from(line.substringBefore(':').trim()) ?: return@mapNotNull null
    val value = line.substringAfter(':', "").trim()
    if (value.isEmpty()) null else RuleRow(kind, value)
}
