package co.phantom.android.ui

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
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import co.phantom.android.LogEntry

/*
 * The log pane, shared by the dashboard card and the full-screen view.
 *
 * Both render the same buffer with the same controls, so "the log looks
 * different in the two places" can never be a source of confusion. Lines never
 * wrap: a wrapped log line reads as two events, and the useful part (the
 * target and the routing decision) is at the front.
 */

@Composable
fun LogPanel(
    lines: List<LogEntry>,
    paused: Boolean,
    showDirect: Boolean,
    onTogglePause: () -> Unit,
    onToggleDirect: (Boolean) -> Unit,
    onClear: () -> Unit,
    modifier: Modifier = Modifier,
    /** Card mode shows the expand affordance; the full screen shows a close. */
    onExpand: (() -> Unit)? = null,
    onClose: (() -> Unit)? = null,
) {
    val colors = LocalPhantomColors.current
    val listState = rememberLazyListState()

    LaunchedEffect(lines.size, paused) {
        if (lines.isNotEmpty() && !paused) {
            listState.scrollToItem(lines.lastIndex)
        }
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = "日志",
                color = colors.textPrimary,
                fontSize = 15.sp,
                fontWeight = FontWeight.Medium,
            )
            if (onExpand != null) {
                // Full screen belongs next to the *title*: sitting beside the
                // trash icon it was one slip away from wiping the log.
                TextButton(onClick = onExpand, contentPadding = androidx.compose.foundation.layout.PaddingValues(8.dp)) {
                    Text("放大", fontSize = 12.sp, color = colors.textSecondary)
                }
            }
            Spacer(modifier = Modifier.weight(1f))
            Text(
                text = "${lines.size} 行",
                color = colors.textTertiary,
                fontSize = 11.sp,
            )
            TextButton(
                onClick = { onToggleDirect(!showDirect) },
                contentPadding = androidx.compose.foundation.layout.PaddingValues(6.dp),
            ) {
                Text(
                    text = if (showDirect) "全部" else "仅隧道",
                    fontSize = 12.sp,
                    color = colors.brand,
                )
            }
            // Text rather than an icon: the pause glyph lives in the extended
            // icon set, and pulling in that whole artifact for one symbol is
            // not worth the download.
            TextButton(
                onClick = onTogglePause,
                contentPadding = androidx.compose.foundation.layout.PaddingValues(6.dp),
            ) {
                Text(
                    text = if (paused) "继续" else "暂停",
                    fontSize = 12.sp,
                    color = colors.textSecondary,
                )
            }
            IconButton(onClick = onClear, modifier = Modifier.size(36.dp)) {
                Icon(
                    imageVector = Icons.Filled.Delete,
                    contentDescription = "清空日志",
                    tint = colors.textSecondary,
                )
            }
            if (onClose != null) {
                IconButton(onClick = onClose, modifier = Modifier.size(36.dp)) {
                    Icon(
                        imageVector = Icons.Filled.Close,
                        contentDescription = "关闭全屏日志",
                        tint = colors.textSecondary,
                    )
                }
            }
        }

        Box(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(12.dp))
                .background(colors.codeSurface)
                .padding(8.dp),
        ) {
            if (lines.isEmpty()) {
                Text(
                    text = "暂无日志",
                    color = colors.textTertiary,
                    fontSize = 12.sp,
                )
            } else {
                LazyColumn(state = listState, verticalArrangement = Arrangement.spacedBy(1.dp)) {
                    // Keyed by the entry id: the pane shows a sliding tail, so
                    // positional keys would make every append look like "all
                    // 200 rows changed" and rebind the whole window.
                    items(lines, key = { it.id }) { entry ->
                        Text(
                            text = entry.text,
                            color = lineColor(entry.text),
                            fontSize = 11.sp,
                            fontFamily = FontFamily.Monospace,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun lineColor(line: String) = when {
    line.contains("ERROR", ignoreCase = true) -> LocalPhantomColors.current.danger
    line.contains("WARN", ignoreCase = true) -> LocalPhantomColors.current.warning
    line.contains("-> Proxy", ignoreCase = true) -> LocalPhantomColors.current.success
    else -> LocalPhantomColors.current.textPrimary
}

/** Full-screen reading mode: same controls, no competing content. */
@Composable
fun LogFullScreen(
    lines: List<LogEntry>,
    paused: Boolean,
    showDirect: Boolean,
    onTogglePause: () -> Unit,
    onToggleDirect: (Boolean) -> Unit,
    onClear: () -> Unit,
    onClose: () -> Unit,
) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(LocalPhantomColors.current.canvas)
            .padding(horizontal = 16.dp, vertical = 12.dp),
    ) {
        LogPanel(
            lines = lines,
            paused = paused,
            showDirect = showDirect,
            onTogglePause = onTogglePause,
            onToggleDirect = onToggleDirect,
            onClear = onClear,
            onClose = onClose,
            modifier = Modifier.fillMaxSize(),
        )
    }
}

/** Spacer used between dashboard sections; keeps gaps in one place. */
@Composable
fun SectionGap() {
    Spacer(modifier = Modifier.height(12.dp))
}
