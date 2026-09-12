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
    DnsCache, DnsProxy, DnsQueryContext, DnsRoute, build_dns_response_packet,
    build_refused_response, extract_a_records, extract_query_domain, parse_dns_addr,
};
use crate::failover::FailoverManager;
use crate::rules::RuleEngine;
use crate::stats::TrafficStats;

const TUN_MTU: usize = 1500;

/// Maximum TCP payload we put in one segment. Comfortably below the TUN MTU
/// once the IP+TCP headers are added.
const TCP_MSS: usize = 1400;

/// Stop reading from the tunnel when this much unacknowledged payload is
/// queued, and resume as soon as an ACK frees space again.
const SEND_HIGH_WATER: usize = 512 * 1024;

/// How long the retransmission supervisor waits between ticks.
const RETRANSMIT_TICK: std::time::Duration = std::time::Duration::from_millis(500);

/// Consecutive *unproductive* retransmissions tolerated before a flow is
/// dropped. A retransmission only counts when the app acknowledged nothing
/// since the previous one, so a long download that keeps making progress is
/// never mistaken for a wedged flow (which is exactly what used to happen:
/// 20 ticks × 500 ms = the 10 s stall Google apps showed before retrying).
const MAX_STALLED_RETRANSMITS: u32 = 12;

/// Upper bound on the retransmission back-off, so a stalled flow is still
/// probed often enough to recover quickly when the app's window reopens.
const MAX_RTO: std::time::Duration = std::time::Duration::from_secs(4);

/// Duplicate ACKs needed before the queue is rewound (RFC 5681 fast retransmit).
const DUP_ACK_THRESHOLD: u32 = 3;

/// How long a direct connection may take before it is retried through the
/// tunnel. Censored addresses are blackholed rather than refused, so "connect
/// failed" only shows up as a timeout; 2.5 s keeps the retry well inside the
/// app's own connect timeout while staying clear of ordinary domestic RTTs.
const DIRECT_FALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2500);

/// Bumped whenever the OS tells us the underlying network changed.
///
/// Every flow records the epoch it was born in. When the epoch moves, sockets
/// bound to the old source address are dead on arrival (the phone's IP changed
/// with the network), so flows are torn down immediately and the apps get a
/// reset to retry on the new link instead of hanging until their own timeout.
static NETWORK_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Invalidate every flow and report the new epoch.
pub fn bump_network_epoch() -> u64 {
    NETWORK_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

/// Current network epoch (0 until the first change is reported).
pub fn network_epoch() -> u64 {
    NETWORK_EPOCH.load(std::sync::atomic::Ordering::SeqCst)
}

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

/// Build the initial send-side state for a flow whose SYN-ACK we are about to
/// emit. The SYN consumes one sequence number, hence `isn + 1`.
fn new_flow_state(
    src_ip: IpAddr,
    dst_ip: IpAddr,
    src_port: u16,
    dst_port: u16,
    client_seq: u32,
) -> TcpFlowState {
    let isn = 1000u32;
    TcpFlowState {
        seq: isn.wrapping_add(1),
        snd_una: isn.wrapping_add(1),
        ack: client_seq.wrapping_add(1),
        send_queue: Vec::new(),
        peer_window: 65535,
        fin_queued: false,
        fin_sent: false,
        drain: Arc::new(tokio::sync::Notify::new()),
        dup_acks: 0,
        last_progress_at: std::time::Instant::now(),
        traced_injections: 0,
        bytes_from_app: 0,
        bytes_to_app: 0,
        end_reason: "",
        epoch: network_epoch(),
        src_ip,
        dst_ip,
        src_port,
        dst_port,
    }
}

/// Minimal TCP state for a tun2socks flow.
///
/// The app's stack talks to us as if we were the remote peer, so this has to
/// behave like a (small) TCP sender: sequence numbers must advance, replies
/// must be segmented to the MSS, and unacknowledged payload must be kept for
/// retransmission. The previous version reused one fixed sequence number for
/// every segment and never looked at the app's ACKs, which silently corrupted
/// anything larger than a single segment — every TLS handshake included.
pub struct TcpFlowState {
    /// Next byte we will send (SND.NXT).
    pub seq: u32,
    /// Oldest byte we sent that the app has not acknowledged (SND.UNA).
    pub snd_una: u32,
    /// Next byte we expect from the app (RCV.NXT).
    pub ack: u32,
    /// Payload received from the tunnel but not yet acknowledged by the app.
    /// The first byte always corresponds to `snd_una`.
    pub send_queue: Vec<u8>,
    /// Receive window most recently advertised by the app.
    pub peer_window: u32,
    /// The relay hit EOF; send a FIN once the queue drains.
    pub fin_queued: bool,
    pub fin_sent: bool,
    /// Signalled whenever an ACK frees queue space (relay backpressure).
    pub drain: Arc<tokio::sync::Notify>,
    /// ACKs seen that acknowledged nothing new, with no data in flight
    /// progress — the fast-retransmit trigger.
    dup_acks: u32,
    /// Last `snd_una` value that made progress, used by the retransmit
    /// supervisor to distinguish "slow but alive" from "wedged".
    last_progress_at: std::time::Instant,
    /// Number of segments injected into the app so far (trace cap).
    traced_injections: u32,
    /// Payload bytes the app sent us / we delivered to the app.
    bytes_from_app: u64,
    bytes_to_app: u64,
    /// Why the flow ended, for the trace summary line.
    end_reason: &'static str,
    /// Network epoch this flow was created in (see [`network_epoch`]); a flow
    /// whose epoch is stale cannot survive a link change.
    epoch: u64,
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
    /// Proxy whitelist: built-in censored-domain FST + user entries. Smart mode
    /// tunnels whitelisted destinations and sends everything else direct.
    whitelist: Option<Arc<crate::whitelist::ProxyWhitelist>>,
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
        match parse_dns_addr(&cfg.client.dns_direct) {
            Some(addr) => {
                dns.set_direct_upstream(addr);
            }
            None => tracing::warn!(
                "Config reload: invalid client.dns_direct '{}', keeping direct resolver {}",
                cfg.client.dns_direct,
                dns.direct_upstream()
            ),
        }
    }

    if let Some(failover) = failover {
        failover.reload(cfg);
    }
}

