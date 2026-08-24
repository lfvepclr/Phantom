//! TUN-based transparent proxy (tun2socks-lite) with Smart routing.
//!
//! Replaces SOCKS5 as the client entry-point for Android/macOS native apps.
//! Supports:
//! - TCP flow relay via local SOCKS5 proxy (Proxy mode)
//! - TCP direct connection (Direct mode, bypass tunnel)
//! - DNS hijack (UDP:53 intercepted and forwarded to upstream DNS)
//! - Rule-based routing (Smart mode)

use bytes::{Bytes, BytesMut};
use etherparse::IpNumber;
use phantom_core::protocol::TargetAddr;
use phantom_core::{PhantomError, ProxyMode, Result, RuleAction, ServerEntry};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::dns::{
    DnsCache, DnsProxy, DnsQueryContext, build_dns_response_packet, extract_a_records,
    extract_query_domain, parse_dns_addr,
};
use crate::failover::FailoverManager;
use crate::rules::RuleEngine;
use crate::stats::TrafficStats;

const TUN_MTU: usize = 1500;

/// Default TUN interface name per platform.
///
/// macOS requires the `utun<N>` naming convention; Linux (CLI / router) has no
/// such constraint so a descriptive name is used instead.
pub const DEFAULT_TUN_NAME: &str = if cfg!(target_os = "macos") {
    "utun7"
} else {
    "phantom0"
};

/// Settings for a self-created TUN device (macOS / Linux).
///
/// Android and HarmonyOS hand the fd over from the OS VPN service instead and
/// therefore ignore this struct entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunSettings {
    pub name: String,
    pub address: std::net::Ipv4Addr,
    pub netmask: std::net::Ipv4Addr,
    pub mtu: u16,
}

impl Default for TunSettings {
    fn default() -> Self {
        Self {
            name: DEFAULT_TUN_NAME.to_string(),
            address: std::net::Ipv4Addr::new(10, 7, 0, 1),
            netmask: std::net::Ipv4Addr::new(255, 255, 255, 0),
            mtu: TUN_MTU as u16,
        }
    }
}

/// A TUN device wrapper that works on both macOS (self-created) and Android
/// (fd passed from VpnService).
pub struct TunDevice {
    #[cfg(not(any(target_os = "android", target_env = "ohos")))]
    inner: tun::AsyncDevice,
    #[cfg(any(target_os = "android", target_env = "ohos"))]
    inner: AsyncFd<FdWrapper>,
}

#[cfg(any(target_os = "android", target_env = "ohos"))]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(any(target_os = "android", target_env = "ohos"))]
use tokio::io::unix::AsyncFd;

#[cfg(any(target_os = "android", target_env = "ohos"))]
#[derive(Debug)]
struct FdWrapper(OwnedFd);

#[cfg(any(target_os = "android", target_env = "ohos"))]
impl AsRawFd for FdWrapper {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
impl TunDevice {
    pub fn create() -> Result<Self> {
        Self::create_with(&TunSettings::default())
    }

    pub fn create_with(settings: &TunSettings) -> Result<Self> {
        let mut config = tun::Configuration::default();
        config
            .tun_name(&settings.name)
            .address(settings.address)
            .netmask(settings.netmask)
            .mtu(settings.mtu)
            .up();

        let dev = tun::create_as_async(&config)
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        Ok(Self { inner: dev })
    }
}

#[cfg(any(target_os = "android", target_env = "ohos"))]
impl TunDevice {
    /// Wrap a raw TUN file descriptor (passed from the OS / VpnService) into a
    /// [TunDevice].
    ///
    /// The function first validates that `fd` is open via `fcntl(F_GETFD)`
    /// before taking ownership, so calling it with a stale or closed fd returns
    /// an error rather than undefined behavior.
    pub fn from_fd(fd: RawFd) -> Result<Self> {
        // Validate the fd is open before taking ownership.
        // SAFETY: `fd` is assumed to be a valid raw fd at the call site; we
        // only read its flags and do not take ownership yet.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD, 0) };
        if flags < 0 {
            return Err(PhantomError::Io(std::io::Error::last_os_error()));
        }
        // SAFETY: `fd` is validated above as an open file descriptor and
        // ownership is transferred from the caller (Kotlin/ArkTS) into Rust.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let async_fd = AsyncFd::new(FdWrapper(owned)).map_err(PhantomError::Io)?;
        Ok(Self { inner: async_fd })
    }
}

/// Host-only stub for `TunDevice::from_fd` so that the Android platform
/// module (which HarmonyOS reuses) can be compiled on macOS/Linux for
/// `cargo check` and other host-side validation.
#[cfg(all(
    target_family = "unix",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl TunDevice {
    pub fn from_fd(_fd: std::os::unix::io::RawFd) -> Result<Self> {
        Err(PhantomError::Config(
            "TUN fd hand-off is only used on Android/HarmonyOS".to_string(),
        ))
    }
}

impl TunDevice {
    /// Read a raw IP packet into `buf`.
    pub async fn read_packet(&mut self, buf: &mut BytesMut) -> Result<usize> {
        #[cfg(not(any(target_os = "android", target_env = "ohos")))]
        {
            buf.clear();
            let n = self.inner.read_buf(buf).await.map_err(PhantomError::Io)?;
            Ok(n)
        }
        #[cfg(any(target_os = "android", target_env = "ohos"))]
        {
            buf.clear();
            buf.resize(TUN_MTU, 0);
            loop {
                let mut guard = self.inner.readable().await.map_err(PhantomError::Io)?;
                // SAFETY: `buf` is a `BytesMut` resized to `TUN_MTU` bytes, and
                // `read` is only allowed to write within its bounds.  The fd is
                // registered with `AsyncFd` and confirmed readable above.
                let n = unsafe {
                    libc::read(
                        guard.get_inner().as_raw_fd(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    )
                };
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::WouldBlock {
                        guard.clear_ready();
                        continue;
                    }
                    return Err(PhantomError::Io(err));
                }
                buf.truncate(n as usize);
                return Ok(n as usize);
            }
        }
    }

