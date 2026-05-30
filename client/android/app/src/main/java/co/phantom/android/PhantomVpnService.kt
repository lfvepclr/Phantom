package co.phantom.android

import android.net.VpnService
import android.content.Intent
import android.os.ParcelFileDescriptor

class PhantomVpnService : VpnService() {

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val builder = Builder()
            .addAddress("10.7.0.2", 24)
            .addRoute("0.0.0.0", 0)
            .addDnsServer("8.8.8.8")
            .addDnsServer("8.8.4.4")
            .setMtu(1500)

        val pfd: ParcelFileDescriptor? = builder.establish()
        if (pfd == null) {
            return START_NOT_STICKY
        }

        val fd = pfd.detachFd()

        val config = """
            [[servers]]
            name = "default"
            address = "127.0.0.1:443"
            public_key = ""

            [client]
            listen = "127.0.0.1:11080"
            mode = "smart"
        """.trimIndent().toByteArray(Charsets.UTF_8)

        val rc = RustBridge.startTunnel(fd, config)
        if (rc != 0) {
            stopSelf()
            return START_NOT_STICKY
        }

        return START_STICKY
    }

    override fun onDestroy() {
        RustBridge.stopTunnel()
        super.onDestroy()
    }
}
