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
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};

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

/// Tick used once a flow has nothing in flight.
///
/// The supervisor exists to retransmit unacknowledged bytes and to flush a
/// queued FIN. A fully acknowledged flow has neither, and the 500 ms cadence
/// then wakes the runtime twice a second, per flow, for nothing. With a few
/// dozen parked keep-alive connections that is the largest single source of
/// idle wake-ups in the client, so a quiet flow is checked far less often —
/// still often enough to notice a FIN or an epoch change promptly.
const IDLE_TICK: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a flow may carry no payload in either direction before it is
/// reclaimed.
///
/// Nothing else ever retires such a flow: the retransmit budget only fires on
/// flows that have bytes in flight, and the relay task is parked on two
/// `await`s that `try_join!` will not return from. It stayed forever, holding
/// an upstream socket, a table entry and its supervisor. Five minutes is past
/// any HTTP keep-alive or streaming gap; anything genuinely idle is reconnected
/// by the app the moment it writes again.
const TCP_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Hard cap on concurrent TCP flows.
///
/// Each flow owns an upstream socket and a supervisor task, and the table used
/// to be unbounded — a burst of background syncs opened tens of thousands of
/// them before any retired (26k sockets stuck in `FIN_WAIT1` on a real device
/// took the whole process down). At the cap a new SYN is answered with RST so
/// the app backs off immediately rather than queueing behind a resource that is
/// not coming back.
const MAX_FLOWS: usize = 4096;

/// How long a *direct* UDP mapping may hear nothing back before it is closed.
///
/// The mapping exists so replies can find their way back to the app; nothing
/// keeps it alive once the app stops talking, and each one held a socket plus a
/// reader task forever — `recv_from` on a bound socket does not fail. A quiet
/// flow is torn down and the next datagram simply opens a fresh one.
const UDP_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a *tunnelled* UDP mapping may go unused before it is released.
///
/// QUIC and gaming traffic come back in bursts, so this is deliberately the
/// longer of the two UDP timeouts — but it still has to exist, because the
/// client-side mapping otherwise only disappears when the server closes the
/// flow, which it has no reason to do.
const UDP_PROXY_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How often the tunnelled-UDP reaper checks for idle flows.
const UDP_REAP_TICK: std::time::Duration = std::time::Duration::from_secs(15);

/// Whether a parked TCP flow has been quiet long enough to reclaim.
///
/// This *is* the "do not kill a live long connection" contract, so it is a free
/// function rather than a condition buried in the supervisor loop: a flow is
/// retired only when nothing is queued, no FIN is still owed to the app, and no
/// payload has moved in either direction for `TCP_IDLE_TIMEOUT`.
///
/// Note the caller derives `idle_for` from `last_activity_at`, which is advanced
/// by payload only — never by a bare ACK. A peer that keeps acknowledging
/// nothing therefore cannot keep a dead flow alive.
fn tcp_idle_reclaimable(
    idle_for: std::time::Duration,
    queued_payload: bool,
    fin_queued: bool,
) -> bool {
    !queued_payload && !fin_queued && idle_for >= TCP_IDLE_TIMEOUT
}

/// Whether a tunnelled UDP mapping has gone unused long enough to release.
///
/// Split out for the same reason as [`tcp_idle_reclaimable`]: the timeout is the
/// only thing keeping an idle QUIC/gaming mapping from living for the session.
fn udp_proxy_idle_expired(idle_for: std::time::Duration) -> bool {
    idle_for >= UDP_PROXY_IDLE_TIMEOUT
}

/// How long the shared DNS-over-tunnel flow may sit without a query.
///
/// One flow serves every query the tunnel resolves, so it is worth keeping
/// while DNS is busy — but it used to stay up for the whole session, holding a
/// socket pair over a link the phone may have long since switched. The next
/// query re-establishes it, and that query can ride the flow-establishing SYN,
/// so the cost of letting it go is one round trip.
const DNS_TUNNEL_IDLE: std::time::Duration = std::time::Duration::from_secs(60);

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

/// Retransmissions of a single flow are rate-limited by this cooldown.
///
/// Three duplicate ACKs mean "one segment is missing", not "send the whole
/// window again". Without the guard each incoming dup-ACK pair re-injected the
/// entire unacknowledged window: 7 seconds of a YouTube stream produced 1600
/// retransmit events and 115 MB of injected duplicates.
const RETRANSMIT_GUARD: std::time::Duration = std::time::Duration::from_millis(50);

/// Consecutive no-progress retransmission rounds tolerated before the flow is
/// reset so the app can reconnect immediately instead of hanging.
const MAX_NO_PROGRESS_ROUNDS: u32 = 6;

/// Sliding window and cap for duplicate bytes injected into one flow.
const DUP_INJECT_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
const DUP_INJECT_WINDOW_BUDGET: u64 = 256 * 1024;
const DUP_INJECT_TOTAL_BUDGET: u64 = 8 * 1024 * 1024;

/// Trace at most one retransmit line per flow per second.
const RETRANSMIT_TRACE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// How much outgoing payload may wait in the write queue before producers apply
/// backpressure. Sized so a stalled queue is visible (and bounded) rather than
/// an OOM or a silent drop.
const TUN_WRITE_HIGH_WATER: usize = 4 * 1024 * 1024;

/// A single TUN write that waited longer than this is worth a WARN line: it is
/// the signature of the reader holding the device while ACKs queue behind it.
const TUN_WRITE_STALL_WARN: std::time::Duration = std::time::Duration::from_millis(100);

/// How long a direct connection may take before it is retried through the
/// tunnel. Censored addresses are blackholed rather than refused, so "connect
/// failed" only shows up as a timeout; 2.5 s keeps the retry well inside the
/// app's own connect timeout while staying clear of ordinary domestic RTTs.
const DIRECT_FALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2500);

/// How long a destination stays marked "direct does not work here".
///
/// Without this every *new* connection to a blackholed address pays the full
/// 2.5 s timeout again. YouTube opens dozens of connections to CDN addresses
/// that are not tied to a whitelisted domain, and the on-device log showed 21
/// such timeouts (≈53 s of stalling) in a ten-minute session — the "不断加载"
/// the operator sees is largely this, one connection at a time.
const DIRECT_FAILURE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// Upper bound on remembered failures, so a scanning app cannot grow it.
const DIRECT_FAILURE_MAX: usize = 512;

/// Bumped whenever the OS tells us the underlying network changed.
///
/// Every flow records the epoch it was born in. When the epoch moves, sockets
/// bound to the old source address are dead on arrival (the phone's IP changed
/// with the network), so flows are torn down immediately and the apps get a
/// reset to retry on the new link instead of hanging until their own timeout.
static NETWORK_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Destinations whose direct connection already failed in this session.
///
/// Keyed by the /24 (IPv4) or the address itself (IPv6): censorship blackholes
/// a whole range, so remembering one address per connection only pays the
/// 2.5 s timeout again for its neighbours.
fn direct_failure_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            IpAddr::V4(std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], 0))
        }
        v6 => v6,
    }
}

