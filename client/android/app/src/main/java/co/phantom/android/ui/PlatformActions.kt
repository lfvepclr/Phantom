package co.phantom.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.io.File

/**
 * Hand the connection string to another app.
 *
 * `ACTION_SEND` rather than a custom sheet: the user's own messengers and note
 * apps are where they keep links, and the platform already knows how to reach
 * them.
 */
fun shareText(context: Context, text: String, subject: String = "Phantom 连接串") {
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_SUBJECT, subject)
        putExtra(Intent.EXTRA_TEXT, text)
    }
    context.startActivity(Intent.createChooser(intent, "分享连接串"))
}

fun copyToClipboard(context: Context, text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    clipboard.setPrimaryClip(ClipData.newPlainText("Phantom", text))
}

/**
 * Share a log file as an attachment.
 *
 * Files are exposed through the app's `FileProvider` under `logs/`; the paths
 * inside `filesDir` are private, so a raw `EXTRA_STREAM` would be rejected by
 * the receiving app.
 */
fun shareFile(context: Context, file: File, mime: String, subject: String) {
    if (!file.exists()) return
    val uri = androidx.core.content.FileProvider.getUriForFile(
        context,
        "${context.packageName}.fileprovider",
        file,
    )
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = mime
        putExtra(Intent.EXTRA_SUBJECT, subject)
        putExtra(Intent.EXTRA_STREAM, uri)
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }
    context.startActivity(Intent.createChooser(intent, subject))
}

/**
 * Show the connection string as a QR code so another device can scan it.
 *
 * The code is generated from the same string that is copied, so "share" and
 * "scan" can never disagree about what the connection is.
 */
@Composable
fun QrShareDialog(
    uri: String,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val colors = LocalPhantomColors.current
    val qr = remember(uri) { encodeQr(uri, 720) }

    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            TextButton(onClick = onDismiss) { Text("关闭") }
        },
        title = {
            Text("分享连接", fontSize = 17.sp, fontWeight = FontWeight.Medium)
        },
        text = {
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Text(
                    text = "用另一台设备上的 Phantom 扫码导入",
                    color = colors.textSecondary,
                    fontSize = 12.sp,
                )
                if (qr != null) {
                    Box(
                        modifier = Modifier
                            .clip(RoundedCornerShape(12.dp))
                            .background(colors.surface)
                            .padding(8.dp),
                    ) {
                        Image(
                            bitmap = qr,
                            contentDescription = "连接二维码",
                            modifier = Modifier.size(240.dp),
                        )
                    }
                } else {
                    Text("连接串过长，无法生成二维码", color = colors.danger, fontSize = 12.sp)
                }
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
                        Text("系统分享", fontSize = 13.sp)
                    }
                }
                Spacer(modifier = Modifier.height(2.dp))
                Text(
                    text = uri,
                    color = colors.textTertiary,
                    fontSize = 10.sp,
                    maxLines = 2,
                )
                Spacer(modifier = Modifier.width(2.dp))
            }
        },
    )
}
