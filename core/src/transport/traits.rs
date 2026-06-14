use crate::Result;
use async_trait::async_trait;
use std::net::SocketAddr;
use tokio::io::{AsyncRead, AsyncWrite};

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Send + Unpin + 'static;

    async fn connect(&self, addr: &SocketAddr) -> Result<Self::Stream>;
    fn name(&self) -> &str;
}

#[async_trait]
pub trait TransportListener: Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Send + Unpin + 'static;

    async fn accept(&self) -> Result<(Self::Stream, SocketAddr)>;
    fn local_addr(&self) -> Result<SocketAddr>;
}
