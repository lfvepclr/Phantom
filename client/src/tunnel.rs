use phantom_core::crypto::KeyPair;
use phantom_core::{ClientConfig, PhantomError, Result};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing;

use crate::failover::FailoverManager;
use crate::hello::verify_server_connection;
use crate::quic_pool::QuicPool;
use crate::http_proxy::handle_inbound;
use crate::tun::TunSettings;

/// Options for the TUN transparent-proxy runtime (`phantom client --tun`).
///
/// Only used by the CLI: the macOS / Android / HarmonyOS apps build their TUN
/// device through their own platform bridge.
#[derive(Debug, Clone, Default)]
pub struct TunRuntimeOptions {
    /// TUN device settings (name / address / netmask / MTU).
    pub tun: TunSettings,
    /// Config path used by the hot-reload watcher. `None` disables reloading.
    pub config_path: Option<String>,
    /// Linux router gateway plumbing. `None` = TUN device only, no policy
    /// routing (single-host transparent proxy).
    #[cfg(target_os = "linux")]
    pub gateway: Option<crate::gateway::GatewayConfig>,
}

/// Resolve when the process is asked to terminate.
///
/// Without this, SIGTERM/SIGINT kill the process outright and destructors never
/// run — which would leave the Linux gateway's `ip rule` and `iptables` entries
/// behind after `phantom.sh stop`.
///
/// Only used by `run_tun` (CLI / router); mobile bridges manage their own
/// lifecycle via the platform VPN service.
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Cannot listen for SIGTERM: {}", e);
                // Fall back to Ctrl-C only.
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub struct PhantomClient {
    config: ClientConfig,
    local_secret: [u8; 32],
    failover: Arc<FailoverManager>,
    /// Multiplexed QUIC connections, one per server address. Harmless for
    /// TCP-only configs: the pool is only touched on the QUIC code path.
    quic_pool: Arc<QuicPool>,
    /// Shared traffic counters: SOCKS5 and TUN traffic land in the same
    /// instance, which the metrics endpoint serves.
    stats: Arc<crate::stats::TrafficStats>,
}

impl PhantomClient {
    pub fn new(config: ClientConfig) -> Result<Self> {
        let key_pair = KeyPair::generate()?;
        let failover = Arc::new(FailoverManager::new(&config)?);
        Ok(Self {
            config,
            local_secret: key_pair.secret,
            failover,
            quic_pool: Arc::new(QuicPool::new()),
            stats: crate::stats::TrafficStats::new(),
        })
    }

    /// Run in SOCKS5-only mode.
    pub async fn run(&self) -> Result<()> {
        self.verify().await?;
        let listener = self.bind_socks5().await?;
        self.spawn_health_check();
        self.spawn_metrics();
        self.accept_socks5(listener).await
    }