#[derive(Default)]
struct DirectFailureCache {
    entries: std::sync::Mutex<HashMap<IpAddr, std::time::Instant>>,
}

impl DirectFailureCache {
    /// `true` when this destination burned a direct attempt recently.
    fn is_known_bad(&self, ip: IpAddr, now: std::time::Instant) -> bool {
        let key = direct_failure_key(ip);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.get(&key) {
            Some(at) if now.duration_since(*at) < DIRECT_FAILURE_TTL => true,
            Some(_) => {
                entries.remove(&key);
                false
            }
            None => false,
        }
    }

    fn remember(&self, ip: IpAddr, now: std::time::Instant) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= DIRECT_FAILURE_MAX {
            // Drop the stalest half rather than refusing new knowledge.
            let mut ordered: Vec<(IpAddr, std::time::Instant)> =
                entries.iter().map(|(k, v)| (*k, *v)).collect();
            ordered.sort_by_key(|(_, at)| *at);
            for (key, _) in ordered.into_iter().take(DIRECT_FAILURE_MAX / 2) {
                entries.remove(&key);
            }
        }
        entries.insert(direct_failure_key(ip), now);
    }

    fn clear(&self) {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

static DIRECT_FAILURES: std::sync::OnceLock<DirectFailureCache> = std::sync::OnceLock::new();

fn direct_failures() -> &'static DirectFailureCache {
    DIRECT_FAILURES.get_or_init(DirectFailureCache::default)
}

/// Invalidate every flow and report the new epoch.
pub fn bump_network_epoch() -> u64 {
    // A new link may not be censored the way the old one was, so the "direct
    // does not work here" memory is scoped to one network.
    direct_failures().clear();
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

    /// Single **non-blocking** write attempt.
    ///
    /// The pump drives reads and writes from one task, so a write must never
    /// park the task: if the kernel queue is full the packet stays at the head
    /// of our own queue and is retried after the next read. Returns the number
    /// of bytes the kernel accepted (which may be short).
    #[cfg(any(target_os = "android", target_env = "ohos"))]
    pub fn try_write_packet(&mut self, pkt: &[u8]) -> std::io::Result<usize> {
        // SAFETY: `pkt` is a valid slice for the duration of the call.
        let n = unsafe {
            libc::write(
                self.inner.get_ref().as_raw_fd(),
                pkt.as_ptr() as *const libc::c_void,
                pkt.len(),
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    /// Single **non-blocking** write attempt (self-created utun path).
    #[cfg(not(any(target_os = "android", target_env = "ohos")))]
    pub fn try_write_packet(&mut self, pkt: &[u8]) -> std::io::Result<usize> {
        use std::pin::Pin;
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        match Pin::new(&mut self.inner).poll_write(&mut cx, pkt) {
            Poll::Ready(Ok(n)) => Ok(n),
            Poll::Ready(Err(e)) => Err(e),
            Poll::Pending => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
        }
    }
}

/// The read/write surface the TUN pump drives.
///
/// Exists so the pump can be exercised by a mock device in tests: the failure
/// mode being fixed is "a read that never becomes ready while writes queue
/// behind it", and that is impossible to reproduce with a real utun.
pub trait TunIo: Send {
    /// Wait for one inbound IP packet.
    fn read_packet<'a>(
        &'a mut self,
        buf: &'a mut BytesMut,
    ) -> impl std::future::Future<Output = Result<usize>> + Send + 'a;

    /// Try to hand one outbound IP packet to the kernel (never blocks).
    fn try_write_packet(&mut self, pkt: &[u8]) -> std::io::Result<usize>;
}

impl TunIo for TunDevice {
    fn read_packet<'a>(
        &'a mut self,
        buf: &'a mut BytesMut,
    ) -> impl std::future::Future<Output = Result<usize>> + Send + 'a {
        TunDevice::read_packet(self, buf)
    }

    fn try_write_packet(&mut self, pkt: &[u8]) -> std::io::Result<usize> {
        TunDevice::try_write_packet(self, pkt)
    }
}

/// Write side of the TUN device.
///
/// Producers (SYN-ACK/ACK/RST/PSH/FIN, UDP replies, DNS answers) enqueue here
/// instead of taking the device lock. That is the whole point: the reader used
/// to hold the device while awaiting a packet, so an ACK the app needed in
/// order to unblock itself could not be written until the *next* inbound packet
/// arrived — a self-inflicted stall that looked like a dead tunnel.
pub struct TunWriter {
    queue: std::sync::Mutex<VecDeque<Bytes>>,
    queued_bytes: AtomicUsize,
    notify: Notify,
    stats: Arc<TrafficStats>,
}

impl TunWriter {
    pub fn new(stats: Arc<TrafficStats>) -> Arc<Self> {
        Arc::new(Self {
            queue: std::sync::Mutex::new(VecDeque::new()),
            queued_bytes: AtomicUsize::new(0),
            notify: Notify::new(),
            stats,
        })
    }

    /// Queue one packet, applying backpressure only above the high-water mark.
    pub async fn send(&self, pkt: Vec<u8>) {
        self.send_bytes(Bytes::from(pkt)).await;
    }