    /// Write a raw IP packet.
    pub async fn write_packet(&mut self, pkt: &[u8]) -> Result<()> {
        #[cfg(not(any(target_os = "android", target_env = "ohos")))]
        {
            self.inner.write_all(pkt).await.map_err(PhantomError::Io)?;
            Ok(())
        }
        #[cfg(any(target_os = "android", target_env = "ohos"))]
        {
            let mut offset = 0;
            while offset < pkt.len() {
                let mut guard = self.inner.writable().await.map_err(PhantomError::Io)?;
                // SAFETY: `pkt` outlives this call and `offset`/`len` are kept
                // within bounds.  The fd is registered with `AsyncFd` and
                // confirmed writable above.
                let n = unsafe {
                    libc::write(
                        guard.get_inner().as_raw_fd(),
                        pkt.as_ptr().add(offset) as *const libc::c_void,
                        pkt.len() - offset,
                    )
                };
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::WouldBlock {
                        guard.clear_ready();
                        continue;
                    }
                    return Err(PhantomError::Io(err));
                }
                offset += n as usize;
            }
            Ok(())
        }
    }
}

/// 5-tuple flow identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlowKey {
    pub src_ip: IpAddr,
    pub dst_ip: IpAddr,
    pub src_port: u16,
    pub dst_port: u16,
    pub proto: u8,
}

/// Per-flow handle shared between the TUN loop and the relay task.
#[derive(Clone)]
pub struct FlowHandle {
    pub src_addr: SocketAddr,
    pub dst_addr: SocketAddr,
    pub state: Arc<Mutex<TcpFlowState>>,
    pub tx_to_relay: tokio::sync::mpsc::UnboundedSender<Bytes>,
}

/// Minimal TCP state for a tun2socks flow.
pub struct TcpFlowState {
    pub seq: u32,
    pub ack: u32,
    pub src_ip: IpAddr,
    pub dst_ip: IpAddr,
    pub src_port: u16,
    pub dst_port: u16,
}

/// Table of active flows.
pub struct FlowTable {
    flows: Arc<Mutex<HashMap<FlowKey, FlowHandle>>>,
}

