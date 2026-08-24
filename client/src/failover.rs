use phantom_core::{ClientConfig, FailoverConfig, PhantomError, Result, ServerEntry};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

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

impl ServerState {
    fn healthy() -> Self {
        Self {
            consecutive_failures: 0,
            status: ServerStatus::Healthy,
        }
    }
}

/// Server pool plus its per-server health state.
///
/// Both live behind a single lock so a hot reload can swap the pool and the
/// state vector atomically — the two must never disagree on length.
struct Pool {
    servers: Vec<ServerEntry>,
    states: Vec<ServerState>,
    current: usize,
}

impl Pool {
    fn new(servers: Vec<ServerEntry>) -> Self {
        let states = servers.iter().map(|_| ServerState::healthy()).collect();
        Self {
            servers,
            states,
            current: 0,
        }
    }
}

/// Failover manager with per-server health tracking and active probing.
///
/// The server pool and the failover tuning are both hot-reloadable: the config
/// watcher calls [`FailoverManager::reload`] and every subsequent
/// [`FailoverManager::select_server`] sees the new pool.
pub struct FailoverManager {
    pool: RwLock<Pool>,
    tuning: RwLock<FailoverConfig>,
    /// Cached copy of `tuning.graceful_migration` so the switch paths never
    /// take the tuning lock while holding the pool lock (lock ordering).
    graceful_migration: AtomicBool,
    /// Epoch bumped on every active-server switch when graceful migration is
    /// disabled; relays subscribe and abort their tunnel on the next bump.
    migration_tx: watch::Sender<u64>,
}

impl FailoverManager {
    pub fn new(config: &ClientConfig) -> Result<Self> {
        if config.servers.is_empty() {
            return Err(PhantomError::AllServersFailed);
        }
        Ok(Self {
            pool: RwLock::new(Pool::new(config.servers.clone())),
            tuning: RwLock::new(config.failover.clone()),
            graceful_migration: AtomicBool::new(config.failover.graceful_migration),
            migration_tx: watch::channel(0u64).0,
        })
    }

