//! TUN-based transparent proxy (tun2socks-lite) with Smart routing.
//!
//! Replaces SOCKS5 as the client entry-point for Android/macOS native apps.
//! Supports:
//! - TCP flow relay via local SOCKS5 proxy (Proxy mode)
//! - TCP direct connection (Direct mode, bypass tunnel)
//! - DNS hijack (UDP:53 intercepted and forwarded to upstream DNS)
//! - Rule-based routing (Smart mode)

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::{Bytes, BytesMut};
use etherparse::IpNumber;
use phantom_core::crypto::cipher::CipherSuite;
use phantom_core::crypto::session::CipherOffer;
use phantom_core::crypto::{NoiseInitiator, split_after_handshake};
use phantom_core::protocol::codec::{FrameReader, FrameWriter};
use phantom_core::protocol::frame::FrameFlags;
use phantom_core::protocol::{Frame, TargetAddr};
use phantom_core::transport::Transport;
use phantom_core::transport::tcp::TcpTransport;
use phantom_core::{CipherPreference, PhantomError, ProxyMode, Result, RuleAction, ServerEntry};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::dns::{
    DnsCache, DnsProxy, DnsQueryContext, build_dns_response_packet, extract_a_records,
    extract_query_domain,
};
use crate::rules::RuleEngine;
use crate::stats::TrafficStats;

const TUN_MTU: usize = 1500;

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
        let mut config = tun::Configuration::default();
        config
            .tun_name("utun7")
            .address((10, 7, 0, 1))
            .netmask((255, 255, 255, 0))
            .mtu(TUN_MTU as u16)
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
}