impl FlowTable {
    pub fn new() -> Self {
        Self {
            flows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn get(&self, key: &FlowKey) -> Option<FlowHandle> {
        self.flows.lock().await.get(key).cloned()
    }

    pub async fn insert(&self, key: FlowKey, handle: FlowHandle) {
        self.flows.lock().await.insert(key, handle);
    }

    pub async fn remove(&self, key: &FlowKey) {
        self.flows.lock().await.remove(key);
    }
}

impl Clone for FlowTable {
    fn clone(&self) -> Self {
        Self {
            flows: Arc::clone(&self.flows),
        }
    }
}

/// Table of active UDP direct-relay sockets.
struct UdpFlowTable {
    flows: Arc<Mutex<HashMap<FlowKey, Arc<tokio::net::UdpSocket>>>>,
}

impl UdpFlowTable {
    fn new() -> Self {
        Self {
            flows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn get_or_create(&self, key: &FlowKey) -> Result<Arc<tokio::net::UdpSocket>> {
        let mut map = self.flows.lock().await;
        if let Some(sock) = map.get(key) {
            return Ok(Arc::clone(sock));
        }
        let sock = tokio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(PhantomError::Io)?;
        let arc = Arc::new(sock);
        map.insert(*key, Arc::clone(&arc));
        Ok(arc)
    }

    async fn remove(&self, key: &FlowKey) {
        self.flows.lock().await.remove(key);
    }
}

impl Clone for UdpFlowTable {
    fn clone(&self) -> Self {
        Self {
            flows: Arc::clone(&self.flows),
        }
    }
}

/// Tracks active UDP proxy flows (sending UDP through the Phantom tunnel).
/// Each flow holds a sender channel for injecting datagrams into the relay task.
struct UdpProxyFlowTable {
    flows: Arc<Mutex<HashMap<FlowKey, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
}

impl UdpProxyFlowTable {
    fn new() -> Self {
        Self {
            flows: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Clone for UdpProxyFlowTable {
    fn clone(&self) -> Self {
        Self {
            flows: Arc::clone(&self.flows),
        }
    }
}

/// Hot-reloadable state shared between the TUN proxy loop and the reload task.
struct HotReloadState {
    proxy_mode: ProxyMode,
    rule_engine: Option<Arc<RuleEngine>>,
    /// Server used for tunnelled UDP flows. Kept here rather than on
    /// [`TunProxy`] so a config reload can retarget new UDP flows.
    server: Option<ServerEntry>,
}

/// Apply a freshly parsed config to every hot-reloadable component.
///
/// Covers proxy mode, rule engine, DNS upstream and the server pool (both the
/// tunnelled-UDP server and the SOCKS5 relay's failover pool). Established
/// flows keep their original routing; only new flows see the update.
async fn apply_reload(
    hot: &Arc<Mutex<HotReloadState>>,
    dns_proxy: Option<&DnsProxy>,
    failover: Option<&FailoverManager>,
    cfg: &phantom_core::ClientConfig,
) {
    {
        let mut state = hot.lock().await;
        if state.proxy_mode != cfg.client.mode {
            tracing::info!(
                "Config reloaded: mode {:?} -> {:?}",
                state.proxy_mode,
                cfg.client.mode
            );
            state.proxy_mode = cfg.client.mode;
        }
        match RuleEngine::from_config(&cfg.rules) {
            Ok(engine) => {
                state.rule_engine = Some(Arc::new(engine));
                tracing::info!("Config reloaded: {} rule(s) active", cfg.rules.rules.len());
            }
            // Keep the previous engine rather than silently falling back to
            // "proxy everything" when the new rule set is malformed.
            Err(e) => tracing::warn!("Config reload: rule parse failed, keeping old rules: {}", e),
        }
        if let Some(server) = cfg.servers.first() {
            if state.server.as_ref() != Some(server) {
                tracing::info!("Config reloaded: UDP proxy server -> '{}'", server.name);
                state.server = Some(server.clone());
            }
        }
    }

    if let Some(dns) = dns_proxy {
        match parse_dns_addr(&cfg.client.dns) {
            Some(addr) => {
                dns.set_upstream(addr);
            }
            None => tracing::warn!(
                "Config reload: invalid client.dns '{}', keeping upstream {}",
                cfg.client.dns,
                dns.upstream()
            ),
        }
    }

    if let Some(failover) = failover {
        failover.reload(cfg);
    }
}

/// Main TUN transparent proxy.
pub struct TunProxy {
    device: Arc<Mutex<TunDevice>>,
    flows: FlowTable,
    udp_flows: UdpFlowTable,
    udp_proxy_flows: UdpProxyFlowTable,
    socks5_addr: SocketAddr,
    hot: Arc<Mutex<HotReloadState>>,
    local_secret: Option<[u8; 32]>,
    dns_proxy: Option<Arc<DnsProxy>>,
    dns_cache: DnsCache,
    config_path: Option<String>,
    failover: Option<Arc<FailoverManager>>,
    stats: Arc<TrafficStats>,
}

impl TunProxy {
    pub fn new(device: TunDevice, socks5_addr: SocketAddr) -> Self {
        Self {
            device: Arc::new(Mutex::new(device)),
            flows: FlowTable::new(),
            udp_flows: UdpFlowTable::new(),
            udp_proxy_flows: UdpProxyFlowTable::new(),
            socks5_addr,
            hot: Arc::new(Mutex::new(HotReloadState {
                proxy_mode: ProxyMode::Smart,
                rule_engine: None,
                server: None,
            })),
            local_secret: None,
            dns_proxy: None,
            dns_cache: DnsCache::new(),
            config_path: None,
            failover: None,
            stats: TrafficStats::new(),
        }
    }

    /// Lock the hot-reload state during construction.
    ///
    /// `try_lock` rather than `blocking_lock`: the builders are called from
    /// inside async contexts (the platform bridges build the proxy in a spawned
    /// task), and `blocking_lock` panics there. The lock is uncontended by
    /// construction because the proxy has not been shared with any task yet.
    fn hot_mut(&self) -> tokio::sync::MutexGuard<'_, HotReloadState> {
        self.hot
            .try_lock()
            .expect("TunProxy builders run before the proxy is shared")
    }

    pub fn with_mode(self, mode: ProxyMode) -> Self {
        self.hot_mut().proxy_mode = mode;
        self
    }

    pub fn with_server(mut self, server: ServerEntry, secret: [u8; 32]) -> Self {
        self.hot_mut().server = Some(server);
        self.local_secret = Some(secret);
        self
    }

    pub fn with_rules(self, engine: RuleEngine) -> Self {
        self.hot_mut().rule_engine = Some(Arc::new(engine));
        self
    }

    pub fn with_config_path(mut self, path: String) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Wire the shared failover manager in so a config reload can swap the
    /// server pool used by the local SOCKS5 relay as well.
    pub fn with_failover(mut self, failover: Arc<FailoverManager>) -> Self {
        self.failover = Some(failover);
        self
    }

    pub fn with_dns(mut self, proxy: DnsProxy) -> Self {
        self.dns_proxy = Some(Arc::new(proxy));
        self
    }

    /// Share the caller's stats instance so SOCKS5 and TUN traffic land in the
    /// same counters (the metrics endpoint serves exactly one `TrafficStats`).
    pub fn with_stats(mut self, stats: Arc<TrafficStats>) -> Self {
        self.stats = stats;
        self
    }

    pub fn stats(&self) -> Arc<TrafficStats> {
        Arc::clone(&self.stats)
    }

    pub async fn run(&self) -> Result<()> {
        // Spawn DNS response handler.
        if let Some(dns) = &self.dns_proxy {
            let dns = Arc::clone(dns);
            let cache = self.dns_cache.clone();
            let device = Arc::clone(&self.device);
            tokio::spawn(async move {
                let _ = dns
                    .run(|payload, ctx, domain| {
                        let cache = cache.clone();
                        let device = device.clone();
                        async move {
                            if let Some(ref domain) = domain {
                                for ip in extract_a_records(&payload) {
                                    cache.insert(ip, domain.clone()).await;
                                }
                            }
                            let pkt = build_dns_response_packet(&payload, &ctx)?;
                            let mut dev = device.lock().await;
                            dev.write_packet(&pkt).await?;
                            Ok(())
                        }
                    })
                    .await;
            });
        }

        // Spawn config hot-reload watcher.
        if let Some(path) = &self.config_path {
            let path = path.clone();
            let hot = Arc::clone(&self.hot);
            let dns_proxy = self.dns_proxy.clone();
            let failover = self.failover.clone();
            let interval = std::time::Duration::from_secs(5);
            tokio::spawn(async move {
                let mut last_mtime = std::time::SystemTime::UNIX_EPOCH;
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;
                    let mtime = match tokio::fs::metadata(&path).await {
                        Ok(m) => m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                        Err(_) => continue,
                    };
                    if mtime <= last_mtime {
                        continue;
                    }
                    last_mtime = mtime;
                    let content = match tokio::fs::read_to_string(&path).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("Config reload read error: {}", e);
                            continue;
                        }
                    };
                    let cfg = match toml::from_str::<phantom_core::ClientConfig>(&content) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("Config reload parse error: {}", e);
                            continue;
                        }
                    };
                    apply_reload(&hot, dns_proxy.as_deref(), failover.as_deref(), &cfg).await;
                }
            });
        }

        // Metrics are served by the tunnel runtime (`PhantomClient::run` /
        // `run_tun`) over the shared stats instance — nothing to spawn here.

        let mut buf = BytesMut::with_capacity(TUN_MTU);
        loop {
            let n = {
                let mut dev = self.device.lock().await;
                dev.read_packet(&mut buf).await?
            };
            if n == 0 {
                continue;
            }
            if let Err(e) = self.handle_packet(&buf[..n]).await {
                tracing::debug!("TUN packet error: {}", e);
            }
        }
    }

    async fn handle_packet(&self, pkt: &[u8]) -> Result<()> {
        if let Ok(ip) = etherparse::Ipv4HeaderSlice::from_slice(pkt) {
            let ip_header_len = ip.ihl() as usize * 4;
            let payload = &pkt[ip_header_len..];
            let src_ip = IpAddr::V4(ip.source_addr());
            let dst_ip = IpAddr::V4(ip.destination_addr());
            match ip.protocol() {
                IpNumber::TCP => self.handle_tcp(payload, src_ip, dst_ip).await,
                IpNumber::UDP => self.handle_udp(payload, src_ip, dst_ip).await,
                _ => Ok(()),
            }
        } else if let Ok(ip) = etherparse::Ipv6HeaderSlice::from_slice(pkt) {
            let payload = &pkt[ip.slice().len()..];
            let src_ip = IpAddr::V6(ip.source_addr());
            let dst_ip = IpAddr::V6(ip.destination_addr());
            match ip.next_header() {
                IpNumber::TCP => self.handle_tcp(payload, src_ip, dst_ip).await,
                IpNumber::UDP => self.handle_udp(payload, src_ip, dst_ip).await,
                _ => Ok(()),
            }
        } else {
            Ok(())
        }
    }

    async fn handle_tcp(&self, payload: &[u8], src_ip: IpAddr, dst_ip: IpAddr) -> Result<()> {
        let tcp = etherparse::TcpHeaderSlice::from_slice(payload)
            .map_err(|e| PhantomError::Protocol(format!("TCP parse: {:?}", e)))?;
        let tcp_header_len = tcp.slice().len();
        let data = &payload[tcp_header_len..];

        let src_port = tcp.source_port();
        let dst_port = tcp.destination_port();
        let key = FlowKey {
            src_ip,
            dst_ip,
            src_port,
            dst_port,
            proto: IpNumber::TCP.0,
        };

        let syn = tcp.syn();
        let fin = tcp.fin();
        let rst = tcp.rst();
        let ack = tcp.ack();

        if rst {
            if let Some(flow) = self.flows.get(&key).await {
                let _ = flow.tx_to_relay.send(Bytes::new());
                self.flows.remove(&key).await;
            }
            return Ok(());
        }

        if syn && !ack {
            self.stats.record_tcp_connect();
            let domain = match dst_ip {
                IpAddr::V4(v4) => self.dns_cache.lookup(v4).await,
                _ => None,
            };
            let hot = self.hot.lock().await;
            let proxy_mode = hot.proxy_mode;
            let rule_engine = hot.rule_engine.clone();
            drop(hot);
            let action = match proxy_mode {
                ProxyMode::Proxy => RuleAction::Proxy,
                ProxyMode::Direct => RuleAction::Direct,
                ProxyMode::Smart | ProxyMode::Auto => {
                    if let Some(engine) = &rule_engine {
                        engine.query(domain.as_deref(), Some(dst_ip), Some(dst_port))
                    } else {
                        RuleAction::Proxy
                    }
                }
            };

            match action {
                RuleAction::Direct => {
                    self.spawn_direct_tcp_flow(
                        key,
                        src_ip,
                        dst_ip,
                        src_port,
                        dst_port,
                        tcp.sequence_number(),
                    )
                    .await?;
                }
                RuleAction::Proxy => {
                    self.spawn_tcp_flow(
                        key,
                        src_ip,
                        dst_ip,
                        src_port,
                        dst_port,
                        tcp.sequence_number(),
                    )
                    .await?;
                }
                RuleAction::Reject => {
                    self.send_tcp_rst(
                        key,
                        src_ip,
                        dst_ip,
                        src_port,
                        dst_port,
                        tcp.sequence_number(),
                    )
                    .await?;
                }
            }
            return Ok(());
        }

        if let Some(flow) = self.flows.get(&key).await {
            if fin {
                let _ = flow.tx_to_relay.send(Bytes::new());
                self.flows.remove(&key).await;
                return Ok(());
            }

            if !data.is_empty() {
                let _ = flow.tx_to_relay.send(Bytes::copy_from_slice(data));
                let state = flow.state.lock().await;
                self.send_tcp_ack(
                    &state,
                    tcp.sequence_number().wrapping_add(data.len() as u32),
                )
                .await?;
            }
        }

        let _ = ack;
        Ok(())
    }

    async fn handle_udp(&self, payload: &[u8], src_ip: IpAddr, dst_ip: IpAddr) -> Result<()> {
        let udp = etherparse::UdpHeaderSlice::from_slice(payload)
            .map_err(|e| PhantomError::Protocol(format!("UDP parse: {:?}", e)))?;
        let data = &payload[udp.slice().len()..];
        let src_port = udp.source_port();
        let dst_port = udp.destination_port();

        // DNS hijack.
        if dst_port == 53 {
            if let Some(dns) = &self.dns_proxy {
                let ctx = DnsQueryContext {
                    src_ip,
                    src_port,
                    dst_ip,
                    dst_port,
                };
                if let Some((domain, _)) = extract_query_domain(data) {
                    tracing::debug!("DNS query for {}", domain);
                }
                dns.forward(data, ctx).await?;
                return Ok(());
            }
        }

        self.stats.record_udp_up(data.len() as u64);

        let domain = match dst_ip {
            IpAddr::V4(v4) => self.dns_cache.lookup(v4).await,
            _ => None,
        };
        let hot = self.hot.lock().await;
        let proxy_mode = hot.proxy_mode;
        let rule_engine = hot.rule_engine.clone();
        drop(hot);
        let action = match proxy_mode {
            ProxyMode::Proxy => RuleAction::Proxy,
            ProxyMode::Direct => RuleAction::Direct,
            ProxyMode::Smart | ProxyMode::Auto => {
                if let Some(engine) = &rule_engine {
                    engine.query(domain.as_deref(), Some(dst_ip), Some(dst_port))
                } else {
                    RuleAction::Proxy
                }
            }
        };

        match action {
            RuleAction::Direct => {
                let key = FlowKey {
                    src_ip,
                    dst_ip,
                    src_port,
                    dst_port,
                    proto: IpNumber::UDP.0,
                };
                let socket = self.udp_flows.get_or_create(&key).await?;
                let dst_sa = SocketAddr::new(dst_ip, dst_port);
                socket
                    .send_to(data, dst_sa)
                    .await
                    .map_err(PhantomError::Io)?;

                // Spawn receiver for this UDP flow if not already running.
                let device = Arc::clone(&self.device);
                let udp_flows = self.udp_flows.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    loop {
                        let n = match socket.recv_from(&mut buf).await {
                            Ok((n, _peer)) => n,
                            Err(e) => {
                                tracing::debug!("UDP recv error: {}", e);
                                break;
                            }
                        };
                        let pkt =
                            match build_udp_packet(dst_ip, dst_port, src_ip, src_port, &buf[..n]) {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::debug!("UDP packet build error: {}", e);
                                    continue;
                                }
                            };
                        {
                            let mut dev = device.lock().await;
                            if let Err(e) = dev.write_packet(&pkt).await {
                                tracing::debug!("TUN write error: {}", e);
                                break;
                            }
                        }
                    }
                    udp_flows.remove(&key).await;
                });
            }
            RuleAction::Proxy => {
                let key = FlowKey {
                    src_ip,
                    dst_ip,
                    src_port,
                    dst_port,
                    proto: IpNumber::UDP.0,
                };
                if let Err(e) = self
                    .spawn_udp_proxy_flow(&key, dst_ip, dst_port, data.to_vec())
                    .await
                {
                    tracing::debug!("UDP proxy flow error: {}", e);
                }
            }
            RuleAction::Reject => {
                tracing::debug!(
                    "UDP {}:{} -> {}:{} ({} bytes) - rejected by rule",
                    src_ip,
                    src_port,
                    dst_ip,
                    dst_port,
                    data.len()
                );
            }
        }
        Ok(())
    }

    async fn spawn_tcp_flow(
        &self,
        key: FlowKey,
        src_ip: IpAddr,
        dst_ip: IpAddr,
        src_port: u16,
        dst_port: u16,
        client_seq: u32,
    ) -> Result<()> {
        let (tx_to_relay, rx_from_tun) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
        let state = Arc::new(Mutex::new(TcpFlowState {
            seq: 1000,
            ack: client_seq.wrapping_add(1),
            src_ip,
            dst_ip,
            src_port,
            dst_port,
        }));

        let handle = FlowHandle {
            src_addr: SocketAddr::new(src_ip, src_port),
            dst_addr: SocketAddr::new(dst_ip, dst_port),
            state: Arc::clone(&state),
            tx_to_relay,
        };
        self.flows.insert(key, handle).await;

        {
            let s = state.lock().await;
            self.send_tcp_syn_ack(&s).await?;
        }

        let device = Arc::clone(&self.device);
        let flows = self.flows.clone();
        let socks5_addr = self.socks5_addr;
        tokio::spawn(async move {
            if let Err(e) = tcp_relay_task(
                rx_from_tun,
                device,
                flows,
                key,
                state,
                socks5_addr,
                dst_ip,
                dst_port,
            )
            .await
            {
                tracing::debug!("TCP relay task ended: {}", e);
            }
        });

        Ok(())
    }

    async fn spawn_direct_tcp_flow(
        &self,
        key: FlowKey,
        src_ip: IpAddr,
        dst_ip: IpAddr,
        src_port: u16,
        dst_port: u16,
        client_seq: u32,
    ) -> Result<()> {
        let (tx_to_relay, rx_from_tun) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
        let state = Arc::new(Mutex::new(TcpFlowState {
            seq: 1000,
            ack: client_seq.wrapping_add(1),
            src_ip,
            dst_ip,
            src_port,
            dst_port,
        }));

        let handle = FlowHandle {
            src_addr: SocketAddr::new(src_ip, src_port),
            dst_addr: SocketAddr::new(dst_ip, dst_port),
            state: Arc::clone(&state),
            tx_to_relay,
        };
        self.flows.insert(key, handle).await;

        {
            let s = state.lock().await;
            self.send_tcp_syn_ack(&s).await?;
        }

        let device = Arc::clone(&self.device);
        let flows = self.flows.clone();
        tokio::spawn(async move {
            if let Err(e) =
                tcp_direct_relay_task(rx_from_tun, device, flows, key, state, dst_ip, dst_port)
                    .await
            {
                tracing::debug!("TCP direct relay task ended: {}", e);
            }
        });

        Ok(())
    }

    /// Send a UDP datagram through the Phantom tunnel.
    /// If a proxy flow already exists for this key, send via channel; otherwise create one.
    async fn spawn_udp_proxy_flow(
        &self,
        key: &FlowKey,
        dst_ip: IpAddr,
        dst_port: u16,
        mut datagram: Vec<u8>,
    ) -> Result<()> {
        // Try sending to an existing flow; a dead sender means the pump has
        // ended, so fall through and re-establish instead of dropping data.
        {
            let map = self.udp_proxy_flows.flows.lock().await;
            if let Some(tx) = map.get(key) {
                match tx.send(datagram) {
                    Ok(()) => return Ok(()),
                    // Recover the datagram from the failed send.
                    Err(e) => datagram = e.0,
                }
            }
        }
        self.udp_proxy_flows.flows.lock().await.remove(key);

        // Need server info to establish a direct tunnel.
        let (server, local_secret) = {
            let hot = self.hot.lock().await;
            let server = hot.server.clone().ok_or_else(|| {
                PhantomError::Config("No server configured for UDP proxy".to_string())
            })?;
            let secret = self.local_secret.ok_or_else(|| {
                PhantomError::Config("No local secret for UDP proxy".to_string())
            })?;
            (server, secret)
        };

        let target = match dst_ip {
            IpAddr::V4(v4) => TargetAddr::IPv4(v4.octets(), dst_port),
            IpAddr::V6(v6) => TargetAddr::IPv6(v6.octets(), dst_port),
        };

        // Shared tunnel plumbing (also used by SOCKS5 UDP ASSOCIATE).
        let flow =
            crate::udp_relay::establish_udp_flow_tcp(&server, &local_secret, target, datagram)
                .await?;

        // Store the outbound channel; the frame pump lives inside udp_relay.
        self.udp_proxy_flows
            .flows
            .lock()
            .await
            .insert(*key, flow.outbound);

        // TUN-side inbound pump: tunnel datagrams → UDP packets → TUN device.
        let device = Arc::clone(&self.device);
        let udp_proxy_flows = self.udp_proxy_flows.clone();
        let key_clone = *key;
        let src_ip = key.src_ip;
        let src_port = key.src_port;
        let mut inbound = flow.inbound;

        tokio::spawn(async move {
            while let Some(data) = inbound.recv().await {
                let pkt = match build_udp_packet(dst_ip, dst_port, src_ip, src_port, &data) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let mut dev = device.lock().await;
                if dev.write_packet(&pkt).await.is_err() {
                    break;
                }
            }
            udp_proxy_flows.flows.lock().await.remove(&key_clone);
        });

        Ok(())
    }

    async fn send_tcp_syn_ack(&self, state: &TcpFlowState) -> Result<()> {
        let mut pkt = Vec::with_capacity(128);
        match (state.dst_ip, state.src_ip) {
            (IpAddr::V4(dst), IpAddr::V4(src)) => {
                etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                    .tcp(state.dst_port, state.src_port, state.seq, 65535)
                    .syn()
                    .ack(state.ack)
                    .write(&mut pkt, &[])
                    .map_err(|e| {
                        PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
                    })?;
            }
            (IpAddr::V6(dst), IpAddr::V6(src)) => {
                etherparse::PacketBuilder::ipv6(dst.octets(), src.octets(), 64)
                    .tcp(state.dst_port, state.src_port, state.seq, 65535)
                    .syn()
                    .ack(state.ack)
                    .write(&mut pkt, &[])
                    .map_err(|e| {
                        PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
                    })?;
            }
            _ => return Ok(()),
        }
        let mut dev = self.device.lock().await;
        dev.write_packet(&pkt).await
    }

    async fn send_tcp_ack(&self, state: &TcpFlowState, ack: u32) -> Result<()> {
        let mut pkt = Vec::with_capacity(128);
        match (state.dst_ip, state.src_ip) {
            (IpAddr::V4(dst), IpAddr::V4(src)) => {
                etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                    .tcp(state.dst_port, state.src_port, state.seq, 65535)
                    .ack(ack)
                    .write(&mut pkt, &[])
                    .map_err(|e| {
                        PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
                    })?;
            }
            (IpAddr::V6(dst), IpAddr::V6(src)) => {
                etherparse::PacketBuilder::ipv6(dst.octets(), src.octets(), 64)
                    .tcp(state.dst_port, state.src_port, state.seq, 65535)
                    .ack(ack)
                    .write(&mut pkt, &[])
                    .map_err(|e| {
                        PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
                    })?;
            }
            _ => return Ok(()),
        }
        let mut dev = self.device.lock().await;
        dev.write_packet(&pkt).await
    }

    async fn send_tcp_rst(
        &self,
        _key: FlowKey,
        src_ip: IpAddr,
        dst_ip: IpAddr,
        src_port: u16,
        dst_port: u16,
        _client_seq: u32,
    ) -> Result<()> {
        let mut pkt = Vec::with_capacity(128);
        match (dst_ip, src_ip) {
            (IpAddr::V4(dst), IpAddr::V4(src)) => {
                etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                    .tcp(dst_port, src_port, 0, 0)
                    .rst()
                    .ack(0)
                    .write(&mut pkt, &[])
                    .map_err(|e| {
                        PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
                    })?;
            }
            (IpAddr::V6(dst), IpAddr::V6(src)) => {
                etherparse::PacketBuilder::ipv6(dst.octets(), src.octets(), 64)
                    .tcp(dst_port, src_port, 0, 0)
                    .rst()
                    .ack(0)
                    .write(&mut pkt, &[])
                    .map_err(|e| {
                        PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
                    })?;
            }
            _ => return Ok(()),
        }
        let mut dev = self.device.lock().await;
        dev.write_packet(&pkt).await
    }
}

