package co.phantom.android.ui

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.Camera
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.border
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.google.mlkit.vision.barcode.BarcodeScanner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * In-app QR scanner.
 *
 * The camera feed is analysed on a single background thread; frames are
 * dropped, never queued, so a slow decode can never build a backlog of stale
 * images. Only `phantom://` payloads are accepted — a random QR code in view
 * should not silently replace the user's connection.
 *
 * The gallery path exists because scanning a QR off another screen is not
 * always possible (screenshot on the same phone, screen glare, no second
 * device), and it costs one extra activity launch.
 */
@Composable
fun ScanScreen(
    onClose: () -> Unit,
    onScanned: (String) -> Unit,
) {
    val context = LocalContext.current
    var hasPermission by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED
        )
    }
    var message by remember { mutableStateOf("") }
    var torchOn by remember { mutableStateOf(false) }
    var manualEntry by remember { mutableStateOf(false) }
    var manualText by remember { mutableStateOf("") }
    var camera by remember { mutableStateOf<Camera?>(null) }
    val handled = remember { AtomicBoolean(false) }
    val lifecycleOwner = androidx.compose.ui.platform.LocalLifecycleOwner.current

    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        hasPermission = granted
        if (!granted) {
            message = "未授予相机权限，可从相册选择二维码图片"
        }
    }

    LaunchedEffect(Unit) {
        if (!hasPermission) permissionLauncher.launch(Manifest.permission.CAMERA)
    }

    val scanner: BarcodeScanner = remember {
        BarcodeScanning.getClient(
            BarcodeScannerOptions.Builder()
                .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
                .build()
        )
    }
    DisposableEffect(scanner) {
        onDispose { scanner.close() }
    }

    fun accept(raw: String?) {
        if (raw.isNullOrEmpty()) return
        if (!raw.startsWith("phantom://")) {
            message = "这不是 Phantom 连接串"
            return
        }
        if (handled.compareAndSet(false, true)) {
            onScanned(raw)
        }
    }

    val galleryLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia()
    ) { uri: Uri? ->
        if (uri == null) return@rememberLauncherForActivityResult
        try {
            val image = InputImage.fromFilePath(context, uri)
            scanner.process(image)
                .addOnSuccessListener { barcodes -> accept(barcodes.firstOrNull()?.rawValue) }
                .addOnFailureListener { message = "图片识别失败：${it.message ?: "未知错误"}" }
        } catch (e: Exception) {
            message = "无法读取图片：${e.message ?: "未知错误"}"
        }
    }

    Box(modifier = Modifier.fillMaxSize().background(Color(0xFF000000))) {
        if (hasPermission) {
            AndroidView(
                modifier = Modifier.fillMaxSize(),
                factory = { ctx ->
                    val previewView = PreviewView(ctx)
                    val executor = Executors.newSingleThreadExecutor()
                    val providerFuture = ProcessCameraProvider.getInstance(ctx)
                    providerFuture.addListener({
                        try {
                            val provider = providerFuture.get()
                            val preview = Preview.Builder().build().also {
                                it.setSurfaceProvider(previewView.surfaceProvider)
                            }
                            val analysis = ImageAnalysis.Builder()
                                // Keep only the newest frame: decoding a stale
                                // one costs battery and reports a code the user
                                // has already moved away from.
                                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                                .build()
                            analysis.setAnalyzer(executor) { proxy: ImageProxy ->
                                val media = proxy.image
                                if (media == null) {
                                    proxy.close()
                                    return@setAnalyzer
                                }
                                val input = InputImage.fromMediaImage(
                                    media,
                                    proxy.imageInfo.rotationDegrees,
                                )
                                scanner.process(input)
                                    .addOnSuccessListener { barcodes ->
                                        accept(barcodes.firstOrNull()?.rawValue)
                                    }
                                    .addOnCompleteListener { proxy.close() }
                            }
                            provider.unbindAll()
                            camera = provider.bindToLifecycle(
                                lifecycleOwner,
                                CameraSelector.DEFAULT_BACK_CAMERA,
                                preview,
                                analysis,
                            )
                        } catch (e: Exception) {
                            message = "相机启动失败：${e.message ?: "未知错误"}"
                        }
                    }, ContextCompat.getMainExecutor(ctx))
                    previewView
                },
            )

            // Viewfinder frame: the overlay is drawn rather than baked into the
            // preview so it stays square regardless of sensor orientation.
            Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth(0.68f)
                        .aspectRatio(1f)
                        .clip(RoundedCornerShape(16.dp))
                        .border(2.dp, Color(0xCC00E5A0), RoundedCornerShape(16.dp))
                        .background(Color(0x1400E5A0)),
                )
            }
        } else {
            Column(
                modifier = Modifier.fillMaxSize().padding(24.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    text = message.ifEmpty { "需要相机权限才能扫码" },
                    color = Color.White,
                    fontSize = 14.sp,
                )
                Spacer(modifier = Modifier.height(12.dp))
                Button(onClick = { permissionLauncher.launch(Manifest.permission.CAMERA) }) {
                    Text("授予相机权限")
                }
            }
        }

        Row(
            modifier = Modifier
                .align(Alignment.TopStart)
                .fillMaxWidth()
                .padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("扫描连接二维码", color = Color.White, fontSize = 16.sp, fontWeight = FontWeight.Medium)
        }

        Column(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            if (message.isNotEmpty()) {
                Text(
                    text = message,
                    color = Color(0xFFFFC7B8),
                    fontSize = 12.sp,
                    modifier = Modifier
                        .clip(RoundedCornerShape(10.dp))
                        .background(Color(0x66000000))
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                )
            }
            // The gallery and manual entry need no CAMERA permission, so they
            // stay reachable once it has been denied — otherwise refusing the
            // prompt would leave no way at all to import a link.
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                OutlinedButton(
                    onClick = {
                        galleryLauncher.launch(
                            PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)
                        )
                    },
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = Color.White),
                    modifier = Modifier.weight(1f),
                ) {
                    Text("从相册选择", fontSize = 13.sp)
                }
                OutlinedButton(
                    onClick = { manualEntry = true },
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = Color.White),
                    modifier = Modifier.weight(1f),
                ) {
                    Text("手动输入", fontSize = 13.sp)
                }
            }

            if (hasPermission) {
                OutlinedButton(
                    onClick = { torchOn = !torchOn },
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = Color.White),
                ) {
                    Text(if (torchOn) "关闭手电筒" else "手电筒", fontSize = 13.sp)
                }
            }

            Button(
                onClick = onClose,
                modifier = Modifier.fillMaxWidth(),
                colors = ButtonDefaults.buttonColors(containerColor = Color(0x33FFFFFF)),
            ) {
                Text("返回", fontSize = 14.sp, color = Color.White)
            }
        }

        if (manualEntry) {
            ManualEntryDialog(
                text = manualText,
                onTextChange = { manualText = it },
                onDismiss = { manualEntry = false },
                onConfirm = {
                    val value = manualText.trim()
                    if (!value.startsWith("phantom://")) {
                        message = "连接串需要以 phantom:// 开头"
                    } else {
                        manualEntry = false
                        accept(value)
                    }
                },
            )
        }
    }

    // Torch is a property of the bound camera, so it can be flipped without
    // re-binding the use cases.
    LaunchedEffect(torchOn, camera) {
        camera?.cameraControl?.enableTorch(torchOn)
    }
}

