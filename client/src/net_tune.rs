//! Socket tuning shared by every hop of the datapath.
//!
//! Nagle's algorithm trades latency for packet count: the kernel holds a small
//! segment back until the previous one is acknowledged. That trade is wrong for
//! a proxy, because the traffic it carries is overwhelmingly request/response
//! shaped — a request header smaller than one segment sits in the send buffer
//! waiting for an ACK while the peer is itself waiting for that request before
//! it will answer.
//!
//! The socket the client opens *towards the server* has always had
//! `TCP_NODELAY` set (see `phantom_core::transport::tcp`). The remaining hops
//! did not:
//!
//! * the loopback pair into the local SOCKS5 ingress (relay ⇄ ingress), and
//! * direct connections to domestic destinations, which ride a real RTT.
//!
//! Disabling Nagle cannot lose data: the kernel still coalesces whenever a full
//! segment is available or the peer's ACK is outstanding, and the receive path
//! is untouched (delayed ACKs are unaffected).

use tokio::net::TcpStream;

/// Route a socket outside the tunnel interface where the platform needs it.
///
/// On Android every socket belongs to the VPN by default once the TUN is up,
/// so the direct resolver (`client.dns_direct`) would send its queries into our
/// own TUN and never reach the physical network. `VpnService.protect()` is the
/// only supported way to exempt a socket, and it has to be called from Kotlin —
/// this is the hook the datapath uses to reach it.
///
/// Everywhere else this is a no-op: macOS and HarmonyOS run the tunnel out of
/// process or exclude the resolver route explicitly, so their sockets already
/// take the physical path.
#[cfg(unix)]
pub fn protect_socket(fd: std::os::unix::io::RawFd) {
    if !crate::platform::protect_fd(fd) {
        tracing::debug!("socket {fd} could not be protected from the tunnel");
    }
}

/// Apply the client's datapath tuning to one connected stream.
///
/// Every option is best-effort: a platform that refuses one must not fail the
/// connection, so failures are logged at debug level and the socket is used
/// as-is.
pub fn tune(stream: &TcpStream) {
    if let Err(e) = stream.set_nodelay(true) {
        tracing::debug!("TCP_NODELAY not set on datapath socket: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Both ends of a proxied connection go through [`tune`], and both must come
    /// out with Nagle disabled — the relay writes the app's segments and the
    /// ingress writes the remote's, either of which may be a small write.
    #[tokio::test]
    async fn tune_disables_nagle_on_both_ends() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback listener");
        let addr = listener.local_addr().expect("local addr");

        let accepted = tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.expect("accept");
            tune(&stream);
            stream
        });

        let dialed = TcpStream::connect(addr).await.expect("connect");
        tune(&dialed);

        assert!(
            dialed.nodelay().expect("nodelay of dialed socket"),
            "the dialing side must be tuned"
        );
        let accepted = accepted.await.expect("accept task");
        assert!(
            accepted.nodelay().expect("nodelay of accepted socket"),
            "the accepting side must be tuned"
        );
    }

    /// A stream that never went through [`tune`] keeps the OS default, which is
    /// what the tuning above is there to change.
    #[tokio::test]
    async fn untuned_loopback_stream_keeps_nagle_enabled() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let accepted = tokio::spawn(async move { listener.accept().await.expect("accept") });

        let dialed = TcpStream::connect(addr).await.expect("connect");
        assert!(
            !dialed.nodelay().expect("nodelay of dialed socket"),
            "tokio's default is Nagle on; tuning has to turn it off explicitly"
        );
        let _ = accepted.await.expect("accept task");
    }
}