async fn tcp_relay_task(
    mut rx_from_tun: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
    device: Arc<Mutex<TunDevice>>,
    flows: FlowTable,
    key: FlowKey,
    state: Arc<Mutex<TcpFlowState>>,
    socks5_addr: SocketAddr,
    dst_ip: IpAddr,
    dst_port: u16,
) -> Result<()> {
    let mut socks5 = TcpStream::connect(socks5_addr)
        .await
        .map_err(PhantomError::Io)?;

    socks5
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .map_err(PhantomError::Io)?;
    let mut resp = [0u8; 2];
    socks5
        .read_exact(&mut resp)
        .await
        .map_err(PhantomError::Io)?;
    if resp[0] != 0x05 || resp[1] != 0x00 {
        return Err(PhantomError::Protocol("SOCKS5 auth failed".into()));
    }

    let mut req = match dst_ip {
        IpAddr::V4(ip) => {
            let mut r = vec![0x05, 0x01, 0x00, 0x01];
            r.extend_from_slice(&ip.octets());
            r
        }
        IpAddr::V6(ip) => {
            let mut r = vec![0x05, 0x01, 0x00, 0x04];
            r.extend_from_slice(&ip.octets());
            r
        }
    };
    req.extend_from_slice(&dst_port.to_be_bytes());
    socks5.write_all(&req).await.map_err(PhantomError::Io)?;

    let mut reply = [0u8; 10];
    socks5
        .read_exact(&mut reply)
        .await
        .map_err(PhantomError::Io)?;
    if reply[1] != 0x00 {
        return Err(PhantomError::Protocol(format!(
            "SOCKS5 connect failed: 0x{:02x}",
            reply[1]
        )));
    }

    let (mut s5_read, mut s5_write) = socks5.split();

    let to_socks5 = async {
        while let Some(data) = rx_from_tun.recv().await {
            if data.is_empty() {
                break;
            }
            if let Err(e) = s5_write.write_all(&data).await {
                tracing::debug!("Write to SOCKS5 failed: {}", e);
                break;
            }
        }
        let _ = s5_write.shutdown().await;
        Ok::<_, PhantomError>(())
    };

    let from_socks5 = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = match s5_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!("Read from SOCKS5 failed: {}", e);
                    break;
                }
            };

            let st = state.lock().await;
            let pkt = match build_tcp_psh_packet(&st, &buf[..n]) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!("Packet build error: {}", e);
                    continue;
                }
            };

            {
                let mut dev = device.lock().await;
                if let Err(e) = dev.write_packet(&pkt).await {
                    tracing::debug!("TUN write error: {}", e);
                    break;
                }
            }
        }
        let st = state.lock().await;
        let pkt = match build_tcp_fin_packet(&st) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("FIN packet build error: {}", e);
                return Ok::<_, PhantomError>(());
            }
        };
        let mut dev = device.lock().await;
        let _ = dev.write_packet(&pkt).await;
        Ok::<_, PhantomError>(())
    };

    tokio::try_join!(to_socks5, from_socks5)?;
    flows.remove(&key).await;
    Ok(())
}