/// Main TUN transparent proxy.
pub struct TunProxy {
    device: Arc<Mutex<TunDevice>>,
    flows: FlowTable,
    udp_flows: UdpFlowTable,
    udp_proxy_flows: UdpProxyFlowTable,
    socks5_addr: SocketAddr,
    hot: Arc<Mutex<HotReloadState>>,
    server: Option<ServerEntry>,
    local_secret: Option<[u8; 32]>,
    dns_proxy: Option<Arc<DnsProxy>>,
    dns_cache: DnsCache,
    config_path: Option<String>,
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
            })),
            server: None,
            local_secret: None,
            dns_proxy: None,
            dns_cache: DnsCache::new(),
            config_path: None,
            stats: TrafficStats::new(),
        }
    }

    pub fn with_mode(self, mode: ProxyMode) -> Self {
        self.hot.blocking_lock().proxy_mode = mode;
        self
    }

    pub fn with_server(mut self, server: ServerEntry, secret: [u8; 32]) -> Self {
        self.server = Some(server);
        self.local_secret = Some(secret);
        self
    }

    pub fn with_rules(self, engine: RuleEngine) -> Self {
        self.hot.blocking_lock().rule_engine = Some(Arc::new(engine));
        self
    }

    pub fn with_config_path(mut self, path: String) -> Self {
        self.config_path = Some(path);
        self
    }

    pub fn with_dns(mut self, proxy: DnsProxy) -> Self {
        self.dns_proxy = Some(Arc::new(proxy));
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
                    if mtime > last_mtime {
                        last_mtime = mtime;
                        match std::fs::read_to_string(&path) {
                            Ok(content) => {
                                match toml::from_str::<phantom_core::ClientConfig>(&content) {
                                    Ok(cfg) => {
                                        let mut hot = hot.lock().await;
                                        hot.proxy_mode = cfg.client.mode;
                                        if let Ok(engine) =
                                            crate::rules::RuleEngine::from_config(&cfg.rules)
                                        {
                                            hot.rule_engine = Some(Arc::new(engine));
                                            tracing::info!("Config reloaded: rules updated");
                                        } else {
                                            tracing::warn!("Config reload: rule parse failed");
                                        }
                                    }
                                    Err(e) => tracing::warn!("Config reload parse error: {}", e),
                                }
                            }
                            Err(e) => tracing::warn!("Config reload read error: {}", e),
                        }
                    }
                }
            });
        }

        // Spawn metrics HTTP server.
        {
            let stats = Arc::clone(&self.stats);
            tokio::spawn(async move {
                let listener = match tokio::net::TcpListener::bind("127.0.0.1:9150").await {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::debug!("Metrics server bind failed: {}", e);
                        return;
                    }
                };
                loop {
                    let (mut stream, _) = match listener.accept().await {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let stats = stats.render_prometheus();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n{}",
                        stats.len(),
                        stats
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                }
            });
        }

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
        datagram: Vec<u8>,
    ) -> Result<()> {
        // Try sending to an existing flow.
        {
            let map = self.udp_proxy_flows.flows.lock().await;
            if let Some(tx) = map.get(key) {
                let _ = tx.send(datagram);
                return Ok(());
            }
        }

        // Need server info to establish a direct tunnel.
        let server = self.server.clone().ok_or_else(|| {
            PhantomError::Config("No server configured for UDP proxy".to_string())
        })?;
        let local_secret = self
            .local_secret
            .ok_or_else(|| PhantomError::Config("No local secret for UDP proxy".to_string()))?;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

        // Build target address for the UDP SYN payload.
        let target = match dst_ip {
            IpAddr::V4(v4) => TargetAddr::IPv4(v4.octets(), dst_port),
            IpAddr::V6(v6) => TargetAddr::IPv6(v6.octets(), dst_port),
        };
        let mut syn_payload = target.encode().to_vec();
        syn_payload.extend_from_slice(&datagram);

        // Establish Noise tunnel to server.
        let addr: SocketAddr = server
            .address
            .parse()
            .map_err(|e| PhantomError::Config(format!("Invalid server address: {}", e)))?;
        let transport = TcpTransport::new(std::time::Duration::from_secs(10));
        let stream = transport.connect(&addr).await?;

        let remote_public = decode_public_key_for_udp(&server.public_key)?;
        let initiator = NoiseInitiator::new(&local_secret, &remote_public);
        let offer = match server.cipher {
            CipherPreference::Auto => CipherOffer::default_offer(),
            CipherPreference::Aes256Gcm => CipherOffer::new(vec![CipherSuite::Aes256Gcm]),
            CipherPreference::Aes128Gcm => CipherOffer::new(vec![CipherSuite::Aes128Gcm]),
            CipherPreference::Ascon128 => CipherOffer::new(vec![CipherSuite::Ascon128]),
            CipherPreference::ChaCha20Poly1305 => CipherOffer::new(vec![CipherSuite::ChaCha20Poly]),
        };
        let result = initiator.handshake(stream, &offer).await?;
        let (session_reader, session_writer) = split_after_handshake(
            result.stream,
            result.split_keys,
            result.chosen_cipher,
            result.is_initiator,
        );
        let mut frame_reader = FrameReader::new(session_reader);
        let mut frame_writer = FrameWriter::new(session_writer);

        // Send UDP SYN frame.
        let stream_id: u32 = 1;
        let syn_frame = Frame {
            version: phantom_core::constants::PROTOCOL_VERSION,
            stream_id,
            flags: FrameFlags::SYN | FrameFlags::UDP | FrameFlags::DATA,
            payload: Bytes::from(syn_payload),
        };
        frame_writer.write_frame(&syn_frame).await?;
        frame_writer.flush().await?;

        // Wait for ACK.
        let ack = frame_reader.read_frame().await?;
        if !ack.flags.contains(FrameFlags::ACK) {
            return Err(PhantomError::Protocol("UDP SYN rejected".to_string()));
        }

        // Store the sender channel.
        self.udp_proxy_flows.flows.lock().await.insert(*key, tx);

        // Spawn relay task.
        let device = Arc::clone(&self.device);
        let udp_proxy_flows = self.udp_proxy_flows.clone();
        let key_clone = *key;
        let src_ip = key.src_ip;
        let src_port = key.src_port;

        tokio::spawn(async move {
            // Writer: receives datagrams from TUN, sends as UDP|DATA frames.
            let writer = async {
                while let Some(data) = rx.recv().await {
                    let frame = Frame {
                        version: phantom_core::constants::PROTOCOL_VERSION,
                        stream_id,
                        flags: FrameFlags::UDP | FrameFlags::DATA,
                        payload: Bytes::from(data),
                    };
                    if frame_writer.write_frame(&frame).await.is_err() {
                        break;
                    }
                    if frame_writer.flush().await.is_err() {
                        break;
                    }
                }
                let _ = frame_writer.write_frame(&Frame::fin(stream_id)).await;
                let _ = frame_writer.flush().await;
                Ok::<_, PhantomError>(())
            };

            // Reader: receives UDP|DATA frames, builds packets, writes to TUN.
            let reader = async {
                loop {
                    let frame = match frame_reader.read_frame().await {
                        Ok(f) => f,
                        Err(_) => break,
                    };
                    if frame.flags.contains(FrameFlags::DATA)
                        && frame.flags.contains(FrameFlags::UDP)
                    {
                        let pkt = match build_udp_packet(
                            dst_ip,
                            dst_port,
                            src_ip,
                            src_port,
                            &frame.payload,
                        ) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        let mut dev = device.lock().await;
                        let _ = dev.write_packet(&pkt).await;
                    } else if frame.flags.contains(FrameFlags::FIN)
                        || frame.flags.contains(FrameFlags::RST)
                    {
                        break;
                    }
                }
                Ok::<_, PhantomError>(())
            };

            let _ = tokio::try_join!(writer, reader);
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

fn decode_public_key_for_udp(b64: &str) -> Result<[u8; 32]> {
    let decoded = STANDARD
        .decode(b64.trim())
        .map_err(|e| PhantomError::Crypto(format!("Base64 decode failed: {}", e)))?;
    if decoded.len() != 32 {
        return Err(PhantomError::Crypto(format!(
            "Public key must be 32 bytes, got {}",
            decoded.len()
        )));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&decoded);
    Ok(key)
}