    fn read_pool(&self) -> std::sync::RwLockReadGuard<'_, Pool> {
        self.pool.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write_pool(&self) -> std::sync::RwLockWriteGuard<'_, Pool> {
        self.pool.write().unwrap_or_else(|e| e.into_inner())
    }

    fn tuning(&self) -> FailoverConfig {
        self.tuning
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Return a snapshot of the currently selected server.
    ///
    /// The entry is cloned rather than borrowed so the pool lock is not held
    /// across the caller's `await` points, which is what makes hot reload safe.
    pub fn select_server(&self) -> Result<ServerEntry> {
        let pool = self.read_pool();
        pool.servers
            .get(pool.current)
            .or_else(|| pool.servers.first())
            .cloned()
            .ok_or(PhantomError::AllServersFailed)
    }

    /// Select the active server together with a migration watcher, atomically:
    /// the epoch the receiver observes is guaranteed to be no older than the
    /// selection, so a hard failover that happens after this call always
    /// reaches the tunnel.
    pub fn select_server_with_migration(&self) -> Result<(ServerEntry, watch::Receiver<u64>)> {
        let pool = self.read_pool();
        let server = pool
            .servers
            .get(pool.current)
            .or_else(|| pool.servers.first())
            .cloned()
            .ok_or(PhantomError::AllServersFailed)?;
        Ok((server, self.migration_tx.subscribe()))
    }

    /// Notify in-flight tunnels that the active server changed. No-op under
    /// the default `graceful_migration = true` policy.
    fn bump_migration_epoch(&self) {
        if !self.graceful_migration.load(Ordering::Relaxed) {
            self.migration_tx.send_modify(|epoch| *epoch += 1);
        }
    }

    /// Snapshot of the whole pool, in configuration order.
    pub fn servers(&self) -> Vec<ServerEntry> {
        self.read_pool().servers.clone()
    }

    pub fn report_failure(&self, server_name: &str) {
        let mut pool = self.write_pool();
        let idx = pool.current;
        if pool.servers.len() < 2 {
            return;
        }
        if pool.servers.get(idx).map(|s| s.name.as_str()) != Some(server_name) {
            return;
        }
        let next = (idx + 1) % pool.servers.len();
        tracing::warn!(
            "Server '{}' failed, switching to '{}'",
            server_name,
            pool.servers[next].name
        );
        pool.current = next;
        self.bump_migration_epoch();
    }

    pub fn report_success(&self, _server_name: &str) {
        // Handled by health check loop resetting counters.
    }

    /// Swap the server pool and failover tuning in place.
    ///
    /// The currently selected server is preserved when it survives the reload,
    /// so re-writing an unrelated part of the config does not migrate traffic.
    /// Returns `true` when the pool actually changed.
    pub fn reload(&self, config: &ClientConfig) -> bool {
        self.graceful_migration
            .store(config.failover.graceful_migration, Ordering::Relaxed);
        *self.tuning.write().unwrap_or_else(|e| e.into_inner()) = config.failover.clone();

        if config.servers.is_empty() {
            tracing::warn!("Config reload: server list is empty, keeping the previous pool");
            return false;
        }

        let mut pool = self.write_pool();
        if pool.servers == config.servers {
            return false;
        }

        let current_entry = pool.servers.get(pool.current).cloned();
        let current = current_entry
            .and_then(|entry| config.servers.iter().position(|s| *s == entry))
            .unwrap_or(0);

        let mut next = Pool::new(config.servers.clone());
        next.current = current;
        tracing::info!(
            "Config reloaded: {} server(s), active = '{}'",
            next.servers.len(),
            next.servers[current].name
        );
        let previous_active = pool.servers.get(pool.current).cloned();
        *pool = next;
        // A reload that retires the active server is a switch too: with
        // graceful migration disabled its tunnels must drop as well.
        if previous_active.as_ref() != pool.servers.get(pool.current) {
            self.bump_migration_epoch();
        }
        true
    }

    /// Run an infinite health-check loop.
    ///
    /// The loop keeps running even for a single-server pool so that a hot
    /// reload which adds servers starts being probed without a restart.
    pub async fn run_health_check_loop(self: Arc<Self>) {
        let mut interval_secs = self.tuning().health_check_interval;
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(1)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;

            // Pick up an interval change from a hot reload.
            let tuning = self.tuning();
            if tuning.health_check_interval != interval_secs {
                interval_secs = tuning.health_check_interval;
                ticker =
                    tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(1)));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            }

            let servers = self.servers();
            if servers.len() <= 1 {
                continue;
            }
            for (idx, server) in servers.into_iter().enumerate() {
                let mgr = Arc::clone(&self);
                let timeout = tuning.health_check_timeout;
                let threshold = tuning.failover_threshold;
                tokio::spawn(async move {
                    let healthy = probe_server(&server, timeout).await;
                    mgr.record_probe(idx, &server, healthy, threshold);
                });
            }
        }
    }

    /// Record a datapath connection failure as failover evidence.
    ///
    /// Health probes only run every `health_check_interval` seconds, which makes
    /// worst-case detection latency `interval x threshold`. A real connection
    /// failure observed on the datapath is equally strong evidence, so it feeds
    /// the same counter immediately — two failed user connections are enough to
    /// flip to the backup without waiting for the next probe tick.
    pub fn report_datapath_failure(&self, server_name: &str) {
        let (idx, server) = {
            let pool = self.read_pool();
            match pool
                .servers
                .iter()
                .enumerate()
                .find(|(_, s)| s.name == server_name)
            {
                Some((i, s)) => (i, s.clone()),
                None => return,
            }
        };
        let threshold = self.tuning().failover_threshold;
        self.record_probe(idx, &server, false, threshold);
    }

    /// Apply a probe result to the pool.
    ///
    /// Split out of the health-check loop so it holds no lock across an
    /// `await` and can be unit-tested without touching the network.
    fn record_probe(&self, idx: usize, server: &ServerEntry, healthy: bool, threshold: u32) {
        let mut pool = self.write_pool();
        // A reload may have shrunk or reordered the pool while the probe was
        // in flight; ignore results that no longer describe this slot.
        if pool.servers.get(idx) != Some(server) {
            return;
        }
        if healthy {
            if pool.states[idx].status != ServerStatus::Healthy {
                tracing::info!("Server '{}' is healthy again", server.name);
            }
            pool.states[idx] = ServerState::healthy();
            return;
        }

        let failures = pool.states[idx].consecutive_failures + 1;
        pool.states[idx].consecutive_failures = failures;
        tracing::warn!(
            "Server '{}' health check failed ({} consecutive)",
            server.name,
            failures
        );
        if failures < threshold {
            pool.states[idx].status = ServerStatus::Degraded;
            return;
        }

        pool.states[idx].status = ServerStatus::Down;
        if pool.current == idx && pool.servers.len() > 1 {
            let next = (idx + 1) % pool.servers.len();
            tracing::warn!(
                "Failover: '{}' down, switching to '{}'",
                server.name,
                pool.servers[next].name
            );
            pool.current = next;
            self.bump_migration_epoch();
        }
    }
}

