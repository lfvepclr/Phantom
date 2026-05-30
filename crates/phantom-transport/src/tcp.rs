use async_trait::async_trait;
use phantom_core::{PhantomError, Result};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;

use crate::traits::{Transport, TransportListener};

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
        self.inner
            .accept()
            .await
            .map_err(PhantomError::Io)
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.inner.local_addr().map_err(PhantomError::Io)
    }
}