async fn tcp_direct_relay_task(
    mut rx_from_tun: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
    device: Arc<Mutex<TunDevice>>,
    flows: FlowTable,
    key: FlowKey,
    state: Arc<Mutex<TcpFlowState>>,
    dst_ip: IpAddr,
    dst_port: u16,
) -> Result<()> {
    let mut target = TcpStream::connect(SocketAddr::new(dst_ip, dst_port))
        .await
        .map_err(PhantomError::Io)?;

    let (mut target_read, mut target_write) = target.split();

    let to_target = async {
        while let Some(data) = rx_from_tun.recv().await {
            if data.is_empty() {
                break;
            }
            if let Err(e) = target_write.write_all(&data).await {
                tracing::debug!("Write to direct target failed: {}", e);
                break;
            }
        }
        let _ = target_write.shutdown().await;
        Ok::<_, PhantomError>(())
    };

    let from_target = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = match target_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!("Read from direct target failed: {}", e);
                    break;
                }
            };

            let st = state.lock().await;
            let pkt = match build_tcp_psh_packet(&st, &buf[..n]) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!("Packet build error: {}", e);
                    continue;
                }
            };

            {
                let mut dev = device.lock().await;
                if let Err(e) = dev.write_packet(&pkt).await {
                    tracing::debug!("TUN write error: {}", e);
                    break;
                }
            }
        }
        let st = state.lock().await;
        let pkt = match build_tcp_fin_packet(&st) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("FIN packet build error: {}", e);
                return Ok::<_, PhantomError>(());
            }
        };
        let mut dev = device.lock().await;
        let _ = dev.write_packet(&pkt).await;
        Ok::<_, PhantomError>(())
    };

    tokio::try_join!(to_target, from_target)?;
    flows.remove(&key).await;
    Ok(())
}

