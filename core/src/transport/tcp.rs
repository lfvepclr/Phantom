use crate::{PhantomError, Result};
use async_trait::async_trait;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;

use crate::transport::traits::{Transport, TransportListener};

/// Default socket buffer size for tunnel endpoints.
///
/// Sized for a 1 Gbps x 30 ms path (BDP ≈ 3.75 MB) so weak/high-latency links
/// can keep the pipe full; the kernel clamps to its own wmem_max/rmem_max, so
/// requesting more is harmless on constrained hosts.
pub const DEFAULT_SOCKET_BUFFER: usize = 4 * 1024 * 1024;

pub struct TcpTransport {
    connect_timeout: Duration,
    nodelay: bool,
    send_buffer: usize,
    recv_buffer: usize,
}

impl TcpTransport {
    pub fn new(connect_timeout: Duration) -> Self {
        Self {
            connect_timeout,
            nodelay: true,
            send_buffer: DEFAULT_SOCKET_BUFFER,
            recv_buffer: DEFAULT_SOCKET_BUFFER,
        }
    }

    /// Override the SO_SNDBUF/SO_RCVBUF requested on tunnel sockets.
    pub fn with_buffers(mut self, send: usize, recv: usize) -> Self {
        self.send_buffer = send;
        self.recv_buffer = recv;
        self
    }
}

#[async_trait]
impl Transport for TcpTransport {
    type Stream = TcpStream;

    async fn connect(&self, addr: &SocketAddr) -> Result<Self::Stream> {
        // socket2 path: the buffers must be set before connect() so the
        // SYN-carried window-scale factor already accounts for them.
        let domain = if addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let socket =
            Socket::new(domain, Type::STREAM, Some(Protocol::TCP)).map_err(PhantomError::Io)?;
        // Buffer failures are non-fatal: the kernel clamps over-large requests.
        let _ = socket.set_send_buffer_size(self.send_buffer);
        let _ = socket.set_recv_buffer_size(self.recv_buffer);
        // Keep the tunnel socket honest across network changes. Without
        // keepalive a socket whose path disappeared (Wi-Fi ⇄ cellular) stays
        // "established" until the OS TCP retransmission timeout — minutes in
        // which the client believes it is connected but nothing moves.
        //
        // 60 s idle + 3 probes at 10 s fails such a socket in about 90 s. That
        // is slower than the 15 s this used to be, on purpose: every probe is a
        // radio wake-up on a phone, and a tunnel left connected but unused would
        // otherwise be probed four times a minute per socket. The fast path for
        // a real link change is the platform's own network callback
        // (`notifyNetworkChange` / `protectProcessNet`), which resets the
        // datapath immediately; keepalive is only the backstop for a path that
        // died silently.
        let keepalive = socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(60))
            .with_interval(Duration::from_secs(10));
        #[cfg(not(any(target_os = "openbsd", target_os = "redox")))]
        let keepalive = keepalive.with_retries(3);
        let _ = socket.set_tcp_keepalive(&keepalive);
        socket.set_nonblocking(true).map_err(PhantomError::Io)?;

        match socket.connect(&(*addr).into()) {
            Ok(()) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(libc::EINPROGRESS) => {}
            Err(e) => return Err(PhantomError::Io(e)),
        }

        let std_stream: std::net::TcpStream = socket.into();
        let stream = TcpStream::from_std(std_stream).map_err(PhantomError::Io)?;

        // Wait for the connect to finish, then surface the real result via
        // SO_ERROR (writable readiness alone does not mean success).
        tokio::time::timeout(self.connect_timeout, stream.writable())
            .await
            .map_err(|_| PhantomError::Timeout)??;
        if let Some(err) = stream.take_error().map_err(PhantomError::Io)? {
            return Err(PhantomError::Io(err));
        }

        stream.set_nodelay(self.nodelay).map_err(PhantomError::Io)?;

        #[cfg(target_os = "linux")]
        {
            // Inherent method on tokio's TcpStream; no std extension trait needed.
            let _ = stream.set_quickack(true);
        }

        Ok(stream)
    }

    fn name(&self) -> &str {
        "tcp"
    }
}

pub struct TcpListener {
    inner: tokio::net::TcpListener,
}

impl TcpListener {
    pub async fn bind(addr: &SocketAddr) -> Result<Self> {
        // socket2 listener: SO_REUSEADDR + pre-sized buffers; accepted sockets
        // inherit SO_SNDBUF/SO_RCVBUF from the listener on Linux and macOS.
        let domain = if addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let socket =
            Socket::new(domain, Type::STREAM, Some(Protocol::TCP)).map_err(PhantomError::Io)?;
        socket.set_reuse_address(true).map_err(PhantomError::Io)?;
        let _ = socket.set_send_buffer_size(DEFAULT_SOCKET_BUFFER);
        let _ = socket.set_recv_buffer_size(DEFAULT_SOCKET_BUFFER);
        socket.set_nonblocking(true).map_err(PhantomError::Io)?;
        socket.bind(&(*addr).into()).map_err(PhantomError::Io)?;
        socket.listen(1024).map_err(PhantomError::Io)?;
        let std_listener: std::net::TcpListener = socket.into();
        let inner = tokio::net::TcpListener::from_std(std_listener).map_err(PhantomError::Io)?;
        Ok(Self { inner })
    }
}

