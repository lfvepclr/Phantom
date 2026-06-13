package co.phantom.android

import android.app.Activity
import android.content.Intent
import android.net.VpnService
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            PhantomApp(this)
        }
    }
}

@Composable
fun PhantomApp(activity: Activity) {
    var isRunning by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf("Idle") }
    var serverURI by remember { mutableStateOf("") }
    var proxyMode by remember { mutableStateOf("smart") }

    fun startTunnel() {
        if (serverURI.isBlank()) {
            status = "Error: server URI required"
            return
        }
        // Pass URI and mode to the VpnService before starting.
        PhantomVpnService.serverURI = serverURI.trim()
        PhantomVpnService.proxyMode = proxyMode

        val intent = VpnService.prepare(activity)
        if (intent != null) {
            activity.startActivityForResult(intent, 0)
            status = "Waiting for VPN permission..."
            return
        }
        activity.startService(Intent(activity, PhantomVpnService::class.java))
        isRunning = true
        status = "Connected"
    }

    fun stopTunnel() {
        activity.stopService(Intent(activity, PhantomVpnService::class.java))
        isRunning = false
        status = "Stopped"
    }

    Surface(
        modifier = Modifier.fillMaxSize(),
        color = MaterialTheme.colorScheme.background
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center
        ) {
            Text(
                text = "Phantom",
                style = MaterialTheme.typography.headlineLarge
            )

            Spacer(modifier = Modifier.height(16.dp))

            OutlinedTextField(
                value = serverURI,
                onValueChange = { serverURI = it },
                label = { Text("Server URI") },
                placeholder = { Text("phantom://key@host:port") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
                enabled = !isRunning
            )

            Spacer(modifier = Modifier.height(8.dp))

            // Proxy mode selector
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceEvenly
            ) {
                listOf("smart", "proxy", "direct").forEach { mode ->
                    FilterChip(
                        selected = proxyMode == mode,
                        onClick = { proxyMode = mode },
                        label = { Text(mode.replaceFirstChar { it.uppercase() }) },
                        enabled = !isRunning
                    )
                }
            }

            Spacer(modifier = Modifier.height(24.dp))

            Text(
                text = status,
                style = MaterialTheme.typography.bodyLarge,
                color = if (isRunning) MaterialTheme.colorScheme.primary
                        else MaterialTheme.colorScheme.error
            )

            Spacer(modifier = Modifier.height(32.dp))

            Button(
                onClick = { if (isRunning) stopTunnel() else startTunnel() },
                modifier = Modifier.fillMaxWidth()
            ) {
                Text(if (isRunning) "Stop Tunnel" else "Start Tunnel")
            }
        }
    }
}
