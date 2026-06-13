package co.phantom.android

object RustBridge {
    init {
        System.loadLibrary("phantom_client")
    }

    /** Start tunnel using a phantom:// URI string. */
    external fun startTunnelWithURI(fd: Int, uri: ByteArray, mode: ByteArray): Int

    /** Legacy: start tunnel with TOML config. */
    external fun startTunnel(fd: Int, config: ByteArray): Int
    external fun stopTunnel(): Int
}
