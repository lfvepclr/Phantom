package co.phantom.android.ui

import android.graphics.Bitmap
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel

/**
 * Render a connection string as a QR bitmap.
 *
 * Error correction M tolerates ~15% damage, which is what makes the code
 * scannable off a phone screen (with its reflections and moiré) rather than
 * only off paper. A one-module margin keeps quiet-zone requirements satisfied
 * so other clients' scanners lock on quickly.
 *
 * Returns null when the payload cannot be encoded (e.g. an absurdly long URI),
 * so the caller can fall back to the copy button instead of crashing.
 */
fun encodeQr(content: String, sizePx: Int = 720): ImageBitmap? {
    if (content.isEmpty()) return null
    return try {
        val hints = mapOf(
            EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M,
            EncodeHintType.MARGIN to 1,
            EncodeHintType.CHARACTER_SET to "UTF-8",
        )
        val matrix = QRCodeWriter().encode(content, BarcodeFormat.QR_CODE, sizePx, sizePx, hints)
        val pixels = IntArray(sizePx * sizePx)
        for (y in 0 until sizePx) {
            val row = y * sizePx
            for (x in 0 until sizePx) {
                pixels[row + x] = if (matrix.get(x, y)) BLACK else WHITE
            }
        }
        val bitmap = Bitmap.createBitmap(sizePx, sizePx, Bitmap.Config.ARGB_8888)
        bitmap.setPixels(pixels, 0, sizePx, 0, 0, sizePx, sizePx)
        bitmap.asImageBitmap()
    } catch (e: Exception) {
        null
    }
}

private const val BLACK = 0xFF000000.toInt()
private const val WHITE = 0xFFFFFFFF.toInt()