    /// Queue one packet that is already a `Bytes` (no copy).
    pub async fn send_bytes(&self, pkt: Bytes) {
        if pkt.is_empty() {
            return;
        }
        let mut pending = Some(pkt);
        loop {
            // Register interest *before* looking at the queue: a producer that
            // pushes between the check and the await would otherwise be lost.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            {
                let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                if self.queued_bytes.load(Ordering::Relaxed) < TUN_WRITE_HIGH_WATER {
                    let pkt = pending.take().expect("packet taken once");
                    self.queued_bytes.fetch_add(pkt.len(), Ordering::Relaxed);
                    queue.push_back(pkt);
                }
            }
            if pending.is_none() {
                self.stats
                    .record_tun_queue_depth(self.queued_bytes.load(Ordering::Relaxed) as u64);
                // Wake the pump; `notify_waiters` cannot leave a stored permit
                // behind, which is why the pump re-checks the queue after
                // registering (see `run_pump`).
                self.notify.notify_waiters();
                return;
            }
            notified.await;
        }
    }

    /// Take the oldest queued packet, if any.
    fn pop(&self) -> Option<Bytes> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let pkt = queue.pop_front()?;
        self.queued_bytes.fetch_sub(pkt.len(), Ordering::Relaxed);
        Some(pkt)
    }

    fn queued(&self) -> usize {
        self.queued_bytes.load(Ordering::Relaxed)
    }

    // ---- TUN-path accounting -------------------------------------------
    //
    // The SOCKS5 path has always counted bytes; the TUN path did not, which is
    // exactly why "the video is stuck but 115 MB moved" was invisible until we
    // read the On-device trace by hand.
    /// Payload accepted from the app (unique bytes).
    pub fn count_up(&self, bytes: u64) {
        self.stats.record_tcp_up(bytes);
    }

    /// Payload injected into the app for the first time.
    pub fn count_down(&self, bytes: u64) {
        self.stats.record_tcp_down(bytes);
    }

    /// Payload injected into the app a second time.
    pub fn count_dup(&self, bytes: u64) {
        self.stats.record_tcp_dup(bytes);
    }

    pub fn count_udp_up(&self, bytes: u64) {
        self.stats.record_udp_up(bytes);
    }

    pub fn count_udp_down(&self, bytes: u64) {
        self.stats.record_udp_down(bytes);
    }

    pub fn count_connection(&self) {
        self.stats.record_tcp_connect();
    }

    pub fn stats(&self) -> &Arc<TrafficStats> {
        &self.stats
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
        last_activity_at: std::time::Instant::now(),
        traced_injections: 0,
        bytes_from_app: 0,
        bytes_to_app: 0,
        retransmit_cooldown_until: None,
        retransmit_round: 0,
        retransmit_una_mark: 0,
        dup_inject_window_bytes: 0,
        dup_inject_window_start: std::time::Instant::now(),
        dup_inject_total: 0,
        last_retransmit_trace_at: None,
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
    /// Last time payload actually crossed this flow, in either direction.
    ///
    /// Deliberately *not* refreshed by bare ACKs: a flow that only ever
    /// acknowledges is exactly the one worth reclaiming, and counting ACK
    /// round-trips would keep every parked connection alive forever.
    last_activity_at: std::time::Instant,
    /// Number of segments injected into the app so far (trace cap).
    traced_injections: u32,
    /// Payload bytes the app sent us / we delivered to the app.
    bytes_from_app: u64,
    bytes_to_app: u64,
    /// Earliest instant the next retransmission may be sent (guard/back-off).
    retransmit_cooldown_until: Option<std::time::Instant>,
    /// Consecutive retransmit rounds in which SND.UNA did not move.
    retransmit_round: u32,
    /// SND.UNA at the last retransmit, used for no-progress detection.
    retransmit_una_mark: u32,
    /// Duplicate bytes injected in the current budget window.
    dup_inject_window_bytes: u64,
    dup_inject_window_start: std::time::Instant,
    /// Duplicate bytes injected into this flow since it was created.
    dup_inject_total: u64,
    /// Last time a `retransmit` trace line was emitted (rate limit).
    last_retransmit_trace_at: Option<std::time::Instant>,
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

    /// Number of live flows, used to bound the table (see [`MAX_FLOWS`]).
    pub async fn len(&self) -> usize {
        self.flows.lock().await.len()
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

    async fn get_or_create(&self, key: &FlowKey) -> Result<(Arc<tokio::net::UdpSocket>, bool)> {
        let mut map = self.flows.lock().await;
        if let Some(sock) = map.get(key) {
            return Ok((Arc::clone(sock), false));
        }
        let sock = tokio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(PhantomError::Io)?;
        let arc = Arc::new(sock);
        map.insert(*key, Arc::clone(&arc));
        Ok((arc, true))
    }

    /// Removes `key`, but only while it still maps to `expected`.
    ///
    /// A retiring reader must not evict the socket a concurrent datagram just
    /// created: the idle timeout and a fresh `send_to` can interleave.
    async fn remove_if(&self, key: &FlowKey, expected: &Arc<tokio::net::UdpSocket>) {
        let mut map = self.flows.lock().await;
        if map.get(key).is_some_and(|current| Arc::ptr_eq(current, expected)) {
            map.remove(key);
        }
    }
}

impl Clone for UdpFlowTable {
    fn clone(&self) -> Self {
        Self {
            flows: Arc::clone(&self.flows),
        }
    }
}

/// One tunnelled UDP flow as the client sees it.
struct UdpProxyFlow {
    /// Datagrams to deliver to the flow's target (the frame pump's input).
    outbound: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// When the flow was established; `last_send_ms` is measured from here.
    created: std::time::Instant,
    /// Milliseconds after `created` of the last datagram we sent. Atomic
    /// because `handle_udp` touches it on the TUN loop while the reaper reads it.
    last_send_ms: std::sync::atomic::AtomicU64,
}

impl UdpProxyFlow {
    /// Milliseconds this flow has been quiet.
    fn idle_ms(&self) -> u64 {
        let since_created = self.created.elapsed().as_millis() as u64;
        since_created.saturating_sub(self.last_send_ms.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// Tracks active UDP proxy flows (sending UDP through the Phantom tunnel).
/// Each flow holds a sender channel for injecting datagrams into the relay task.
struct UdpProxyFlowTable {
    flows: Arc<Mutex<HashMap<FlowKey, Arc<UdpProxyFlow>>>>,
}

impl UdpProxyFlowTable {
    fn new() -> Self {
        Self {
            flows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Removes `key`, but only while it still maps to `expected`.
    async fn remove_if(&self, key: &FlowKey, expected: &Arc<UdpProxyFlow>) {
        let mut map = self.flows.lock().await;
        if map.get(key).is_some_and(|current| Arc::ptr_eq(current, expected)) {
            map.remove(key);
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
    writer: Arc<TunWriter>,
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
            let action = match route {
                DnsRoute::Tunnel => RuleAction::Proxy,
                DnsRoute::Local => RuleAction::Direct,
            };
            tracing::info!(
                "{}",
                crate::whitelist::dns_route_log_line(&domain, route.as_str(), action, &joined)
            );
        }
    }
    let pkt = build_dns_response_packet(&payload, &ctx)?;
    writer.send(pkt).await;
    Ok(())
}

/// Main TUN transparent proxy.
///
/// Generic over the device only so tests can drive the pump with a mock; the
/// platform bridges keep writing `TunProxy::new(device, addr)` and get the
/// default `TunDevice` instantiation.
pub struct TunProxy<D: TunIo = TunDevice> {
    /// The device is owned by the pump task while `run()` is in flight, hence
    /// the `Option`. Nothing else may touch it — all output goes through
    /// [`TunWriter`].
    device: Arc<Mutex<Option<D>>>,
    writer: Arc<TunWriter>,
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

impl<D: TunIo> TunProxy<D> {
    pub fn new(device: D, socks5_addr: SocketAddr) -> Self {
        let stats = TrafficStats::new();
        Self {
            device: Arc::new(Mutex::new(Some(device))),
            writer: TunWriter::new(Arc::clone(&stats)),
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
            stats,
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
        // The writer reports queue depth and byte counts, so it must share the
        // instance the runtime serves over `/metrics`. Safe here: builders run
        // before `run()`, so the queue is still empty.
        self.writer = TunWriter::new(Arc::clone(&stats));
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
                self.writer.send(pkt).await;
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
                let device = Arc::clone(&self.writer);
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
                    loop {
                        // The shared flow is one socket pair serving every
                        // tunnelled query, so it is worth keeping while DNS is
                        // busy — but it used to outlive the session. Let a quiet
                        // stretch close it; the next query re-establishes the
                        // flow and can ride the flow-establishing SYN, so the
                        // cost is one round trip.
                        match tokio::time::timeout(DNS_TUNNEL_IDLE, inbound.recv()).await {
                            Ok(Some(payload)) => {
                                dns_task
                                    .handle_tunnel_response(payload, &mut on_response)
                                    .await;
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
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
            let device = Arc::clone(&self.writer);
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

        // Take ownership of the device for the lifetime of the pump. Every
        // writer goes through `self.writer`, so nothing else needs it.
        let mut device = {
            let mut guard = self.device.lock().await;
            guard
                .take()
                .ok_or_else(|| PhantomError::Config("TUN pump already running".to_string()))?
        };
        self.run_pump(&mut device).await
    }

    /// Single task owning one TUN device: reads and writes interleave here, so
    /// an ACK can always go out while the reader waits for the next packet.
    ///
    /// The previous shape locked the device around `read_packet().await`, which
    /// blocks until the app sends something. Every reply queued behind that
    /// lock, so a video flow that needed our ACK to continue waiting for its
    /// own next packet — the deadlock-ish stall behind the 115 MB of duplicate
    /// injections: the app kept re-ACKing a hole we could not fill because the
    /// ACK we owed it was stuck behind a read that only that ACK could unblock.
    async fn run_pump<D2: TunIo>(&self, device: &mut D2) -> Result<()> {
        let mut buf = BytesMut::with_capacity(TUN_MTU);
        // Packet that is currently blocked on kernel backpressure.
        let mut pending: Option<Bytes> = None;
        // When the current packet started waiting for kernel writability.
        let mut stalled_since: Option<std::time::Instant> = None;

        loop {
            // 1. Drain the write queue as far as the kernel accepts.
            loop {
                if pending.is_none() {
                    pending = self.writer.pop();
                }
                let Some(pkt) = pending.take() else { break };
                match device.try_write_packet(&pkt) {
                    Ok(n) if n == pkt.len() => {
                        if let Some(started) = stalled_since.take() {
                            let waited = started.elapsed();
                            self.stats
                                .record_tun_write_wait(waited.as_millis() as u64);
                            if waited >= TUN_WRITE_STALL_WARN {
                                tracing::warn!(
                                    "tun write stalled {}ms (queue {} bytes)",
                                    waited.as_millis(),
                                    self.writer.queued()
                                );
                                crate::tun_trace!(
                                    "tun write stalled {}ms queue={}",
                                    waited.as_millis(),
                                    self.writer.queued()
                                );
                            }
                        }
                    }
                    Ok(n) if n > 0 => {
                        // Partial write: keep the tail and retry.
                        stalled_since.get_or_insert_with(std::time::Instant::now);
                        pending = Some(pkt.slice(n..));
                        break;
                    }
                    // `0` bytes taken, or the kernel queue is full.
                    Ok(_) => {
                        stalled_since.get_or_insert_with(std::time::Instant::now);
                        pending = Some(pkt);
                        break;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        stalled_since.get_or_insert_with(std::time::Instant::now);
                        pending = Some(pkt);
                        break;
                    }
                    Err(e) => return Err(PhantomError::Io(e)),
                }
            }

            // 2. Wait for either a new packet to write or an inbound packet.
            //
            // `notified()` is registered (and enabled) *before* the queue is
            // inspected again, so a producer that pushes in between cannot be
            // missed — `notify_waiters` stores no permit.
            let notified = self.writer.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if pending.is_none() {
                if let Some(pkt) = self.writer.pop() {
                    pending = Some(pkt);
                    continue;
                }
            }

            // While a write is blocked the kernel queue is full, so retry on a
            // short timer as well: reads are not guaranteed to arrive.
            let retry = tokio::time::sleep(std::time::Duration::from_millis(2));
            tokio::pin!(retry);

            tokio::select! {
                biased;
                _ = &mut notified => {}
                _ = &mut retry, if pending.is_some() => {}
                result = device.read_packet(&mut buf) => {
                    let n = result?;
                    if n == 0 {
                        continue;
                    }
                    if let Err(e) = self.handle_packet(&buf[..n]).await {
                        tracing::debug!("TUN packet error: {}", e);
                    }
                }
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
            // Bound the table before spending any work on the new flow: each
            // entry owns an upstream socket and a supervisor task, and the table
            // used to grow without limit until the process ran out of
            // descriptors. Refusing with RST makes the app back off at once.
            if self.flows.len().await >= MAX_FLOWS {
                tracing::debug!(
                    "flow table full ({}); refusing TCP flow to {}:{}",
                    MAX_FLOWS,
                    dst_ip,
                    dst_port
                );
                self.send_tcp_rst(
                    key,
                    src_ip,
                    dst_ip,
                    src_port,
                    dst_port,
                    tcp.sequence_number(),
                )
                .await?;
                return Ok(());
            }
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
                "{}",
                crate::whitelist::route_log_line(
                    format_args!("{}:{}", dst_ip, dst_port),
                    action,
                    decision.reason.as_str()
                )
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
                    self.stats.record_dup_ack();
                    if st.dup_acks >= DUP_ACK_THRESHOLD && st.seq != st.snd_una {
                        st.dup_acks = 0;
                        fast_retransmit = true;
                    }
                }
            }
            if fast_retransmit {
                let now = std::time::Instant::now();
                match decide_retransmit(&mut st, now) {
                    RetransmitDecision::Send { bytes } => {
                        if should_trace_retransmit(&mut st, now) {
                            crate::tun_trace!(
                                "retransmit {}:{} snd_una={} bytes={} snd_nxt={} queued={} win={} round={}",
                                dst_ip,
                                dst_port,
                                st.snd_una,
                                bytes,
                                st.seq,
                                st.send_queue.len(),
                                st.peer_window,
                                st.retransmit_round
                            );
                        }
                        retransmit_one_mss(&mut st, &self.writer, bytes).await;
                        self.stats.record_tcp_dup(bytes as u64);
                    }
                    RetransmitDecision::Cooldown => {
                        // The app keeps re-ACKing the same hole; one retransmit
                        // per guard window is enough to repair it.
                        self.stats.record_retransmit_suppressed(1);
                    }
                    RetransmitDecision::NothingQueued => {}
                    RetransmitDecision::GiveUp(reason) => {
                        self.stats.record_retransmit_budget_rst();
                        st.end_reason = reason.as_str();
                        let rst = build_tcp_rst_packet(&st).ok();
                        drop(st);
                        if let Some(pkt) = rst {
                            self.writer.send(pkt).await;
                        }
                        crate::tun_trace!(
                            "flow {}:{} reset after retransmit limit: {}",
                            dst_ip,
                            dst_port,
                            reason.as_str()
                        );
                        self.flows.remove(&key).await;
                        return Ok(());
                    }
                }
            }

            if fin {
                st.ack = st.ack.wrapping_add(1);
                let ack_pkt = build_tcp_ack_packet(&st)?;
                self.writer.send(ack_pkt).await;
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
                    // Payload from the app counts as activity too: an upload
                    // that never receives anything back is still in use.
                    st.last_activity_at = std::time::Instant::now();
                    self.writer.count_up(chunk.len() as u64);
                    let _ = flow.tx_to_relay.send(Bytes::copy_from_slice(chunk));
                }

                flush_send_queue(&mut st, &self.writer).await?;

                let ack_pkt = build_tcp_ack_packet(&st)?;
                self.writer.send(ack_pkt).await;
            } else if ack {
                // Pure ACK/window update: whatever was blocked may now flow.
                flush_send_queue(&mut st, &self.writer).await?;
            }
        }
        Ok(())
    }

    async fn handle_udp(&self, payload: &[u8], src_ip: IpAddr, dst_ip: IpAddr) -> Result<()> {
        let udp = etherparse::UdpHeaderSlice::from_slice(payload)
            .map_err(|e| PhantomError::Protocol(format!("UDP parse: {:?}", e)))?;
        let data = &payload[udp.slice().len()..];
        self.writer.count_udp_up(data.len() as u64);
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
                let (socket, created) = self.udp_flows.get_or_create(&key).await?;
                let dst_sa = SocketAddr::new(dst_ip, dst_port);
                socket
                    .send_to(data, dst_sa)
                    .await
                    .map_err(PhantomError::Io)?;
                if !created {
                    // A reader is already parked on this socket; spawning
                    // another per datagram would leave a growing pile of tasks
                    // that never return.
                    return Ok(());
                }

                // Spawn receiver for this UDP flow — exactly once per flow.
                let device = Arc::clone(&self.writer);
                let udp_flows = self.udp_flows.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    loop {
                        // Bound how long a mapping survives without a reply.
                        // The socket, its table entry and this task all go with
                        // it; the next datagram from the app opens a fresh one.
                        let received =
                            tokio::time::timeout(UDP_IDLE_TIMEOUT, socket.recv_from(&mut buf)).await;
                        let n = match received {
                            Ok(Ok((n, _peer))) => n,
                            Ok(Err(e)) => {
                                tracing::debug!("UDP recv error: {}", e);
                                break;
                            }
                            Err(_) => {
                                crate::tun_trace!(
                                    "udp flow {}:{} idle; closing",
                                    dst_ip,
                                    dst_port
                                );
                                tracing::debug!("UDP flow {}:{} idle; reclaiming", dst_ip, dst_port);
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
                        device.count_udp_down(n as u64);
                        device.send(pkt).await;
                    }
                    udp_flows.remove_if(&key, &socket).await;
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
        self.writer.count_connection();

        let device = Arc::clone(&self.writer);
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

        let device = Arc::clone(&self.writer);
        let flows = self.flows.clone();
        let socks5_addr = self.socks5_addr;
        let stats = Arc::clone(&self.stats);
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
                stats,
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
            if let Some(existing) = map.get(key) {
                existing
                    .last_send_ms
                    .store(existing.created.elapsed().as_millis() as u64, Ordering::Relaxed);
                match existing.outbound.send(datagram) {
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
        let entry = Arc::new(UdpProxyFlow {
            outbound: flow.outbound,
            created: std::time::Instant::now(),
            last_send_ms: std::sync::atomic::AtomicU64::new(0),
        });
        self.udp_proxy_flows
            .flows
            .lock()
            .await
            .insert(*key, Arc::clone(&entry));

        // TUN-side inbound pump: tunnel datagrams → UDP packets → TUN device.
        let device = Arc::clone(&self.writer);
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
                device.count_udp_down(data.len() as u64);
                device.send(pkt).await;
            }
            udp_proxy_flows.flows.lock().await.remove(&key_clone);
        });

        // Client-side idle expiry. The server has no reason to close a flow it
        // is not being asked about, so without this the mapping — and the
        // tunnel-side flow behind it — lived for the whole session.
        let reaper_entry = Arc::clone(&entry);
        let reaper_table = self.udp_proxy_flows.clone();
        let reap_key = *key;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(UDP_REAP_TICK).await;
                let idle_for = std::time::Duration::from_millis(reaper_entry.idle_ms());
                if !udp_proxy_idle_expired(idle_for) {
                    continue;
                }
                crate::tun_trace!("udp proxy flow {}:{} idle; closing", reap_key.dst_ip, reap_key.dst_port);
                tracing::debug!(
                    "UDP proxy flow {}:{} idle for {}s; releasing",
                    reap_key.dst_ip,
                    reap_key.dst_port,
                    idle_for.as_secs()
                );
                // Dropping the sender closes the frame pump's input, which tears
                // the tunnel-side flow down with it.
                reaper_table.remove_if(&reap_key, &reaper_entry).await;
                return;
            }
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
        let writer = Arc::clone(&self.writer);
        let stats = Arc::clone(&self.stats);
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
                        writer.send(pkt).await;
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
                    last_check = now;
                    if st.fin_queued && !st.fin_sent {
                        let _ = flush_send_queue(&mut st, &writer).await;
                    }
                    let idle_for = now.duration_since(st.last_activity_at);
                    // Reclaim only a flow that is genuinely parked: queued
                    // payload or a pending FIN both mean something is still
                    // owed to one of the two ends.
                    if tcp_idle_reclaimable(idle_for, !st.send_queue.is_empty(), st.fin_queued) {
                        st.end_reason = "idle timeout";
                        let rst = build_tcp_rst_packet(&st).ok();
                        drop(st);
                        // The empty payload is the relay task's stop signal: it
                        // shuts the upstream down, which is what lets its
                        // `try_join!` return and actually releases the socket.
                        // Dropping the table entry on its own would leak it.
                        if let Some(flow) = flows.get(&key).await {
                            let _ = flow.tx_to_relay.send(Bytes::new());
                        }
                        if let Some(pkt) = rst {
                            writer.send(pkt).await;
                        }
                        crate::tun_trace!(
                            "flow {}:{} retired: idle for {}s",
                            key.dst_ip,
                            key.dst_port,
                            idle_for.as_secs()
                        );
                        tracing::debug!(
                            "TCP flow {}:{} idle for {}s; reclaiming",
                            key.dst_ip,
                            key.dst_port,
                            idle_for.as_secs()
                        );
                        flows.remove(&key).await;
                        return;
                    }
                    // Nothing in flight to retransmit, so the fast cadence buys
                    // nothing; check back on the idle tick instead.
                    rto = IDLE_TICK;
                    continue;
                }
                // Back in flight: return to the retransmission cadence.
                rto = RETRANSMIT_TICK;
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
                    crate::tun_trace!(
                        "zero-window probe {}:{} queued={}",
                        key.dst_ip,
                        key.dst_port,
                        st.send_queue.len()
                    );
                    let probe = 1.min(st.send_queue.len());
                    retransmit_one_mss(&mut st, &writer, probe).await;
                    stats.record_tcp_dup(probe as u64);
                    st.peer_window = saved;
                } else {
                    // A timeout means "the oldest segment is missing", exactly
                    // like three duplicate ACKs — so it goes through the same
                    // one-segment, budgeted, back-off path instead of rewinding
                    // SND.NXT and re-injecting the whole window every tick.
                    let now = std::time::Instant::now();
                    match decide_retransmit(&mut st, now) {
                        RetransmitDecision::Send { bytes } => {
                            if should_trace_retransmit(&mut st, now) {
                                crate::tun_trace!(
                                    "retransmit {}:{} snd_una={} bytes={} queued={} win={} round={} cause=timeout",
                                    key.dst_ip,
                                    key.dst_port,
                                    st.snd_una,
                                    bytes,
                                    st.send_queue.len(),
                                    st.peer_window,
                                    st.retransmit_round
                                );
                            }
                            retransmit_one_mss(&mut st, &writer, bytes).await;
                            stats.record_tcp_dup(bytes as u64);
                        }
                        RetransmitDecision::Cooldown => stats.record_retransmit_suppressed(1),
                        RetransmitDecision::NothingQueued => {}
                        RetransmitDecision::GiveUp(reason) => {
                            stats.record_retransmit_budget_rst();
                            st.end_reason = reason.as_str();
                            let rst = build_tcp_rst_packet(&st).ok();
                            drop(st);
                            if let Some(pkt) = rst {
                                writer.send(pkt).await;
                            }
                            crate::tun_trace!(
                                "flow {}:{} reset after retransmit limit: {}",
                                key.dst_ip,
                                key.dst_port,
                                reason.as_str()
                            );
                            flows.remove(&key).await;
                            return;
                        }
                    }
                }
                // The cooldown now owns the pacing; keep ticking fast enough to
                // notice a reopened window promptly.
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
        let r = {
            self.writer.send(pkt).await;
            Ok::<(), PhantomError>(())
        };
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
        self.writer.send(pkt).await;
        Ok(())
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
        self.writer.send(pkt).await;
        Ok(())
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
    device: Arc<TunWriter>,
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
    device: Arc<TunWriter>,
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
    device: Arc<TunWriter>,
    flows: FlowTable,
    key: FlowKey,
    state: Arc<Mutex<TcpFlowState>>,
    dst_ip: IpAddr,
    dst_port: u16,
    socks5_addr: SocketAddr,
    fallback_to_tunnel: bool,
    stats: Arc<TrafficStats>,
) -> Result<()> {
    // Nothing has been read from the tunnel yet, so the app's buffered payload
    // is still in `rx_from_tun` and can be handed to the tunnel relay verbatim
    // if the direct connect fails.
    //
    // A destination that already timed out once goes straight to the tunnel:
    // the app cannot afford another 2.5 s of nothing on every connection.
    if fallback_to_tunnel && direct_failures().is_known_bad(dst_ip, std::time::Instant::now()) {
        tracing::info!(
            "{}",
            crate::whitelist::route_log_line(
                format_args!("{}:{}", dst_ip, dst_port),
                phantom_core::RuleAction::Proxy,
                "direct unreachable earlier on this network"
            )
        );
        crate::tun_trace!("direct-retry-skip {}:{}", dst_ip, dst_port);
        stats.record_route_direct_failed();
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
            direct_failures().remember(dst_ip, std::time::Instant::now());
            stats.record_route_direct_failed();
            tracing::info!(
                "{}",
                crate::whitelist::route_log_line(
                    format_args!("{}:{}", dst_ip, dst_port),
                    phantom_core::RuleAction::Proxy,
                    format_args!("direct connect failed: {}; retrying through the tunnel", e)
                )
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
            // Blackholed, not refused: remember it so the next connection to
            // this range skips the 2.5 s wait entirely.
            direct_failures().remember(dst_ip, std::time::Instant::now());
            stats.record_route_direct_failed();
            tracing::info!(
                "{}",
                crate::whitelist::route_log_line(
                    format_args!("{}:{}", dst_ip, dst_port),
                    phantom_core::RuleAction::Proxy,
                    "direct connect timed out; retrying through the tunnel"
                )
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

/// What to do about a "the app is missing a segment" signal.
#[derive(Debug, PartialEq, Eq)]
enum RetransmitDecision {
    /// Re-send this many bytes starting at SND.UNA.
    Send { bytes: usize },
    /// Inside the cooldown/back-off window: count it, do not inject anything.
    Cooldown,
    /// Nothing is queued to resend (nothing we can do from here).
    NothingQueued,
    /// The flow has burned its retransmission budget and must be reset.
    GiveUp(FlowResetReason),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FlowResetReason {
    /// Six consecutive rounds without SND.UNA moving.
    NoProgress,
    /// Duplicate injection exceeded its window or total budget.
    DuplicateBudget,
}

impl FlowResetReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoProgress => "no progress after retransmits",
            Self::DuplicateBudget => "duplicate injection budget spent",
        }
    }
}

/// Decide whether a retransmission may be sent right now.
///
/// Two rules, both learned the hard way from a YouTube stream that stalled
/// while flooding the TUN device with 115 MB of duplicates:
///
/// 1. **One segment, not the window.** Three duplicate ACKs say "a segment is
///    missing"; re-sending everything from SND.UNA multiplies the loss instead
///    of repairing it (the app drops most of it, ACKs the same hole, and the
///    next dup-ACK round starts over).
/// 2. **Budgeted.** A flow may inject at most `256 KiB/s` and `8 MiB` total of
///    duplicates; past that it is reset so the app reconnects immediately
///    instead of watching a spinner forever.
///
/// The cooldown also doubles per no-progress round (50→1600 ms), so a truly
/// wedged flow stops hammering the link.
fn decide_retransmit(state: &mut TcpFlowState, now: std::time::Instant) -> RetransmitDecision {
    if state.send_queue.is_empty() {
        return RetransmitDecision::NothingQueued;
    }

    // Any forward movement resets both the round counter and the back-off.
    if state.snd_una != state.retransmit_una_mark {
        state.retransmit_una_mark = state.snd_una;
        state.retransmit_round = 0;
        state.retransmit_cooldown_until = None;
    }

    if let Some(until) = state.retransmit_cooldown_until {
        if now < until {
            return RetransmitDecision::Cooldown;
        }
    }

    // Roll the per-second budget window.
    if now.duration_since(state.dup_inject_window_start) >= DUP_INJECT_WINDOW {
        state.dup_inject_window_start = now;
        state.dup_inject_window_bytes = 0;
    }
    if state.dup_inject_total >= DUP_INJECT_TOTAL_BUDGET
        || state.dup_inject_window_bytes >= DUP_INJECT_WINDOW_BUDGET
    {
        return RetransmitDecision::GiveUp(FlowResetReason::DuplicateBudget);
    }
    if state.retransmit_round >= MAX_NO_PROGRESS_ROUNDS {
        return RetransmitDecision::GiveUp(FlowResetReason::NoProgress);
    }

    let window = state.peer_window as usize;
    let bytes = TCP_MSS.min(state.send_queue.len()).min(window);
    if bytes == 0 {
        // Zero window: the supervisor probes that case separately.
        return RetransmitDecision::NothingQueued;
    }

    // 50, 100, 200, 400, 800, 1600 ms — capped, so a dead flow still gets an
    // occasional cheap probe instead of being declared dead on a hunch.
    let shift = state.retransmit_round.min(5);
    let backoff = RETRANSMIT_GUARD
        .checked_mul(1 << shift)
        .unwrap_or(MAX_RTO)
        .min(MAX_RTO);
    state.retransmit_cooldown_until = Some(now + backoff);
    state.retransmit_round = state.retransmit_round.saturating_add(1);
    state.dup_inject_window_bytes += bytes as u64;
    state.dup_inject_total += bytes as u64;
    RetransmitDecision::Send { bytes }
}

/// Emit at most one `retransmit` trace line per flow per second.
fn should_trace_retransmit(state: &mut TcpFlowState, now: std::time::Instant) -> bool {
    match state.last_retransmit_trace_at {
        Some(last) if now.duration_since(last) < RETRANSMIT_TRACE_INTERVAL => false,
        _ => {
            state.last_retransmit_trace_at = Some(now);
            true
        }
    }
}

/// Re-send exactly one MSS starting at SND.UNA. SND.NXT is left untouched.
async fn retransmit_one_mss(state: &mut TcpFlowState, writer: &Arc<TunWriter>, bytes: usize) {
    let payload = state.send_queue[..bytes].to_vec();
    let seq = state.snd_una;
    match build_tcp_psh_packet_at(state, seq, &payload) {
        Ok(pkt) => writer.send(pkt).await,
        Err(e) => tracing::debug!("retransmit build failed: {}", e),
    }
}

/// Send as much queued payload as the app's receive window allows, segmented to
/// the MSS, then the FIN once everything is out.
async fn flush_send_queue(state: &mut TcpFlowState, device: &Arc<TunWriter>) -> Result<()> {
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
        device.send(pkt).await;
        device.count_down(n as u64);
        state.seq = state.seq.wrapping_add(n as u32);
        state.bytes_to_app += n as u64;
        // Payload really crossed the flow, so its idle clock restarts.
        state.last_activity_at = std::time::Instant::now();
        bursts += 1;
        if bursts >= 64 {
            // Yield to the runtime on very large bursts; the supervisor and the
            // next ACK pick the rest up.
            break;
        }
    }

    if state.send_queue.is_empty() && state.fin_queued && !state.fin_sent {
        let pkt = build_tcp_fin_packet(state)?;
        device.send(pkt).await;
        state.seq = state.seq.wrapping_add(1);
        state.fin_sent = true;
    }
    Ok(())
}

/// Hand a chunk of tunnelled payload to the app, waiting for queue space when
/// the app has not acknowledged enough yet.
async fn queue_tunnel_payload(
    state: &Arc<Mutex<TcpFlowState>>,
    device: &Arc<TunWriter>,
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
    build_tcp_psh_packet_at(state, state.seq, payload)
}

/// Same as [`build_tcp_psh_packet`] but with an explicit sequence number, so a
/// retransmission can re-send SND.UNA without rewinding SND.NXT.
fn build_tcp_psh_packet_at(state: &TcpFlowState, seq: u32, payload: &[u8]) -> Result<Vec<u8>> {
    let mut pkt = Vec::with_capacity(128 + payload.len());
    match (state.dst_ip, state.src_ip) {
        (IpAddr::V4(dst), IpAddr::V4(src)) => {
            etherparse::PacketBuilder::ipv4(dst.octets(), src.octets(), 64)
                .tcp(state.dst_port, state.src_port, seq, 65535)
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

    // -----------------------------------------------------------------------
    // TUN write path
    // -----------------------------------------------------------------------

    /// A device whose reads never complete and which records what is written
    /// to it. Both behaviours are impossible to arrange with a real utun, and
    /// both are exactly what the reported stall looked like: the app went quiet,
    /// so `read_packet` never returned, while replies queued up behind it.
    struct MockTun {
        written: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }

    impl MockTun {
        fn new(written: Arc<std::sync::Mutex<Vec<Vec<u8>>>>) -> Self {
            Self { written }
        }
    }

    impl TunIo for MockTun {
        async fn read_packet(&mut self, _buf: &mut BytesMut) -> Result<usize> {
            std::future::pending::<()>().await;
            Ok(0)
        }

        fn try_write_packet(&mut self, pkt: &[u8]) -> std::io::Result<usize> {
            self.written
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(pkt.to_vec());
            Ok(pkt.len())
        }
    }

    /// Before the fix the reader held the device for the whole time it waited
    /// for an inbound packet, so every reply queued behind it: 100 packets took
    /// far longer than a second (in practice: until the app spoke again) and an
    /// ACK the app was waiting for could not be delivered at all.
    #[tokio::test]
    async fn pump_drains_the_write_queue_while_reads_never_become_ready() {
        let written: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let proxy = Arc::new(TunProxy::new(
            MockTun::new(Arc::clone(&written)),
            "127.0.0.1:1080".parse().unwrap(),
        ));

        let mut device = MockTun::new(Arc::clone(&written));
        let pump_proxy = Arc::clone(&proxy);
        let pump = tokio::spawn(async move {
            let _ = pump_proxy.run_pump(&mut device).await;
        });

        for i in 0..100u8 {
            proxy.writer.send(vec![i; 64]).await;
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let count = written.lock().unwrap_or_else(|e| e.into_inner()).len();
            if count == 100 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {count}/100 packets were written within 1 s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // Order is preserved: sequence matters for the TCP stack above us.
        let sink = written.lock().unwrap_or_else(|e| e.into_inner());
        for (i, pkt) in sink.iter().enumerate() {
            assert_eq!(pkt[0], i as u8, "packet {i} written out of order");
        }
        drop(sink);

        // The queue drained completely, so a producer is never left waiting.
        assert_eq!(proxy.writer.queued(), 0);
        pump.abort();
    }

    // -----------------------------------------------------------------------
    // Retransmission discipline
    // -----------------------------------------------------------------------

    fn flow_with_queue(bytes: usize) -> TcpFlowState {
        let mut state = new_flow_state(
            "10.7.0.2".parse().unwrap(),
            "142.251.91.201".parse().unwrap(),
            40000,
            443,
            5000,
        );
        state.send_queue = vec![7u8; bytes];
        state.peer_window = 65535;
        state
    }

    #[test]
    fn retransmit_sends_one_mss_then_respects_the_guard() {
        let mut state = flow_with_queue(6 * TCP_MSS);
        let t0 = std::time::Instant::now();

        match decide_retransmit(&mut state, t0) {
            RetransmitDecision::Send { bytes } => {
                assert_eq!(bytes, TCP_MSS, "a dup ACK means one segment is missing");
            }
            other => panic!("expected a single-segment retransmit, got {other:?}"),
        }

        // The app keeps re-ACKing the same hole; without the guard this is the
        // loop that produced 1600 retransmits and 115 MB of duplicates.
        assert_eq!(
            decide_retransmit(&mut state, t0 + std::time::Duration::from_millis(10)),
            RetransmitDecision::Cooldown
        );

        // Past the guard another single segment may go out.
        assert!(matches!(
            decide_retransmit(&mut state, t0 + RETRANSMIT_GUARD + std::time::Duration::from_millis(1)),
            RetransmitDecision::Send { bytes } if bytes == TCP_MSS
        ));

        // Spending the budget is bounded: two segments, not a window.
        assert_eq!(state.dup_inject_total, 2 * TCP_MSS as u64);
    }

    #[test]
    fn retransmit_gives_up_after_repeated_no_progress() {
        let mut state = flow_with_queue(4 * TCP_MSS);
        let mut now = std::time::Instant::now();
        let mut sends = 0;
        let mut give_up = None;
        for _ in 0..40 {
            match decide_retransmit(&mut state, now) {
                RetransmitDecision::Send { .. } => {
                    sends += 1;
                    now += MAX_RTO;
                }
                RetransmitDecision::Cooldown => now += std::time::Duration::from_millis(10),
                RetransmitDecision::GiveUp(reason) => {
                    give_up = Some(reason);
                    break;
                }
                RetransmitDecision::NothingQueued => panic!("queue was filled"),
            }
        }
        assert_eq!(give_up, Some(FlowResetReason::NoProgress));
        assert_eq!(sends, MAX_NO_PROGRESS_ROUNDS, "back-off rounds are bounded");
    }

    #[test]
    fn retransmit_total_budget_stops_the_flood() {
        let mut state = flow_with_queue(64);
        let mut now = std::time::Instant::now();
        let mut injected = 0u64;
        let mut result = None;
        for _ in 0..200_000 {
            match decide_retransmit(&mut state, now) {
                RetransmitDecision::Send { bytes } => {
                    injected += bytes as u64;
                    // Simulate the app draining what we sent, so the flow keeps
                    // making progress and only the byte budget can stop it.
                    state.snd_una = state.snd_una.wrapping_add(bytes as u32);
                    state.send_queue = vec![7u8; 64];
                    now += std::time::Duration::from_millis(20);
                }
                RetransmitDecision::Cooldown => now += std::time::Duration::from_millis(5),
                RetransmitDecision::GiveUp(reason) => {
                    result = Some(reason);
                    break;
                }
                RetransmitDecision::NothingQueued => panic!("queue was filled"),
            }
        }
        assert_eq!(result, Some(FlowResetReason::DuplicateBudget));
        assert!(
            injected <= DUP_INJECT_TOTAL_BUDGET + TCP_MSS as u64,
            "injected {injected} bytes, budget is {DUP_INJECT_TOTAL_BUDGET}"
        );
        assert!(
            injected >= DUP_INJECT_TOTAL_BUDGET - DUP_INJECT_WINDOW_BUDGET,
            "the cap should be reached, not tripped early ({injected} bytes)"
        );
    }

    // -----------------------------------------------------------------------
    // Direct-connect failure memory
    // -----------------------------------------------------------------------

    #[test]
    fn direct_failure_is_remembered_per_range() {
        let cache = DirectFailureCache::default();
        let t0 = std::time::Instant::now();
        let bad = "209.85.228.136".parse().unwrap();

        assert!(!cache.is_known_bad(bad, t0));
        cache.remember(bad, t0);
        assert!(cache.is_known_bad(bad, t0));
        // Same /24: the whole range is blackholed, so its neighbours must not
        // pay the 2.5 s timeout again.
        assert!(cache.is_known_bad("209.85.228.169".parse().unwrap(), t0));
        // A different range is unaffected.
        assert!(!cache.is_known_bad("142.250.199.78".parse().unwrap(), t0));
    }

    #[test]
    fn direct_failure_memory_expires() {
        let cache = DirectFailureCache::default();
        let t0 = std::time::Instant::now();
        let bad = "209.85.228.136".parse().unwrap();
        cache.remember(bad, t0);
        assert!(cache.is_known_bad(bad, t0 + DIRECT_FAILURE_TTL - std::time::Duration::from_secs(1)));
        assert!(!cache.is_known_bad(bad, t0 + DIRECT_FAILURE_TTL + std::time::Duration::from_secs(1)));
    }

    #[test]
    fn direct_failure_memory_is_bounded() {
        let cache = DirectFailureCache::default();
        let t0 = std::time::Instant::now();
        for i in 0..(DIRECT_FAILURE_MAX + 64) {
            let ip: IpAddr = format!("10.{}.{}.1", (i / 256) % 256, i % 256).parse().unwrap();
            cache.remember(ip, t0 + std::time::Duration::from_millis(i as u64));
        }
        assert!(
            cache.len() <= DIRECT_FAILURE_MAX,
            "cache grew to {} entries",
            cache.len()
        );
    }

    // -----------------------------------------------------------------------
    // Idle reclaim
    // -----------------------------------------------------------------------

    /// The point of the idle timeout is to retire a flow nobody is using, so it
    /// must not fire while either end is still owed something.
    #[test]
    fn tcp_idle_reclaim_spares_a_flow_with_anything_outstanding() {
        let past = TCP_IDLE_TIMEOUT + std::time::Duration::from_secs(60);
        assert!(
            tcp_idle_reclaimable(past, false, false),
            "a parked flow should be reclaimed"
        );
        assert!(
            !tcp_idle_reclaimable(past, true, false),
            "queued payload means the app is still waiting for bytes"
        );
        assert!(
            !tcp_idle_reclaimable(past, false, true),
            "a FIN still owed to the app means the flow is mid-close"
        );
        assert!(!tcp_idle_reclaimable(past, true, true));
    }

    /// Long connections only survive because the threshold is far past any
    /// keep-alive or streaming gap, so the boundary is asserted explicitly: a
    /// later tweak that made the timeout bite would fail here, not on a phone.
    #[test]
    fn tcp_idle_reclaim_fires_only_past_the_timeout() {
        assert!(!tcp_idle_reclaimable(
            TCP_IDLE_TIMEOUT - std::time::Duration::from_secs(1),
            false,
            false
        ));
        assert!(tcp_idle_reclaimable(TCP_IDLE_TIMEOUT, false, false));
        // An HTTP keep-alive gap and an idle stretch of a streaming response
        // both sit well inside the window.
        assert!(!tcp_idle_reclaimable(
            std::time::Duration::from_secs(120),
            false,
            false
        ));
    }

    #[test]
    fn udp_proxy_idle_expiry_fires_only_past_the_timeout() {
        assert!(!udp_proxy_idle_expired(
            UDP_PROXY_IDLE_TIMEOUT - std::time::Duration::from_secs(1)
        ));
        assert!(udp_proxy_idle_expired(UDP_PROXY_IDLE_TIMEOUT));
    }

    /// The tunnelled mapping is documented as "deliberately the longer of the
    /// two" because QUIC and gaming traffic return in bursts, and it must stay
    /// longer than the reaper's own tick or a flow could be reaped before the
    /// reaper has had a chance to see it being used.
    #[test]
    fn udp_timeouts_keep_their_documented_ordering() {
        assert!(UDP_PROXY_IDLE_TIMEOUT >= UDP_IDLE_TIMEOUT);
        assert!(UDP_IDLE_TIMEOUT > UDP_REAP_TICK);
        assert!(DNS_TUNNEL_IDLE > UDP_REAP_TICK);
    }
}