/// Write an upstream DNS answer back into the TUN and feed the IP -> domain
/// cache that the TCP path uses to match the proxy whitelist.
///
/// Shared by both transports: direct answers arrive on the local resolver
/// socket, tunnelled answers arrive on the shared UDP-over-tunnel flow.
async fn deliver_dns_response(
    payload: Bytes,
    ctx: DnsQueryContext,
    domain: Option<String>,
    route: DnsRoute,
    cache: DnsCache,
    device: Arc<Mutex<TunDevice>>,
) -> Result<()> {
    if let Some(ref domain) = domain {
        let ips = extract_a_records(&payload);
        for ip in &ips {
            cache.insert(*ip, domain.clone()).await;
        }
        if !ips.is_empty() {
            let joined = ips
                .iter()
                .map(|ip| ip.to_string())
                .collect::<Vec<_>>()
                .join(",");
            // Same shape as the TCP route lines ("route <target> -> <action>")
            // so the UI's "show direct traffic" switch filters domestic DNS
            // noise with the very same rule, and so a reader can tell at a
            // glance whether a domain was resolved through the tunnel.
            tracing::info!(
                "route {}:53 -> {} (dns {}) {}",
                domain,
                match route {
                    DnsRoute::Tunnel => "Proxy",
                    DnsRoute::Local => "Direct",
                },
                route.as_str(),
                joined
            );
        }
    }
    let pkt = build_dns_response_packet(&payload, &ctx)?;
    let mut dev = device.lock().await;
    dev.write_packet(&pkt).await?;
    Ok(())
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
                whitelist: None,
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

    /// Attach the proxy whitelist used by Smart-mode routing.
    pub fn with_whitelist(self, whitelist: Arc<crate::whitelist::ProxyWhitelist>) -> Self {
        self.hot_mut().whitelist = Some(whitelist);
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

    /// Attach the DNS router. Takes an `Arc` because the platform layers keep
    /// a second handle to it (network-change handling drops the shared tunnel
    /// flow without restarting the tunnel).
    pub fn with_dns(mut self, proxy: Arc<DnsProxy>) -> Self {
        self.dns_proxy = Some(proxy);
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

    /// Decide where a UDP:53 query is resolved and get it on its way.
    ///
    /// Smart mode (and the platform default) sends whitelisted/censored
    /// domains through the tunnel and everything else to the direct resolver,
    /// so domestic names keep resolving to domestic nodes. Proxy mode tunnels
    /// every query; Direct mode resolves everything locally.
    async fn handle_dns_query(
        &self,
        data: &[u8],
        src_ip: IpAddr,
        src_port: u16,
        dst_ip: IpAddr,
        dst_port: u16,
    ) -> Result<()> {
        let dns = match &self.dns_proxy {
            Some(dns) => Arc::clone(dns),
            None => return Ok(()),
        };

        let domain = extract_query_domain(data).map(|(domain, _)| domain);
        let hot = self.hot.lock().await;
        let proxy_mode = hot.proxy_mode;
        let rule_engine = hot.rule_engine.clone();
        let whitelist = hot.whitelist.clone();
        drop(hot);

        let decision = crate::whitelist::decide(
            proxy_mode,
            rule_engine.as_deref(),
            whitelist.as_deref(),
            domain.as_deref(),
            None,
            dst_port,
        );

        if decision.action == RuleAction::Reject {
            if let Some(refused) = build_refused_response(data) {
                let ctx = DnsQueryContext {
                    src_ip,
                    src_port,
                    dst_ip,
                    dst_port,
                };
                let pkt = build_dns_response_packet(&refused, &ctx)?;
                let mut dev = self.device.lock().await;
                dev.write_packet(&pkt).await?;
            }
            return Ok(());
        }

        let mut route = if decision.action == RuleAction::Proxy {
            DnsRoute::Tunnel
        } else {
            DnsRoute::Local
        };

        let ctx = DnsQueryContext {
            src_ip,
            src_port,
            dst_ip,
            dst_port,
        };
        let id = dns.register(data, ctx, route).await?;

        if route == DnsRoute::Tunnel {
            if !dns.has_tunnel_flow() {
                // The first datagram rides the flow-establishing SYN, so a
                // successful call has already delivered this query.
                if !self.ensure_dns_tunnel(data.to_vec()).await {
                    route = DnsRoute::Local;
                    dns.reroute(id, route).await;
                }
            } else if let Err(e) = dns.send(data, route).await {
                // A flow that died between the check and the send must not
                // black-hole the query.
                tracing::warn!("DNS tunnel send failed ({}); using the direct resolver", e);
                dns.set_tunnel_sender(None);
                route = DnsRoute::Local;
                dns.reroute(id, route).await;
            }
        }

        if route == DnsRoute::Local {
            dns.send(data, route).await?;
        }

        // The answer line ("dns <domain> -> <ips> via <route>") is what the
        // user actually reads; the request line is debug-level detail so a
        // single lookup does not cost two lines in the phone's log pane.
        tracing::debug!(
            "dns query {} -> {} ({})",
            domain.as_deref().unwrap_or("<unknown>"),
            route.as_str(),
            decision.reason.as_str()
        );
        Ok(())
    }

    /// Lazily establish the shared UDP-over-tunnel flow used for tunnelled DNS.
    ///
    /// Returns `true` when a flow is usable afterwards. On success the passed
    /// datagram has already been sent through the new flow (it rides the SYN
    /// frame, so resolving costs no extra round trip).
    async fn ensure_dns_tunnel(&self, first_datagram: Vec<u8>) -> bool {
        let dns = match &self.dns_proxy {
            Some(dns) => Arc::clone(dns),
            None => return false,
        };
        if dns.has_tunnel_flow() {
            return true;
        }

        let resolver = dns.upstream();
        let target = match resolver.ip() {
            IpAddr::V4(v4) => TargetAddr::IPv4(v4.octets(), resolver.port()),
            IpAddr::V6(v6) => TargetAddr::IPv6(v6.octets(), resolver.port()),
        };
        let (server, secret) = {
            let hot = self.hot.lock().await;
            match (hot.server.clone(), self.local_secret) {
                (Some(server), Some(secret)) => (server, secret),
                _ => {
                    tracing::warn!(
                        "DNS: no tunnel server/secret available; falling back to the direct resolver"
                    );
                    return false;
                }
            }
        };

        match crate::udp_relay::establish_udp_flow_tcp(&server, &secret, target, first_datagram)
            .await
        {
            Ok(channels) => {
                dns.set_tunnel_sender(Some(channels.outbound));
                let mut inbound = channels.inbound;
                let dns_task = Arc::clone(&dns);
                let cache = self.dns_cache.clone();
                let device = Arc::clone(&self.device);
                tokio::spawn(async move {
                    let mut on_response = move |payload: Bytes,
                                                ctx: DnsQueryContext,
                                                domain: Option<String>,
                                                route| {
                        let cache = cache.clone();
                        let device = device.clone();
                        async move {
                            deliver_dns_response(payload, ctx, domain, route, cache, device).await
                        }
                    };
                    while let Some(payload) = inbound.recv().await {
                        dns_task
                            .handle_tunnel_response(payload, &mut on_response)
                            .await;
                    }
                    tracing::info!("DNS tunnel flow closed; the next query re-establishes it");
                    dns_task.set_tunnel_sender(None);
                });
                tracing::info!(
                    "DNS tunnel flow established via {} -> {}",
                    server.name,
                    resolver
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    "DNS tunnel flow to {} failed ({}); using the direct resolver",
                    resolver,
                    e
                );
                false
            }
        }
    }

    pub async fn run(&self) -> Result<()> {
        // Spawn the direct-path DNS response handler. Tunnel-path responses are
        // handled by the flow task started lazily in `ensure_dns_tunnel`.
        if let Some(dns) = &self.dns_proxy {
            let dns = Arc::clone(dns);
            let cache = self.dns_cache.clone();
            let device = Arc::clone(&self.device);
            tokio::spawn(async move {
                let mut on_response = move |payload: Bytes,
                                            ctx: DnsQueryContext,
                                            domain: Option<String>,
                                            route| {
                    let cache = cache.clone();
                    let device = device.clone();
                    async move { deliver_dns_response(payload, ctx, domain, route, cache, device).await }
                };
                let _ = dns.run_local(&mut on_response).await;
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
            crate::tun_trace!(
                "SYN {}:{} -> {}:{} seq={} win={} opts=[{}]",
                src_ip,
                src_port,
                dst_ip,
                dst_port,
                tcp.sequence_number(),
                tcp.window_size(),
                tcp_options_summary(&tcp)
            );
            let domain = match dst_ip {
                IpAddr::V4(v4) => self.dns_cache.lookup(v4).await,
                _ => None,
            };
            let hot = self.hot.lock().await;
            let proxy_mode = hot.proxy_mode;
            let rule_engine = hot.rule_engine.clone();
            let whitelist = hot.whitelist.clone();
            drop(hot);
            let decision = crate::whitelist::decide(
                proxy_mode,
                rule_engine.as_deref(),
                whitelist.as_deref(),
                domain.as_deref(),
                Some(dst_ip),
                dst_port,
            );
            let action = decision.action;
            // A "direct" verdict for an IP whose domain we have never seen is a
            // guess: the app may be talking to a censored host that it resolved
            // itself (its own DoH, or a cached answer from a previous session),
            // so our DNS cache is empty and the whitelist cannot match. Those
            // connections are blackholed by the network rather than refused, so
            // let the direct relay retry through the tunnel when it cannot
            // connect. Google Earth/Maps are exactly this shape: they dial
            // Google IPs straight from their own resolver.
            let fallback_to_tunnel = decision.allows_tunnel_fallback(proxy_mode);
            match action {
                RuleAction::Proxy => self.stats.record_route_proxy(),
                RuleAction::Direct => self.stats.record_route_direct(),
                RuleAction::Reject => {}
            }
            // One line per new TCP flow: this is the main breadcrumb for
            // verifying that a domain actually took the tunnel.
            tracing::info!(
                "route {}:{} -> {:?} ({})",
                dst_ip,
                dst_port,
                action,
                decision.reason.as_str()
            );

            match action {
                RuleAction::Direct => {
                    self.spawn_direct_tcp_flow(
                        key,
                        src_ip,
                        dst_ip,
                        src_port,
                        dst_port,
                        tcp.sequence_number(),
                        fallback_to_tunnel,
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
            let mut st = flow.state.lock().await;

            // The app's ACK/window comes first: it may unblock queued payload.
            let mut fast_retransmit = false;
            if ack {
                let advanced =
                    apply_peer_ack(&mut st, tcp.acknowledgment_number(), tcp.window_size());
                if !advanced && data.is_empty() && !fin {
                    st.dup_acks = st.dup_acks.saturating_add(1);
                    if st.dup_acks >= DUP_ACK_THRESHOLD && st.seq != st.snd_una {
                        st.dup_acks = 0;
                        fast_retransmit = true;
                    }
                }
            }
            if fast_retransmit {
                crate::tun_trace!(
                    "3-dup-ACK fast retransmit {}:{} snd_una={} snd_nxt={} queued={} win={}",
                    dst_ip,
                    dst_port,
                    st.snd_una,
                    st.seq,
                    st.send_queue.len(),
                    st.peer_window
                );
                st.seq = st.snd_una;
                flush_send_queue(&mut st, &self.device).await?;
            }

            if fin {
                st.ack = st.ack.wrapping_add(1);
                let ack_pkt = build_tcp_ack_packet(&st)?;
                {
                    let mut dev = self.device.lock().await;
                    let _ = dev.write_packet(&ack_pkt).await;
                }
                let _ = flow.tx_to_relay.send(Bytes::new());
                drop(st);
                self.flows.remove(&key).await;
                return Ok(());
            }

            if !data.is_empty() {
                let seq = tcp.sequence_number();
                let expected = st.ack;
                // Accept in-order bytes only; drop gaps (the ACK below asks for
                // a resend) and trim retransmitted prefixes so the relay never
                // forwards the same bytes into the tunnel twice.
                let chunk: &[u8] = if seq == expected {
                    data
                } else if tcp_seq_before(expected, seq) {
                    &[]
                } else {
                    let overlap = expected.wrapping_sub(seq) as usize;
                    if overlap >= data.len() {
                        &[]
                    } else {
                        &data[overlap..]
                    }
                };
                if !chunk.is_empty() {
                    st.ack = st.ack.wrapping_add(chunk.len() as u32);
                    st.bytes_from_app += chunk.len() as u64;
                    let _ = flow.tx_to_relay.send(Bytes::copy_from_slice(chunk));
                }

                flush_send_queue(&mut st, &self.device).await?;

                let ack_pkt = build_tcp_ack_packet(&st)?;
                {
                    let mut dev = self.device.lock().await;
                    dev.write_packet(&ack_pkt).await?;
                }
            } else if ack {
                // Pure ACK/window update: whatever was blocked may now flow.
                flush_send_queue(&mut st, &self.device).await?;
            }
        }
        Ok(())
    }

    async fn handle_udp(&self, payload: &[u8], src_ip: IpAddr, dst_ip: IpAddr) -> Result<()> {
        let udp = etherparse::UdpHeaderSlice::from_slice(payload)
            .map_err(|e| PhantomError::Protocol(format!("UDP parse: {:?}", e)))?;
        let data = &payload[udp.slice().len()..];
        let src_port = udp.source_port();
        let dst_port = udp.destination_port();

        // DNS hijack. The loop guard keeps the direct resolver socket's own
        // queries out of the hijack, otherwise they would be captured by the
        // very TUN they are trying to bypass.
        if dst_port == 53 {
            if let Some(dns) = &self.dns_proxy {
                if src_port != dns.local_port() {
                    return self
                        .handle_dns_query(data, src_ip, src_port, dst_ip, dst_port)
                        .await;
                }
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
        let whitelist = hot.whitelist.clone();
        drop(hot);
        let decision = crate::whitelist::decide(
            proxy_mode,
            rule_engine.as_deref(),
            whitelist.as_deref(),
            domain.as_deref(),
            Some(dst_ip),
            dst_port,
        );
        let action = decision.action;
        let key = FlowKey {
            src_ip,
            dst_ip,
            src_port,
            dst_port,
            proto: IpNumber::UDP.0,
        };
        // One line per new UDP flow (not per packet) so long-lived flows such as
        // QUIC or gaming traffic do not flood the in-app log.
        let known_flow = match action {
            RuleAction::Proxy => self.udp_proxy_flows.flows.lock().await.contains_key(&key),
            _ => self.udp_flows.flows.lock().await.contains_key(&key),
        };
        if !known_flow {
            tracing::info!(
                "route {}:{} (udp) -> {:?} ({})",
                dst_ip,
                dst_port,
                action,
                decision.reason.as_str()
            );
        }

        match action {
            RuleAction::Direct => {
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
        let state = Arc::new(Mutex::new(new_flow_state(
            src_ip, dst_ip, src_port, dst_port, client_seq,
        )));

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

        self.spawn_retransmit_supervisor(Arc::clone(&state), key);

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
        fallback_to_tunnel: bool,
    ) -> Result<()> {
        let (tx_to_relay, rx_from_tun) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
        let state = Arc::new(Mutex::new(new_flow_state(
            src_ip, dst_ip, src_port, dst_port, client_seq,
        )));

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

        self.spawn_retransmit_supervisor(Arc::clone(&state), key);

        let device = Arc::clone(&self.device);
        let flows = self.flows.clone();
        let socks5_addr = self.socks5_addr;
        tokio::spawn(async move {
            if let Err(e) = tcp_direct_relay_task(
                rx_from_tun,
                device,
                flows,
                key,
                state,
                dst_ip,
                dst_port,
                socks5_addr,
                fallback_to_tunnel,
            )
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
            let secret = self
                .local_secret
                .ok_or_else(|| PhantomError::Config("No local secret for UDP proxy".to_string()))?;
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

    /// Go-back-N retransmission for one flow.
    ///
    /// Every unacknowledged byte stays in `send_queue`, so a timeout only has
    /// to rewind SND.NXT to SND.UNA and re-send. Duplicates are harmless (the
    /// app drops bytes it already has) and it keeps one lost segment from
    /// wedging a connection — which is exactly what used to happen once we
    /// started emitting more than a single segment per flow.
    ///
    /// The supervisor only counts a timeout as *stalled* when the app has
    /// acknowledged nothing since the previous one. Counting raw ticks instead
    /// killed healthy long downloads after 10 s (20 ticks), because a busy flow
    /// always has unacknowledged bytes in flight.
    fn spawn_retransmit_supervisor(&self, state: Arc<Mutex<TcpFlowState>>, key: FlowKey) {
        let device = Arc::clone(&self.device);
        let flows = self.flows.clone();
        tokio::spawn(async move {
            let mut stalled = 0u32;
            let mut rto = RETRANSMIT_TICK;
            let mut last_check = std::time::Instant::now();
            loop {
                tokio::time::sleep(rto).await;
                if flows.get(&key).await.is_none() {
                    return;
                }
                let mut st = state.lock().await;
                if st.epoch != network_epoch() {
                    st.end_reason = "network changed";
                    let rst = build_tcp_rst_packet(&st).ok();
                    drop(st);
                    if let Some(pkt) = rst {
                        let mut dev = device.lock().await;
                        let _ = dev.write_packet(&pkt).await;
                    }
                    crate::tun_trace!(
                        "flow {}:{} retired: network changed",
                        key.dst_ip,
                        key.dst_port
                    );
                    flows.remove(&key).await;
                    return;
                }
                let unacked = st.seq.wrapping_sub(st.snd_una);
                let now = std::time::Instant::now();
                if unacked == 0 {
                    stalled = 0;
                    rto = RETRANSMIT_TICK;
                    last_check = now;
                    if st.fin_queued && !st.fin_sent {
                        let _ = flush_send_queue(&mut st, &device).await;
                    }
                    continue;
                }
                // Any acknowledgement since the previous tick means the flow is
                // alive; restart the budget instead of counting down to a kill.
                if st.last_progress_at > last_check {
                    stalled = 0;
                    rto = RETRANSMIT_TICK;
                }
                last_check = now;
                stalled += 1;
                if stalled > MAX_STALLED_RETRANSMITS {
                    st.end_reason = "no ack";
                    drop(st);
                    crate::tun_trace!(
                        "flow {}:{} dropped after {} stalled retransmits",
                        key.dst_ip,
                        key.dst_port,
                        stalled
                    );
                    tracing::debug!(
                        "TCP flow {}:{} stopped acknowledging; dropping",
                        key.dst_ip,
                        key.dst_port
                    );
                    flows.remove(&key).await;
                    return;
                }
                if st.peer_window == 0 {
                    // Zero window: probe with a single byte at SND.UNA so the
                    // app re-advertises as soon as its buffer drains
                    // (RFC 1122 §4.2.2.17). Without this a flow whose window
                    // closes waits for the app to speak first, which it never
                    // does while it still believes it is being served.
                    let saved = st.peer_window;
                    st.peer_window = 1;
                    st.seq = st.snd_una;
                    crate::tun_trace!(
                        "zero-window probe {}:{} queued={}",
                        key.dst_ip,
                        key.dst_port,
                        st.send_queue.len()
                    );
                    let _ = flush_send_queue(&mut st, &device).await;
                    st.peer_window = saved;
                    st.seq = st.snd_una;
                } else {
                    st.seq = st.snd_una;
                    let _ = flush_send_queue(&mut st, &device).await;
                }
                rto = (rto * 2).min(MAX_RTO);
            }
        });
    }

    async fn send_tcp_syn_ack(&self, state: &TcpFlowState) -> Result<()> {
        let mut pkt = Vec::with_capacity(128);
        // The SYN itself occupies the sequence number *before* SND.NXT.
        let syn_seq = state.seq.wrapping_sub(1);
        match (state.dst_ip, state.src_ip) {
            (IpAddr::V4(dst), IpAddr::V4(src)) => {
                etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                    .tcp(state.dst_port, state.src_port, syn_seq, 65535)
                    .syn()
                    .ack(state.ack)
                    .write(&mut pkt, &[])
                    .map_err(|e| {
                        PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
                    })?;
            }
            (IpAddr::V6(dst), IpAddr::V6(src)) => {
                etherparse::PacketBuilder::ipv6(dst.octets(), src.octets(), 64)
                    .tcp(state.dst_port, state.src_port, syn_seq, 65535)
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
        let r = dev.write_packet(&pkt).await;
        if r.is_ok() {
            crate::tun_trace!(
                "SYN-ACK {}:{} seq={} ack={} win={} opts=[mss={}]",
                state.dst_ip,
                state.dst_port,
                syn_seq,
                state.ack,
                state.peer_window,
                TCP_MSS
            );
        }
        r
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

/// Hand an already-established flow over to the tunnel relay.
///
/// Used when a direct connection turned out to be unreachable. The app still
/// believes it is talking to the destination (its SYN was answered long ago),
/// so the relay just changes upstream: the flow state, the sequence numbers and
/// everything the app already buffered carry over untouched.
async fn retry_through_tunnel(
    rx_from_tun: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
    device: Arc<Mutex<TunDevice>>,
    flows: FlowTable,
    key: FlowKey,
    state: Arc<Mutex<TcpFlowState>>,
    socks5_addr: SocketAddr,
    dst_ip: IpAddr,
    dst_port: u16,
) -> Result<()> {
    crate::tun_trace!(
        "flow {}:{} retried through the tunnel",
        dst_ip,
        dst_port
    );
    tcp_relay_task(
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
    // Loopback hop into the local ingress: it carries the app's segments
    // upstream and the remote's payload downstream, in both cases as small
    // writes whenever the flow is interactive.
    crate::net_tune::tune(&socks5);

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
            // ATYP_PREROUTED: this flow already won a PROXY verdict in the TUN
            // path; the relay must not re-decide it from the IP alone.
            let mut r = vec![0x05, 0x01, 0x00, crate::socks5::ATYP_PREROUTED, 0x01];
            r.extend_from_slice(&ip.octets());
            r
        }
        IpAddr::V6(ip) => {
            let mut r = vec![0x05, 0x01, 0x00, crate::socks5::ATYP_PREROUTED, 0x04];
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
        let mut buf = vec![0u8; 16384];
        loop {
            let n = match s5_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!("Read from SOCKS5 failed: {}", e);
                    break;
                }
            };
            if let Err(e) = queue_tunnel_payload(&state, &device, &buf[..n]).await {
                tracing::debug!("TUN write error: {}", e);
                break;
            }
        }
        // Queue the FIN behind whatever is still unacknowledged; the supervisor
        // and the app's ACKs push it out.
        let mut st = state.lock().await;
        st.fin_queued = true;
        let _ = flush_send_queue(&mut st, &device).await;
        Ok::<_, PhantomError>(())
    };

    tokio::try_join!(to_socks5, from_socks5)?;
    {
        let st = state.lock().await;
        crate::tun_trace!(
            "flow end (tunnel) {}:{} up={} down={} queued={} reason={}",
            key.dst_ip,
            key.dst_port,
            st.bytes_from_app,
            st.bytes_to_app,
            st.send_queue.len(),
            if st.end_reason.is_empty() { "relay done" } else { st.end_reason }
        );
    }
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
    socks5_addr: SocketAddr,
    fallback_to_tunnel: bool,
) -> Result<()> {
    // Nothing has been read from the tunnel yet, so the app's buffered payload
    // is still in `rx_from_tun` and can be handed to the tunnel relay verbatim
    // if the direct connect fails.
    let connect = tokio::time::timeout(
        DIRECT_FALLBACK_TIMEOUT,
        TcpStream::connect(SocketAddr::new(dst_ip, dst_port)),
    )
    .await;
    let mut target = match connect {
        Ok(Ok(stream)) => {
            // Direct destinations ride a real RTT (10–40 ms locally, more to
            // overseas CDNs), which is exactly the regime where Nagle costs a
            // whole extra round trip on a request/response exchange.
            crate::net_tune::tune(&stream);
            stream
        }
        Ok(Err(e)) if fallback_to_tunnel => {
            tracing::info!(
                "route {}:{} -> Proxy (direct connect failed: {}; retrying through the tunnel)",
                dst_ip,
                dst_port,
                e
            );
            return retry_through_tunnel(
                rx_from_tun,
                device,
                flows,
                key,
                state,
                socks5_addr,
                dst_ip,
                dst_port,
            )
            .await;
        }
        Err(_) if fallback_to_tunnel => {
            tracing::info!(
                "route {}:{} -> Proxy (direct connect timed out; retrying through the tunnel)",
                dst_ip,
                dst_port
            );
            return retry_through_tunnel(
                rx_from_tun,
                device,
                flows,
                key,
                state,
                socks5_addr,
                dst_ip,
                dst_port,
            )
            .await;
        }
        Ok(Err(e)) => {
            flows.remove(&key).await;
            return Err(PhantomError::Io(e));
        }
        Err(_) => {
            flows.remove(&key).await;
            return Err(PhantomError::Timeout);
        }
    };

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
        let mut buf = vec![0u8; 16384];
        loop {
            let n = match target_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!("Read from direct target failed: {}", e);
                    break;
                }
            };
            if let Err(e) = queue_tunnel_payload(&state, &device, &buf[..n]).await {
                tracing::debug!("TUN write error: {}", e);
                break;
            }
        }
        let mut st = state.lock().await;
        st.fin_queued = true;
        let _ = flush_send_queue(&mut st, &device).await;
        Ok::<_, PhantomError>(())
    };

    tokio::try_join!(to_target, from_target)?;
    {
        let st = state.lock().await;
        crate::tun_trace!(
            "flow end (direct) {}:{} up={} down={} queued={}",
            key.dst_ip,
            key.dst_port,
            st.bytes_from_app,
            st.bytes_to_app,
            st.send_queue.len()
        );
    }
    flows.remove(&key).await;
    Ok(())
}

/// Build a TCP PSH+ACK packet for either IPv4 or IPv6.
/// Apply the app's ACK (and advertised window) to a flow.
/// Is `a` before `b` in TCP's wrapping sequence space?
fn tcp_seq_before(a: u32, b: u32) -> bool {
    b.wrapping_sub(a) < 0x8000_0000 && a != b
}

/// One-line summary of a TCP option list, for the TUN trace.
///
/// The interesting negotiation details are the peer's MSS (how large our
/// segments may be), its window scale (whether the window it advertises later
/// is scaled) and whether it asked for SACK/timestamps — all three change what
/// a stalled flow looks like.
fn tcp_options_summary(tcp: &etherparse::TcpHeaderSlice<'_>) -> String {
    use etherparse::TcpOptionElement;
    let mut parts: Vec<String> = Vec::new();
    for opt in tcp.options_iterator() {
        match opt {
            Ok(TcpOptionElement::Noop) => {}
            Ok(TcpOptionElement::MaximumSegmentSize(mss)) => parts.push(format!("mss={mss}")),
            Ok(TcpOptionElement::WindowScale(scale)) => parts.push(format!("wscale={scale}")),
            Ok(TcpOptionElement::SelectiveAcknowledgementPermitted) => {
                parts.push("sack=ok".to_string())
            }
            Ok(TcpOptionElement::SelectiveAcknowledgement(_, _)) => {
                parts.push("sack=blk".to_string())
            }
            Ok(TcpOptionElement::Timestamp(_ts, 0)) => parts.push("ts".to_string()),
            Ok(TcpOptionElement::Timestamp(_, _)) => parts.push("ts=echo".to_string()),
            Err(_) => {
                parts.push("opt?".to_string());
                break;
            }
        }
    }
    parts.join(" ")
}

/// Apply the app's ACK (and advertised window) to a flow.
///
/// `send_queue` always starts at `snd_una`, so a valid ACK simply drops that
/// many bytes off the front. Returns `true` when the ACK advanced SND.UNA,
/// which is what the duplicate-ACK counter in the caller keys off.
fn apply_peer_ack(state: &mut TcpFlowState, ack: u32, window: u16) -> bool {
    state.peer_window = window as u32;
    let advanced = ack.wrapping_sub(state.snd_una);
    if advanced == 0 {
        return false;
    }
    let queued = state.send_queue.len() as u32;
    let consume = advanced.min(queued) as usize;
    if consume > 0 {
        state.send_queue.drain(0..consume);
    }
    state.snd_una = state.snd_una.wrapping_add(consume as u32);
    // An ACK may cover the FIN, which occupies one sequence number past the
    // queued payload. Retire it explicitly: leaving SND.UNA a byte behind made
    // the retransmission supervisor treat a completed flow as wedged.
    if (advanced as usize) > consume && state.fin_sent {
        state.snd_una = state.seq;
    }
    state.dup_acks = 0;
    state.last_progress_at = std::time::Instant::now();
    state.drain.notify_waiters();
    true
}

/// Send as much queued payload as the app's receive window allows, segmented to
/// the MSS, then the FIN once everything is out.
async fn flush_send_queue(state: &mut TcpFlowState, device: &Arc<Mutex<TunDevice>>) -> Result<()> {
    let mut bursts = 0;
    loop {
        let in_flight = state.seq.wrapping_sub(state.snd_una);
        // Strictly obey the advertised window. Over-running it (an earlier
        // 8 KiB floor did) makes the app silently drop segments, which then
        // looks exactly like a stalled flow.
        let window = state.peer_window;
        let allowed = window.saturating_sub(in_flight) as usize;
        if allowed == 0 {
            break;
        }
        let offset = state.seq.wrapping_sub(state.snd_una) as usize;
        if offset >= state.send_queue.len() {
            break;
        }
        let n = TCP_MSS.min(allowed).min(state.send_queue.len() - offset);
        let payload = state.send_queue[offset..offset + n].to_vec();
        if state.traced_injections < 8 {
            state.traced_injections += 1;
            crate::tun_trace!(
                "inject {}:{} seq={} len={} snd_una={} snd_nxt={} win={} inflight={} queued={}",
                state.dst_ip,
                state.dst_port,
                state.seq,
                n,
                state.snd_una,
                state.seq,
                window,
                in_flight,
                state.send_queue.len()
            );
        }
        let pkt = build_tcp_psh_packet(state, &payload)?;
        {
            let mut dev = device.lock().await;
            dev.write_packet(&pkt).await?;
        }
        state.seq = state.seq.wrapping_add(n as u32);
        state.bytes_to_app += n as u64;
        bursts += 1;
        if bursts >= 64 {
            // Yield to the runtime on very large bursts; the supervisor and the
            // next ACK pick the rest up.
            break;
        }
    }

    if state.send_queue.is_empty() && state.fin_queued && !state.fin_sent {
        let pkt = build_tcp_fin_packet(state)?;
        {
            let mut dev = device.lock().await;
            dev.write_packet(&pkt).await?;
        }
        state.seq = state.seq.wrapping_add(1);
        state.fin_sent = true;
    }
    Ok(())
}

/// Hand a chunk of tunnelled payload to the app, waiting for queue space when
/// the app has not acknowledged enough yet.
async fn queue_tunnel_payload(
    state: &Arc<Mutex<TcpFlowState>>,
    device: &Arc<Mutex<TunDevice>>,
    data: &[u8],
) -> Result<()> {
    let mut offset = 0;
    while offset < data.len() {
        // `Notify` must outlive the guard that produced it.
        let drain = Arc::clone(&state.lock().await.drain);
        let notified = drain.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        let mut st = state.lock().await;
        if st.send_queue.len() >= SEND_HIGH_WATER {
            drop(st);
            notified.await;
            continue;
        }
        let take = (SEND_HIGH_WATER - st.send_queue.len()).min(data.len() - offset);
        st.send_queue
            .extend_from_slice(&data[offset..offset + take]);
        offset += take;
        flush_send_queue(&mut st, device).await?;
    }
    Ok(())
}

/// Bare ACK carrying the flow's current receive-next and send-next.
fn build_tcp_ack_packet(state: &TcpFlowState) -> Result<Vec<u8>> {
    let mut pkt = Vec::with_capacity(128);
    match (state.dst_ip, state.src_ip) {
        (IpAddr::V4(dst), IpAddr::V4(src)) => {
            etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, state.seq, 65535)
                .ack(state.ack)
                .write(&mut pkt, &[])
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        (IpAddr::V6(dst), IpAddr::V6(src)) => {
            etherparse::PacketBuilder::ipv6(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, state.seq, 65535)
                .ack(state.ack)
                .write(&mut pkt, &[])
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        _ => {
            return Err(PhantomError::Protocol(
                "IP version mismatch in TCP ACK".to_string(),
            ));
        }
    }
    Ok(pkt)
}

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

/// Build a RST+ACK that a real peer would emit, i.e. with sequence numbers the
/// app's stack accepts.
///
/// The `seq=0/ack=0` reset used by the reject path is only a best effort: a
/// synchronised stack validates the sequence number against its RCV.NXT and
/// ignores anything else. Flows torn down for a network change have to be
/// reset *properly*, otherwise the app keeps waiting for data that can never
/// arrive instead of reconnecting on the new link.
fn build_tcp_rst_packet(state: &TcpFlowState) -> Result<Vec<u8>> {
    let mut pkt = Vec::with_capacity(128);
    match (state.dst_ip, state.src_ip) {
        (IpAddr::V4(dst), IpAddr::V4(src)) => {
            etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, state.seq, 65535)
                .rst()
                .ack(state.ack)
                .write(&mut pkt, &[])
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        (IpAddr::V6(dst), IpAddr::V6(src)) => {
            etherparse::PacketBuilder::ipv6(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, state.seq, 65535)
                .rst()
                .ack(state.ack)
                .write(&mut pkt, &[])
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        }
        _ => {
            return Err(PhantomError::Protocol(
                "IP version mismatch in TCP RST".to_string(),
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
            whitelist: None,
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
            builtin_proxy_whitelist: false,
        }
    }

    /// DNS proxy with the production default resolvers (tunnel + direct).
    async fn test_dns_proxy() -> DnsProxy {
        DnsProxy::new(
            "8.8.8.8:53".parse().unwrap(),
            "223.5.5.5:53".parse().unwrap(),
        )
        .await
        .unwrap()
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
        let dns = test_dns_proxy().await;
        let cfg = config(
            ProxyMode::Smart,
            "tls://1.1.1.1:853",
            RulesConfig::default(),
        );

        apply_reload(&hot, Some(&dns), None, &cfg).await;

        assert_eq!(dns.upstream(), "1.1.1.1:853".parse().unwrap());
    }

    /// `client.dns_direct` is retargeted independently of `client.dns`.
    #[tokio::test]
    async fn apply_reload_retargets_the_direct_dns_upstream() {
        let hot = hot_state(ProxyMode::Smart);
        let dns = test_dns_proxy().await;
        let mut cfg = config(ProxyMode::Smart, "8.8.8.8:53", RulesConfig::default());
        cfg.client.dns_direct = "119.29.29.29:53".to_string();

        apply_reload(&hot, Some(&dns), None, &cfg).await;

        assert_eq!(dns.direct_upstream(), "119.29.29.29:53".parse().unwrap());
        assert_eq!(dns.upstream(), "8.8.8.8:53".parse().unwrap());
    }

    /// An unparseable `client.dns` must not drop DNS hijacking on the floor.
    #[tokio::test]
    async fn apply_reload_keeps_dns_upstream_when_new_value_is_invalid() {
        let hot = hot_state(ProxyMode::Smart);
        let dns = test_dns_proxy().await;
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
