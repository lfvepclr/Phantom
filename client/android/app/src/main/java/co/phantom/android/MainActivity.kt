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

    fun startTunnel() {
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
