use phantom_core::{ClientConfig, PhantomError, Result};
use phantom_core::crypto::KeyPair;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing;

use crate::failover::FailoverManager;
use crate::socks5::handle_socks5_connection;

pub struct PhantomClient {
    config: ClientConfig,
    local_secret: [u8; 32],
    failover: Arc<FailoverManager>,
}

impl PhantomClient {
    pub fn new(config: ClientConfig) -> Result<Self> {
        let key_pair = KeyPair::generate()?;
        let failover = Arc::new(FailoverManager::new(&config)?);
        Ok(Self {
            config,
            local_secret: key_pair.secret,
            failover,
        })
    }

    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.config.client.listen).await?;
        tracing::info!("SOCKS5 proxy listening on {}", self.config.client.listen);

        let health_failover = Arc::clone(&self.failover);
        tokio::spawn(async move {
            health_failover.run_health_check_loop().await;
        });

        loop {
            let (stream, peer) = listener.accept().await.map_err(PhantomError::Io)?;
            tracing::info!("SOCKS5 connection from {}", peer);

            let config = self.config.clone();
            let failover = Arc::clone(&self.failover);
            let local_secret = self.local_secret;
            tokio::spawn(async move {
                if let Err(e) = handle_socks5_connection(stream, &config, &failover, local_secret).await {
                    tracing::info!("Connection error from {}: {}", peer, e);
                }
            });
        }
    }
}
