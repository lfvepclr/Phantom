package co.phantom.android

object RustBridge {
    init {
        System.loadLibrary("phantom_client")
    }

    external fun startTunnel(fd: Int, config: ByteArray): Int
    external fun stopTunnel(): Int
}