/**
 * Fallback for when the camera is unavailable or unusable: the link is typed
 * (or pasted) instead of photographed.
 *
 * It is deliberately a plain card rather than a themed dialog — it sits on top
 * of a full-bleed black preview, so it carries its own surface.
 */
@Composable
private fun ManualEntryDialog(
    text: String,
    onTextChange: (String) -> Unit,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    val colors = LocalPhantomColors.current
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Color(0xEE000000))
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        PhantomCard {
            Text(
                text = "手动输入连接串",
                color = colors.textPrimary,
                fontSize = 16.sp,
                fontWeight = FontWeight.Medium,
            )
            OutlinedTextField(
                value = text,
                onValueChange = onTextChange,
                placeholder = { Text("phantom://key@host:port", fontSize = 12.sp) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            if (text.isNotBlank() && !text.trim().startsWith("phantom://")) {
                Text(
                    text = "连接串需要以 phantom:// 开头",
                    color = colors.danger,
                    fontSize = 11.sp,
                )
            }
            Row(
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                OutlinedButton(onClick = onDismiss, modifier = Modifier.weight(1f)) {
                    Text("取消", fontSize = 13.sp)
                }
                Button(
                    onClick = onConfirm,
                    enabled = text.isNotBlank(),
                    modifier = Modifier.weight(1f),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.brand,
                        contentColor = colors.onBrand,
                    ),
                ) {
                    Text("导入", fontSize = 13.sp)
                }
            }
        }
    }
}