#[async_trait]
impl TransportListener for TcpListener {
    type Stream = TcpStream;

    async fn accept(&self) -> Result<(Self::Stream, SocketAddr)> {
        self.inner.accept().await.map_err(PhantomError::Io)
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.inner.local_addr().map_err(PhantomError::Io)
    }
}

/// Try to bind a TCP listener starting at `start_addr.port()`. If the port is
/// already in use, increment the port and try again — up to `max_attempts`
/// total attempts. Returns the listener and the actual bound address.
pub async fn try_bind_tcp_with_fallback(
    start_addr: SocketAddr,
    max_attempts: u16,
) -> Result<(TcpListener, SocketAddr)> {
    let ip = start_addr.ip();
    let start_port = start_addr.port();
    let mut last_err: Option<std::io::Error> = None;
    for offset in 0..max_attempts {
        let port = start_port.saturating_add(offset);
        let addr = SocketAddr::new(ip, port);
        match TcpListener::bind(&addr).await {
            Ok(listener) => return Ok((listener, addr)),
            Err(PhantomError::Io(io_err)) if io_err.kind() == std::io::ErrorKind::AddrInUse => {
                last_err = Some(io_err);
            }
            Err(e) => return Err(e),
        }
    }
    let end_port = start_port.saturating_add(max_attempts.saturating_sub(1));
    Err(PhantomError::Config(format!(
        "No free TCP port in {ip}:{start_port}..{end_port} ({} attempt(s) all busy): {}",
        max_attempts,
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "AddrInUse".to_string())
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use tokio::net::TcpListener as TokioTcp;

    /// Reserve `count` consecutive loopback ports, returning the base port and
    /// the listeners holding them.
    ///
    /// Picking a port by binding `:0` and dropping the listener is racy: the
    /// port is free again before the caller can use it, so a sibling test in
    /// the same binary can take it. Here every port stays bound for the
    /// lifetime of the returned guards, making the "busy" side of each
    /// assertion deterministic.
    async fn reserve_consecutive(count: u16) -> (u16, Vec<TokioTcp>) {
        assert!(count > 0);
        for _ in 0..64 {
            let first = TokioTcp::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("failed to bind an ephemeral port");
            let base = first.local_addr().unwrap().port();
            if base.checked_add(count).is_none() {
                continue;
            }
            let mut held = vec![first];
            for offset in 1..count {
                match TokioTcp::bind((Ipv4Addr::LOCALHOST, base + offset)).await {
                    Ok(listener) => held.push(listener),
                    // A neighbouring port is taken; start over from a new base.
                    Err(_) => break,
                }
            }
            if held.len() == count as usize {
                return (base, held);
            }
        }
        panic!("could not reserve {} consecutive loopback ports", count);
    }

    #[tokio::test]
    async fn try_bind_tcp_with_fallback_picks_next_port() {
        // Hold two consecutive ports so the fallback has to skip both.
        let (base, _held) = reserve_consecutive(2).await;
        let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), base);

        let (_listener, bound) = try_bind_tcp_with_fallback(start, 50)
            .await
            .expect("a free port should exist within a 50-port window");
        assert_eq!(bound.ip(), start.ip());
        assert!(
            bound.port() >= base + 2,
            "expected both occupied ports to be skipped, got {}",
            bound.port()
        );
    }

    #[tokio::test]
    async fn try_bind_tcp_with_fallback_first_port_free() {
        // Reserve a port, release it, then immediately claim it through the
        // function under test. A concurrent bind may win that port, so retry a
        // few times before declaring failure.
        for attempt in 0..16 {
            let (base, held) = reserve_consecutive(1).await;
            drop(held);
            let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), base);
            match try_bind_tcp_with_fallback(start, 1).await {
                Ok((_listener, bound)) => {
                    assert_eq!(bound.port(), base, "a free start port must be used as-is");
                    return;
                }
                Err(_) if attempt < 15 => continue,
                Err(e) => panic!("never won a free port: {:?}", e),
            }
        }
    }

    #[tokio::test]
    async fn try_bind_tcp_with_fallback_exhausts_attempts() {
        // Both ports in the window stay bound for the whole test, so the
        // fallback must run out of attempts.
        let (base, _held) = reserve_consecutive(2).await;
        let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), base);

        let result: Result<(TcpListener, SocketAddr)> = try_bind_tcp_with_fallback(start, 2).await;
        match result {
            Ok((_, bound)) => panic!("expected an error, bound {} instead", bound),
            Err(PhantomError::Config(msg)) => {
                assert!(
                    msg.contains("No free TCP port"),
                    "unexpected message: {}",
                    msg
                )
            }
            Err(other) => panic!("expected Config error, got {:?}", other),
        }
    }
}