/// Build a TCP PSH+ACK packet for either IPv4 or IPv6.
fn build_tcp_psh_packet(state: &TcpFlowState, payload: &[u8]) -> Result<Vec<u8>> {
    let mut pkt = Vec::with_capacity(128 + payload.len());
    match (state.dst_ip, state.src_ip) {
        (IpAddr::V4(dst), IpAddr::V4(src)) => {
            etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, state.seq, 65535)
                .ack(state.ack)
                .psh()
                .write(&mut pkt, payload)
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        (IpAddr::V6(dst), IpAddr::V6(src)) => {
            etherparse::PacketBuilder::ipv6(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, state.seq, 65535)
                .ack(state.ack)
                .psh()
                .write(&mut pkt, payload)
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        _ => {
            return Err(PhantomError::Protocol(
                "IP version mismatch in TCP packet".to_string(),
            ));
        }
    }
    Ok(pkt)
}

/// Build a TCP FIN+ACK packet for either IPv4 or IPv6.
fn build_tcp_fin_packet(state: &TcpFlowState) -> Result<Vec<u8>> {
    let mut pkt = Vec::with_capacity(128);
    match (state.dst_ip, state.src_ip) {
        (IpAddr::V4(dst), IpAddr::V4(src)) => {
            etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, state.seq, 65535)
                .ack(state.ack)
                .fin()
                .write(&mut pkt, &[])
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        (IpAddr::V6(dst), IpAddr::V6(src)) => {
            etherparse::PacketBuilder::ipv6(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, state.seq, 65535)
                .ack(state.ack)
                .fin()
                .write(&mut pkt, &[])
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        _ => {
            return Err(PhantomError::Protocol(
                "IP version mismatch in TCP packet".to_string(),
            ));
        }
    }
    Ok(pkt)
}

/// Build a raw IPv4/IPv6 + UDP packet swapping src/dst for the response path.
fn build_udp_packet(
    src_ip: IpAddr,
    src_port: u16,
    dst_ip: IpAddr,
    dst_port: u16,
    payload: &[u8],
) -> Result<Vec<u8>> {
    let mut pkt = Vec::with_capacity(128 + payload.len());
    match (src_ip, dst_ip) {
        (IpAddr::V4(src), IpAddr::V4(dst)) => {
            etherparse::PacketBuilder::ipv4(src.octets(), dst.octets(), 64)
                .udp(src_port, dst_port)
                .write(&mut pkt, payload)
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        (IpAddr::V6(src), IpAddr::V6(dst)) => {
            etherparse::PacketBuilder::ipv6(src.octets(), dst.octets(), 64)
                .udp(src_port, dst_port)
                .write(&mut pkt, payload)
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        _ => {
            return Err(PhantomError::Protocol(
                "IP version mismatch in UDP packet".to_string(),
            ));
        }
    }
    Ok(pkt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::{
        CipherPreference, ClientRule, ClientSettings, RulePattern, RulesConfig, TransportProtocol,
    };

    fn hot_state(mode: ProxyMode) -> Arc<Mutex<HotReloadState>> {
        Arc::new(Mutex::new(HotReloadState {
            proxy_mode: mode,
            rule_engine: None,
            server: None,
        }))
    }

    fn server(name: &str) -> ServerEntry {
        ServerEntry {
            name: name.to_string(),
            address: "127.0.0.1:443".to_string(),
            public_key: "dGVzdA==".to_string(),
            psk: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
            cipher: CipherPreference::Auto,
            protocol: TransportProtocol::Tcp,
        }
    }

    fn config(mode: ProxyMode, dns: &str, rules: RulesConfig) -> phantom_core::ClientConfig {
        phantom_core::ClientConfig {
            servers: vec![server("primary")],
            client: ClientSettings {
                mode,
                dns: dns.to_string(),
                ..ClientSettings::default()
            },
            rules,
            ..phantom_core::ClientConfig::default()
        }
    }

    fn rules_with(pattern: RulePattern) -> RulesConfig {
        RulesConfig {
            rules: vec![ClientRule {
                pattern,
                action: RuleAction::Direct,
            }],
            final_action: RuleAction::Proxy,
        }
    }

    /// The `TunProxy` builders are invoked from async contexts, where
    /// `Mutex::blocking_lock` panics. This guards the `try_lock` invariant that
    /// `hot_mut` relies on.
    #[tokio::test]
    async fn hot_state_is_lockable_from_an_async_context() {
        let hot = hot_state(ProxyMode::Smart);
        assert!(
            hot.try_lock().is_ok(),
            "builders must not need to block on the hot-reload lock"
        );
    }

    #[tokio::test]
    async fn apply_reload_updates_mode_rules_and_server() {
        let hot = hot_state(ProxyMode::Direct);
        let cfg = config(
            ProxyMode::Smart,
            "1.1.1.1:53",
            rules_with(RulePattern::IpCidr {
                value: "10.0.0.0/8".to_string(),
            }),
        );

        apply_reload(&hot, None, None, &cfg).await;

        let state = hot.lock().await;
        assert_eq!(state.proxy_mode, ProxyMode::Smart);
        assert!(state.rule_engine.is_some());
        assert_eq!(state.server.as_ref().unwrap().name, "primary");
    }

    /// A malformed rule set must not silently downgrade routing to "proxy
    /// everything"; the previously loaded engine stays in place.
    #[tokio::test]
    async fn apply_reload_keeps_previous_rules_when_new_set_is_invalid() {
        let hot = hot_state(ProxyMode::Smart);
        let good = config(
            ProxyMode::Smart,
            "1.1.1.1:53",
            rules_with(RulePattern::IpCidr {
                value: "10.0.0.0/8".to_string(),
            }),
        );
        apply_reload(&hot, None, None, &good).await;
        let engine_before = hot.lock().await.rule_engine.clone().unwrap();

        let bad = config(
            ProxyMode::Smart,
            "1.1.1.1:53",
            rules_with(RulePattern::IpCidr {
                value: "not-a-cidr".to_string(),
            }),
        );
        apply_reload(&hot, None, None, &bad).await;

        let engine_after = hot.lock().await.rule_engine.clone().unwrap();
        assert!(
            Arc::ptr_eq(&engine_before, &engine_after),
            "invalid rules must leave the previous engine untouched"
        );
    }

    #[tokio::test]
    async fn apply_reload_retargets_the_dns_upstream() {
        let hot = hot_state(ProxyMode::Smart);
        let dns = DnsProxy::new("8.8.8.8:53".parse().unwrap()).await.unwrap();
        let cfg = config(ProxyMode::Smart, "tls://1.1.1.1:853", RulesConfig::default());

        apply_reload(&hot, Some(&dns), None, &cfg).await;

        assert_eq!(dns.upstream(), "1.1.1.1:853".parse().unwrap());
    }

    /// An unparseable `client.dns` must not drop DNS hijacking on the floor.
    #[tokio::test]
    async fn apply_reload_keeps_dns_upstream_when_new_value_is_invalid() {
        let hot = hot_state(ProxyMode::Smart);
        let dns = DnsProxy::new("8.8.8.8:53".parse().unwrap()).await.unwrap();
        let cfg = config(ProxyMode::Smart, "not-an-address", RulesConfig::default());

        apply_reload(&hot, Some(&dns), None, &cfg).await;

        assert_eq!(dns.upstream(), "8.8.8.8:53".parse().unwrap());
    }

    #[tokio::test]
    async fn apply_reload_swaps_the_failover_pool() {
        let hot = hot_state(ProxyMode::Smart);
        let initial = config(ProxyMode::Smart, "1.1.1.1:53", RulesConfig::default());
        let failover = FailoverManager::new(&initial).unwrap();

        let mut next = initial.clone();
        next.servers = vec![server("backup")];
        apply_reload(&hot, None, Some(&failover), &next).await;

        assert_eq!(failover.select_server().unwrap().name, "backup");
        assert_eq!(hot.lock().await.server.as_ref().unwrap().name, "backup");
    }

    #[tokio::test]
    async fn apply_reload_tolerates_missing_optional_components() {
        // The SOCKS5-only path has neither a DNS proxy nor a failover manager
        // wired in; reloading must still update the mode.
        let hot = hot_state(ProxyMode::Direct);
        let cfg = config(ProxyMode::Proxy, "1.1.1.1:53", RulesConfig::default());
        apply_reload(&hot, None, None, &cfg).await;
        assert_eq!(hot.lock().await.proxy_mode, ProxyMode::Proxy);
    }

    #[test]
    fn default_tun_settings_match_the_platform_convention() {
        let settings = TunSettings::default();
        assert_eq!(settings.mtu, TUN_MTU as u16);
        assert_eq!(settings.address, std::net::Ipv4Addr::new(10, 7, 0, 1));
        assert_eq!(settings.netmask, std::net::Ipv4Addr::new(255, 255, 255, 0));
        // macOS only accepts `utun<N>`; Linux has no such constraint.
        if cfg!(target_os = "macos") {
            assert!(settings.name.starts_with("utun"));
        } else {
            assert_eq!(settings.name, "phantom0");
        }
    }
}