/// Quick TCP connect probe.  Returns true if the server's TCP port is reachable.
async fn probe_server(server: &ServerEntry, timeout_secs: u64) -> bool {
    let addr: SocketAddr = match server.address.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::net::TcpStream::connect(addr),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Simple server selection for MVP (priority-based by order)
pub fn select_server(config: &ClientConfig) -> Result<&ServerEntry> {
    config.servers.first().ok_or(PhantomError::AllServersFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::{CipherPreference, TransportProtocol};

    fn entry(name: &str, port: u16) -> ServerEntry {
        ServerEntry {
            name: name.to_string(),
            address: format!("127.0.0.1:{}", port),
            public_key: "dGVzdA==".to_string(),
            psk: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
            cipher: CipherPreference::Auto,
            protocol: TransportProtocol::Tcp,
        }
    }

    fn config(servers: Vec<ServerEntry>) -> ClientConfig {
        ClientConfig {
            servers,
            ..ClientConfig::default()
        }
    }

    #[test]
    fn new_rejects_empty_pool() {
        assert!(FailoverManager::new(&config(Vec::new())).is_err());
    }

    #[test]
    fn reload_replaces_pool() {
        let mgr = FailoverManager::new(&config(vec![entry("a", 1)])).unwrap();
        assert_eq!(mgr.select_server().unwrap().name, "a");

        assert!(mgr.reload(&config(vec![entry("b", 2), entry("c", 3)])));
        assert_eq!(mgr.servers().len(), 2);
        // "a" is gone, so selection falls back to the head of the new pool.
        assert_eq!(mgr.select_server().unwrap().name, "b");
    }

    #[test]
    fn reload_preserves_active_server() {
        let mgr = FailoverManager::new(&config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        mgr.report_failure("a");
        assert_eq!(mgr.select_server().unwrap().name, "b");

        // "b" survives the reload but moves to the tail: it stays active.
        assert!(mgr.reload(&config(vec![entry("c", 3), entry("b", 2)])));
        assert_eq!(mgr.select_server().unwrap().name, "b");
    }

    #[test]
    fn reload_is_noop_for_identical_pool() {
        let servers = vec![entry("a", 1), entry("b", 2)];
        let mgr = FailoverManager::new(&config(servers.clone())).unwrap();
        assert!(!mgr.reload(&config(servers)));
    }

    #[test]
    fn reload_keeps_pool_when_new_config_has_no_servers() {
        let mgr = FailoverManager::new(&config(vec![entry("a", 1)])).unwrap();
        assert!(!mgr.reload(&config(Vec::new())));
        assert_eq!(mgr.select_server().unwrap().name, "a");
    }

    #[test]
    fn reload_updates_tuning() {
        let mut cfg = config(vec![entry("a", 1)]);
        let mgr = FailoverManager::new(&cfg).unwrap();
        assert_eq!(mgr.tuning().health_check_interval, 5);

        cfg.failover.health_check_interval = 7;
        mgr.reload(&cfg);
        assert_eq!(mgr.tuning().health_check_interval, 7);
    }

    #[test]
    fn probe_failures_switch_server_after_threshold() {
        let mgr = FailoverManager::new(&config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        let a = entry("a", 1);

        mgr.record_probe(0, &a, false, 3);
        mgr.record_probe(0, &a, false, 3);
        assert_eq!(mgr.select_server().unwrap().name, "a");

        mgr.record_probe(0, &a, false, 3);
        assert_eq!(mgr.select_server().unwrap().name, "b");
    }

    #[test]
    fn probe_success_resets_failure_counter() {
        let mgr = FailoverManager::new(&config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        let a = entry("a", 1);

        mgr.record_probe(0, &a, false, 3);
        mgr.record_probe(0, &a, false, 3);
        mgr.record_probe(0, &a, true, 3);
        mgr.record_probe(0, &a, false, 3);
        // Counter restarted, so the threshold is not reached yet.
        assert_eq!(mgr.select_server().unwrap().name, "a");
    }

    #[test]
    fn stale_probe_result_after_reload_is_ignored() {
        let mgr = FailoverManager::new(&config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        mgr.reload(&config(vec![entry("c", 3), entry("d", 4)]));
        // Probe of the retired "a" must not touch slot 0 of the new pool.
        mgr.record_probe(0, &entry("a", 1), false, 1);
        assert_eq!(mgr.select_server().unwrap().name, "c");
    }

    #[test]
    fn report_failure_ignores_single_server_pool() {
        let mgr = FailoverManager::new(&config(vec![entry("a", 1)])).unwrap();
        mgr.report_failure("a");
        assert_eq!(mgr.select_server().unwrap().name, "a");
    }

    fn hard_config(servers: Vec<ServerEntry>) -> ClientConfig {
        let mut cfg = config(servers);
        cfg.failover.graceful_migration = false;
        cfg
    }

    #[tokio::test]
    async fn hard_failover_bumps_migration_epoch() {
        let mgr = FailoverManager::new(&hard_config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        let (server, mut rx) = mgr.select_server_with_migration().unwrap();
        assert_eq!(server.name, "a");

        mgr.report_failure("a");
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.changed())
            .await
            .expect("hard failover must notify in-flight tunnels")
            .expect("sender alive");
    }

    #[tokio::test]
    async fn graceful_policy_never_bumps_epoch() {
        let mgr = FailoverManager::new(&config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        let (_server, rx) = mgr.select_server_with_migration().unwrap();

        mgr.report_failure("a");
        assert_eq!(mgr.select_server().unwrap().name, "b");
        assert!(
            !rx.has_changed().unwrap(),
            "graceful migration must leave in-flight tunnels alone"
        );
    }

    #[tokio::test]
    async fn probe_driven_switch_also_bumps_epoch() {
        let mgr = FailoverManager::new(&hard_config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        let (_server, rx) = mgr.select_server_with_migration().unwrap();
        let a = entry("a", 1);

        mgr.record_probe(0, &a, false, 1);
        assert_eq!(mgr.select_server().unwrap().name, "b");
        assert!(rx.has_changed().unwrap());
    }

    #[tokio::test]
    async fn reload_retiring_active_server_bumps_epoch() {
        let mgr = FailoverManager::new(&hard_config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        let (_server, rx) = mgr.select_server_with_migration().unwrap();

        // "a" is gone from the new pool: the active selection moves.
        let mut next = hard_config(vec![entry("b", 2)]);
        next.failover.graceful_migration = false;
        assert!(mgr.reload(&next));
        assert!(rx.has_changed().unwrap());
    }

    #[tokio::test]
    async fn reload_preserving_active_server_does_not_bump() {
        let mgr = FailoverManager::new(&hard_config(vec![entry("a", 1), entry("b", 2)])).unwrap();
        let (_server, rx) = mgr.select_server_with_migration().unwrap();

        // Same servers, shuffled: "a" is still the active entry.
        let mut next = hard_config(vec![entry("b", 2), entry("a", 1)]);
        next.failover.graceful_migration = false;
        assert!(mgr.reload(&next));
        assert!(!rx.has_changed().unwrap());
    }
}
