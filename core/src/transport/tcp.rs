use crate::{PhantomError, Result};
use async_trait::async_trait;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;

use crate::transport::traits::{Transport, TransportListener};

pub struct TcpTransport {
    connect_timeout: Duration,
    nodelay: bool,
}

impl TcpTransport {
    pub fn new(connect_timeout: Duration) -> Self {
        Self {
            connect_timeout,
            nodelay: true,
        }
    }
}

#[async_trait]
impl Transport for TcpTransport {
    type Stream = TcpStream;

    async fn connect(&self, addr: &SocketAddr) -> Result<Self::Stream> {
        let stream = tokio::time::timeout(self.connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| PhantomError::Timeout)?
            .map_err(|e| PhantomError::Io(e))?;

        stream
            .set_nodelay(self.nodelay)
            .map_err(|e| PhantomError::Io(e))?;

        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::TcpStreamExt;
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
        let inner = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(PhantomError::Io)?;
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

    /// Pick a free port (immediately) and return it. The std listener is
    /// dropped before we return, so the OS may still hold the port in
    /// TIME_WAIT for a moment. Use this only when we want to **probe** a
    /// free port (we never rebind the same port right after).
    fn pick_free_port() -> u16 {
        let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        port
    }

    /// Synchronously occupy a port and return a guard. The guard's Drop will
    /// release the port (though TIME_WAIT may still apply). Using
    /// `tokio::net::TcpListener` here so SO_REUSEADDR is set.
    async fn occupy_async(port: u16) -> TokioTcp {
        TokioTcp::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("failed to occupy port")
    }

    #[tokio::test]
    async fn try_bind_tcp_with_fallback_picks_next_port() {
        // Pick a free port, then occupy it via tokio (which sets SO_REUSEADDR),
        // so rebinding the same port is fine; the next attempt will succeed.
        let port = pick_free_port();
        // Brief sleep to let the port leave TIME_WAIT.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _occupying = occupy_async(port).await;

        let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let (_listener, bound) = try_bind_tcp_with_fallback(start, 5)
            .await
            .expect("should find a free port within 5 attempts");
        assert_eq!(bound.ip(), start.ip());
        assert!(
            bound.port() > start.port(),
            "expected fallback to a higher port, got {}",
            bound.port()
        );
    }

    #[tokio::test]
    async fn try_bind_tcp_with_fallback_first_port_free() {
        // Find a free port and immediately rebind it via tokio. SO_REUSEADDR
        // is set on tokio TcpListener, so the rebind should succeed.
        let port = pick_free_port();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let (_listener, bound) = try_bind_tcp_with_fallback(start, 3).await.unwrap();
        assert_eq!(bound.port(), port);
    }

    #[tokio::test]
    async fn try_bind_tcp_with_fallback_exhausts_attempts() {
        // Occupy 3 consecutive ports via tokio (SO_REUSEADDR), then ask for
        // max_attempts=2 — should fail.
        let port = pick_free_port();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _a = occupy_async(port).await;
        let _b = occupy_async(port + 1).await;
        let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let result: Result<(TcpListener, SocketAddr)> = try_bind_tcp_with_fallback(start, 2).await;
        match result {
            Ok(_) => panic!("expected an error, got Ok"),
            Err(PhantomError::Config(msg)) => assert!(msg.contains("No free TCP port")),
            Err(other) => panic!("expected Config error, got {:?}", other),
        }
    }
}
