use phantom_core::{ClientConfig, PhantomError, Result, ServerEntry};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
pub enum ServerStatus {
    Healthy,
    Degraded,
    Down,
}

pub struct ServerState {
    pub consecutive_failures: u32,
    pub status: ServerStatus,
}

/// Failover manager with per-server health tracking and active probing.
pub struct FailoverManager {
    servers: Vec<ServerEntry>,
    current: AtomicUsize,
    states: Vec<Mutex<ServerState>>,
    health_check_interval: u64,
    health_check_timeout: u64,
    failover_threshold: u32,
}

impl FailoverManager {
    pub fn new(config: &ClientConfig) -> Result<Self> {
        if config.servers.is_empty() {
            return Err(PhantomError::AllServersFailed);
        }
        let states = config
            .servers
            .iter()
            .map(|_| Mutex::new(ServerState {
                consecutive_failures: 0,
                status: ServerStatus::Healthy,
            }))
            .collect();
        Ok(Self {
            servers: config.servers.clone(),
            current: AtomicUsize::new(0),
            states,
            health_check_interval: config.failover.health_check_interval,
            health_check_timeout: config.failover.health_check_timeout,
            failover_threshold: config.failover.failover_threshold,
        })
    }

    pub fn select_server(&self) -> Result<&ServerEntry> {
        let idx = self.current.load(Ordering::Relaxed);
        if idx < self.servers.len() {
            Ok(&self.servers[idx])
        } else {
            self.servers.first().ok_or(PhantomError::AllServersFailed)
        }
    }

    pub fn report_failure(&self, server_name: &str) {
        let idx = self.current.load(Ordering::Relaxed);
        if idx < self.servers.len() && self.servers[idx].name == server_name {
            let next = (idx + 1) % self.servers.len();
            tracing::warn!(
                "Server '{}' failed, switching to '{}'",
                server_name,
                self.servers[next].name
            );
            self.current.store(next, Ordering::Relaxed);
        }
    }

    pub fn report_success(&self, _server_name: &str) {
        // Handled by health check loop resetting counters.
    }

    /// Run an infinite health-check loop.  Spawns its own tasks.
    pub async fn run_health_check_loop(self: Arc<Self>) {
        if self.servers.len() <= 1 {
            return;
        }
        let interval = std::time::Duration::from_secs(self.health_check_interval);
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            for (idx, server) in self.servers.iter().enumerate() {
                let mgr = Arc::clone(&self);
                let server = server.clone();
                tokio::spawn(async move {
                    let healthy = mgr.probe_server(&server).await;
                    let mut state = mgr.states[idx].lock().await;
                    if healthy {
                        if state.status != ServerStatus::Healthy {
                            tracing::info!("Server '{}' is healthy again", server.name);
                        }
                        state.consecutive_failures = 0;
                        state.status = ServerStatus::Healthy;
                    } else {
                        state.consecutive_failures += 1;
                        tracing::warn!(
                            "Server '{}' health check failed ({} consecutive)",
                            server.name,
                            state.consecutive_failures
                        );
                        if state.consecutive_failures >= mgr.failover_threshold {
                            state.status = ServerStatus::Down;
                            let current = mgr.current.load(Ordering::Relaxed);
                            if current == idx {
                                let next = (current + 1) % mgr.servers.len();
                                tracing::warn!(
                                    "Failover: '{}' down, switching to '{}'",
                                    server.name,
                                    mgr.servers[next].name
                                );
                                mgr.current.store(next, Ordering::Relaxed);
                            }
                        } else {
                            state.status = ServerStatus::Degraded;
                        }
                    }
                });
            }
        }
    }

    /// Quick TCP connect probe.  Returns true if the server's TCP port is reachable.
    async fn probe_server(&self, server: &ServerEntry) -> bool {
        let addr: SocketAddr = match server.address.parse() {
            Ok(a) => a,
            Err(_) => return false,
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(self.health_check_timeout),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        {
            Ok(Ok(_)) => true,
            _ => false,
        }
    }
}

/// Simple server selection for MVP (priority-based by order)
pub fn select_server(config: &ClientConfig) -> Result<&ServerEntry> {
    config
        .servers
        .first()
        .ok_or(PhantomError::AllServersFailed)
}