    /// Run in TUN transparent-proxy mode: a local SOCKS5 listener plus a TUN
    /// device whose traffic is routed through it.
    ///
    /// On Linux this is the router entry point; `options.gateway` additionally
    /// installs the policy routing that pulls forwarded LAN traffic in.
    ///
    /// Not compiled on Android/HarmonyOS: there the OS owns the VPN interface
    /// and hands us a ready-made fd (see `TunDevice::from_fd`), so a
    /// self-created TUN device is never possible.
    #[cfg(not(any(target_os = "android", target_env = "ohos")))]
    pub async fn run_tun(&self, options: TunRuntimeOptions) -> Result<()> {
        self.verify().await?;

        // Bind SOCKS5 before touching the network configuration: a port clash
        // should fail cleanly rather than half-install a gateway.
        let listener = self.bind_socks5().await?;

        let device = crate::tun::TunDevice::create_with(&options.tun).map_err(|e| {
            PhantomError::Config(format!(
                "Failed to create TUN device '{}': {} (TUN mode requires root)",
                options.tun.name, e
            ))
        })?;
        tracing::info!(
            "TUN device up: {} addr={} mtu={}",
            options.tun.name,
            options.tun.address,
            options.tun.mtu
        );

        // Held for the lifetime of the tunnel; Drop reverts the routing changes.
        #[cfg(target_os = "linux")]
        let _gateway = match options.gateway {
            Some(config) => Some(crate::gateway::Gateway::install(config)?),
            None => None,
        };

        let socks5_addr = self
            .config
            .client
            .listen
            .parse()
            .map_err(|e| PhantomError::Config(format!("Invalid SOCKS5 listen address: {}", e)))?;

        let mut proxy = crate::tun::TunProxy::new(device, socks5_addr)
            .with_mode(self.config.client.mode)
            .with_failover(Arc::clone(&self.failover))
            .with_stats(Arc::clone(&self.stats));

        if let Some(server) = self.config.servers.first() {
            proxy = proxy.with_server(server.clone(), self.local_secret);
        }
        if let Some(path) = options.config_path {
            proxy = proxy.with_config_path(path);
        }
        match crate::rules::RuleEngine::from_config(&self.config.rules) {
            Ok(engine) => {
                proxy = proxy.with_rules(engine);
                tracing::info!(
                    "Smart routing enabled with {} rule(s)",
                    self.config.rules.rules.len()
                );
            }
            Err(e) => tracing::warn!("Rule engine init failed: {}", e),
        }
        if let Some(dns_addr) = crate::dns::parse_dns_addr(&self.config.client.dns) {
            match crate::dns::DnsProxy::new(dns_addr).await {
                Ok(dns) => {
                    proxy = proxy.with_dns(dns);
                    tracing::info!("DNS hijack enabled, upstream = {}", dns_addr);
                }
                Err(e) => tracing::warn!("DNS proxy init failed: {}", e),
            }
        } else {
            tracing::warn!(
                "Invalid client.dns value '{}', DNS hijack disabled",
                self.config.client.dns
            );
        }

        self.spawn_health_check();
        self.spawn_metrics();

        let socks5 = self.accept_socks5(listener);
        let tun = proxy.run();
        // Returning normally (rather than dying on the signal's default
        // disposition) is what lets the gateway's Drop revert the routing and
        // firewall changes: a process killed by SIGTERM never unwinds.
        tokio::select! {
            result = socks5 => result,
            result = tun => result,
            () = shutdown_signal() => {
                tracing::info!("Shutdown signal received, tearing down");
                Ok(())
            }
        }
    }

    /// Prove the full path (client → server → internet) before opening any
    /// local entry point, so "running" never means "listening but unusable".
    async fn verify(&self) -> Result<()> {
        match verify_server_connection(&self.config).await {
            Ok(result) if result.success => {
                tracing::info!(
                    "Hello verification passed: {} ({} ms)",
                    result.message,
                    result.latency_ms
                );
                Ok(())
            }
            Ok(result) => {
                let msg = format!("Hello verification failed: {}", result.message);
                tracing::error!("{}", msg);
                Err(PhantomError::HelloVerification(msg))
            }
            Err(e) => {
                tracing::error!("Hello verification error: {}", e);
                Err(e)
            }
        }
    }

    async fn bind_socks5(&self) -> Result<TcpListener> {
        let listener = TcpListener::bind(&self.config.client.listen).await?;
        tracing::info!("SOCKS5 proxy listening on {}", self.config.client.listen);
        Ok(listener)
    }

    fn spawn_health_check(&self) {
        let failover = Arc::clone(&self.failover);
        tokio::spawn(async move {
            failover.run_health_check_loop().await;
        });
    }

    /// Serve the Prometheus endpoint on `client.metrics_listen`.
    fn spawn_metrics(&self) {
        let addr: std::net::SocketAddr = match self.config.client.metrics_listen.parse() {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    "Invalid client.metrics_listen '{}': {}; metrics disabled",
                    self.config.client.metrics_listen,
                    e
                );
                return;
            }
        };
        let stats = Arc::clone(&self.stats);
        tokio::spawn(async move {
            crate::stats::serve_metrics(stats, addr).await;
        });
    }

    /// Expose the shared counters (used by platform bridges and tests).
    pub fn stats(&self) -> Arc<crate::stats::TrafficStats> {
        Arc::clone(&self.stats)
    }

    async fn accept_socks5(&self, listener: TcpListener) -> Result<()> {
        loop {
            let (stream, peer) = listener.accept().await.map_err(PhantomError::Io)?;
            tracing::debug!("SOCKS5 connection from {}", peer);

            let config = self.config.clone();
            let failover = Arc::clone(&self.failover);
            let local_secret = self.local_secret;
            let quic_pool = Arc::clone(&self.quic_pool);
            let stats = Arc::clone(&self.stats);
            tokio::spawn(async move {
                if let Err(e) = handle_inbound(
                    stream,
                    &config,
                    &failover,
                    &quic_pool,
                    local_secret,
                    &stats,
                )
                .await
                {
                    tracing::debug!("Connection error from {}: {}", peer, e);
                }
            });
        }
    }
}
