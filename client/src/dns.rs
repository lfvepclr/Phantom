//! DNS proxy for TUN mode.
//!
//! Intercepts UDP:53 queries from the TUN device and answers them on one of
//! two transports, chosen per query by the routing decision:
//!
//! * **Tunnel** (`DnsRoute::Tunnel`) — censored domains (Smart whitelist) and
//!   everything in Proxy mode. The query travels inside the Phantom tunnel and
//!   is resolved by the server's network, so GFW pollution never applies.
//! * **Local** (`DnsRoute::Local`) — everything else. The query goes out of the
//!   physical network, so domestic CDNs keep answering with a local node.
//!
//! Responses from both transports land in the same pending-query table and are
//! rewritten back into the TUN with the application's original 5-tuple.
//!
//! This replaces the previous behaviour of forwarding *every* query over a
//! plain UDP socket to `tls://8.8.8.8:853` (a port that only speaks TLS), which
//! made TUN-mode name resolution fail outright.

use bytes::{Bytes, BytesMut};
use phantom_core::{PhantomError, Result};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

/// Parse a `client.dns` config value into a socket address.
///
/// Accepts a bare IP (`1.1.1.1` → port 53), an `ip:port` pair, and tolerates
/// the `tls://` / `https://` scheme prefixes used in the config templates.
pub fn parse_dns_addr(dns: &str) -> Option<SocketAddr> {
    let stripped = dns.strip_prefix("tls://").unwrap_or(dns);
    let stripped = stripped.strip_prefix("https://").unwrap_or(stripped);
    stripped
        .parse()
        .ok()
        .or_else(|| format!("{}:53", stripped).parse().ok())
}

/// Simple DNS header (12 bytes).
#[derive(Debug, Clone, Copy)]
pub struct DnsHeader {
    pub id: u16,
    pub flags: u16,
    pub questions: u16,
    pub answer_rrs: u16,
    pub authority_rrs: u16,
    pub additional_rrs: u16,
}

impl DnsHeader {
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 12 {
            return None;
        }
        Some(Self {
            id: u16::from_be_bytes([buf[0], buf[1]]),
            flags: u16::from_be_bytes([buf[2], buf[3]]),
            questions: u16::from_be_bytes([buf[4], buf[5]]),
            answer_rrs: u16::from_be_bytes([buf[6], buf[7]]),
            authority_rrs: u16::from_be_bytes([buf[8], buf[9]]),
            additional_rrs: u16::from_be_bytes([buf[10], buf[11]]),
        })
    }

    pub fn encode(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&self.id.to_be_bytes());
        buf[2..4].copy_from_slice(&self.flags.to_be_bytes());
        buf[4..6].copy_from_slice(&self.questions.to_be_bytes());
        buf[6..8].copy_from_slice(&self.answer_rrs.to_be_bytes());
        buf[8..10].copy_from_slice(&self.authority_rrs.to_be_bytes());
        buf[10..12].copy_from_slice(&self.additional_rrs.to_be_bytes());
    }
}

/// Extract the queried domain name from a DNS question section.
/// Returns the domain and the number of bytes consumed in the question section.
pub fn extract_query_domain(buf: &[u8]) -> Option<(String, usize)> {
    if buf.len() < 12 {
        return None;
    }
    let header = DnsHeader::decode(buf)?;
    if header.questions == 0 {
        return None;
    }

    let mut offset = 12;
    let mut labels = Vec::new();
    loop {
        if offset >= buf.len() {
            return None;
        }
        let len = buf[offset] as usize;
        if len == 0 {
            offset += 1;
            break;
        }
        if len & 0xC0 == 0xC0 {
            // Compression pointer — skip for MVP.
            offset += 2;
            break;
        }
        offset += 1;
        if offset + len > buf.len() {
            return None;
        }
        labels.push(String::from_utf8_lossy(&buf[offset..offset + len]).into_owned());
        offset += len;
    }

    // Skip QTYPE and QCLASS (4 bytes).
    if offset + 4 > buf.len() {
        return None;
    }
    offset += 4;

    Some((labels.join("."), offset))
}

/// Per-query tracking: original 5-tuple so we can rewrite the response.
#[derive(Debug, Clone)]
pub struct DnsQueryContext {
    pub src_ip: IpAddr,
    pub src_port: u16,
    pub dst_ip: IpAddr,
    pub dst_port: u16,
}

/// Transport a query was sent on. Recorded per transaction so the response
/// handler can log where the answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsRoute {
    /// Sent through the Phantom tunnel to `client.dns`.
    Tunnel,
    /// Sent over the physical network to `client.dns_direct`.
    Local,
}

impl DnsRoute {
    pub fn as_str(&self) -> &'static str {
        match self {
            DnsRoute::Tunnel => "tunnel",
            DnsRoute::Local => "local",
        }
    }
}

/// DNS record type for an IPv6 address.
pub const DNS_TYPE_AAAA: u16 = 28;

/// Build a response that mirrors the question and carries `rcode`.
///
/// Shared by the two synthetic answers the hijack can produce: REFUSED when a
/// rule rejects the query, and NOERROR/NODATA when the answer would be a dead
/// end for the app (see [`build_nodata_response`]).
fn build_status_response(query: &[u8], rcode: u16) -> Option<Vec<u8>> {
    let header = DnsHeader::decode(query)?;
    let (_, question_end) = extract_query_domain(query)?;
    let mut out = Vec::with_capacity(question_end);
    out.extend_from_slice(&query[..question_end]);
    // QR=1, RD copied from the query, RA=1, RCODE as requested.
    let mut flags = header.flags | 0x8080;
    flags = (flags & !0x000F) | (rcode & 0x000F);
    out[2..4].copy_from_slice(&flags.to_be_bytes());
    // No answers / authority / additional records.
    out[6..8].copy_from_slice(&0u16.to_be_bytes());
    out[8..10].copy_from_slice(&0u16.to_be_bytes());
    out[10..12].copy_from_slice(&0u16.to_be_bytes());
    Some(out)
}

/// Build a REFUSED answer for a query, used when a rule rejects port 53.
pub fn build_refused_response(query: &[u8]) -> Option<Vec<u8>> {
    build_status_response(query, 5)
}

/// Build a NOERROR/NODATA answer: the name exists, it simply has no record of
/// this type.
///
/// This is how the client declines to advertise an IPv6 address the tunnel
/// cannot reach, without lying in a way resolvers treat as a failure. The app
/// sees "no AAAA for this name" and uses the A record — which is what a
/// resolver would answer for an IPv4-only host.
pub fn build_nodata_response(query: &[u8]) -> Option<Vec<u8>> {
    build_status_response(query, 0)
}

/// QTYPE of the first question, or `None` when the packet is not parseable.
pub fn dns_query_type(buf: &[u8]) -> Option<u16> {
    let (_, question_end) = extract_query_domain(buf)?;
    // `question_end` points past QTYPE+QCLASS, so QTYPE is four bytes back.
    let qtype_at = question_end.checked_sub(4)?;
    if qtype_at + 2 > buf.len() {
        return None;
    }
    Some(u16::from_be_bytes([buf[qtype_at], buf[qtype_at + 1]]))
}
/// Shared DNS proxy state.
pub struct DnsProxy {
    /// Locally bound socket used for the direct path. Its local port doubles as
    /// the loop guard in `TunProxy::handle_udp`.
    local_socket: Arc<UdpSocket>,
    /// Resolver reached through the tunnel (`client.dns`).
    tunnel_upstream: RwLock<SocketAddr>,
    /// Resolver reached over the physical network (`client.dns_direct`).
    local_upstream: RwLock<SocketAddr>,
    /// Outbound half of the shared tunnel-side UDP flow to `tunnel_upstream`.
    /// `None` until the flow is established; cleared again when it dies.
    tunnel_out: RwLock<Option<UnboundedSender<Vec<u8>>>>,
    /// Pending queries: DNS transaction ID -> original TUN context.
    pending: Arc<Mutex<std::collections::HashMap<u16, DnsQueryContext>>>,
    /// Query domain names tracked so we can populate the DNS cache from responses.
    query_domains: Arc<Mutex<std::collections::HashMap<u16, String>>>,
    /// Transport each in-flight transaction is using.
    query_routes: Arc<Mutex<std::collections::HashMap<u16, DnsRoute>>>,
    /// Answer AAAA queries for tunnel-routed domains with an empty NOERROR.
    ///
    /// Set from `client.dns_ipv6_via_tunnel` (default `false`). See that field
    /// for why the safe default is to *not* advertise IPv6 through the tunnel.
    suppress_tunnel_aaaa: std::sync::atomic::AtomicBool,
}

impl DnsProxy {
    /// Create a new DNS proxy bound to a local ephemeral port.
    ///
    /// `tunnel_upstream` is only dialled through the tunnel (never directly),
    /// `local_upstream` is only dialled over the physical network.
    pub async fn new(tunnel_upstream: SocketAddr, local_upstream: SocketAddr) -> Result<Self> {
        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let socket = UdpSocket::bind(bind_addr)
            .await
            .map_err(|e| PhantomError::Io(e))?;
        // This socket deliberately talks to the *physical* network. On Android
        // the VPN owns every socket once the TUN is up, so it has to be exempted
        // here or the direct resolver's queries would be captured by our own
        // TUN and never answered.
        #[cfg(unix)]
        crate::net_tune::protect_socket(std::os::unix::io::AsRawFd::as_raw_fd(&socket));
        tracing::info!(
            "DNS proxy bound to {} (tunnel resolver {}, direct resolver {})",
            socket.local_addr()?,
            tunnel_upstream,
            local_upstream
        );
        Ok(Self {
            local_socket: Arc::new(socket),
            tunnel_upstream: RwLock::new(tunnel_upstream),
            local_upstream: RwLock::new(local_upstream),
            tunnel_out: RwLock::new(None),
            pending: Arc::new(Mutex::new(std::collections::HashMap::new())),
            query_domains: Arc::new(Mutex::new(std::collections::HashMap::new())),
            query_routes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            suppress_tunnel_aaaa: std::sync::atomic::AtomicBool::new(true),
        })
    }

    /// Configure whether AAAA answers are withheld from tunnel-routed lookups.
    pub fn set_suppress_tunnel_aaaa(&self, suppress: bool) {
        self.suppress_tunnel_aaaa
            .store(suppress, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether AAAA answers are withheld from tunnel-routed lookups.
    pub fn suppresses_tunnel_aaaa(&self) -> bool {
        self.suppress_tunnel_aaaa
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Local port of the direct-path socket (loop guard for `handle_udp`).
    pub fn local_port(&self) -> u16 {
        self.local_socket
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(0)
    }

    /// Install (or clear) the shared tunnel-side flow sender.
    pub fn set_tunnel_sender(&self, tx: Option<UnboundedSender<Vec<u8>>>) {
        *self.tunnel_out.write().unwrap_or_else(|e| e.into_inner()) = tx;
    }

    /// Whether a tunnel-side flow is currently usable.
    pub fn has_tunnel_flow(&self) -> bool {
        self.tunnel_out
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Record an in-flight query before it is sent.
    ///
    /// Registering first (and sending second) matters for the tunnel path: the
    /// first datagram may ride the flow-setup SYN, and its answer can arrive
    /// before the caller would otherwise have registered the transaction.
    pub async fn register(
        &self,
        payload: &[u8],
        ctx: DnsQueryContext,
        route: DnsRoute,
    ) -> Result<u16> {
        let header = DnsHeader::decode(payload)
            .ok_or_else(|| PhantomError::Protocol("Malformed DNS query".to_string()))?;
        let id = header.id;

        if let Some((domain, _)) = extract_query_domain(payload) {
            self.query_domains.lock().await.insert(id, domain);
        }
        self.pending.lock().await.insert(id, ctx);
        self.query_routes.lock().await.insert(id, route);
        Ok(id)
    }

    /// Move an already-registered query onto another transport.
    pub async fn reroute(&self, id: u16, route: DnsRoute) {
        if let Some(entry) = self.query_routes.lock().await.get_mut(&id) {
            *entry = route;
        }
    }

    /// Send an already-registered query on the given transport.
    pub async fn send(&self, payload: &[u8], route: DnsRoute) -> Result<()> {
        match route {
            DnsRoute::Tunnel => {
                let sender = self
                    .tunnel_out
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let sender = sender.ok_or_else(|| {
                    PhantomError::Config("No tunnel DNS flow established".to_string())
                })?;
                sender
                    .send(payload.to_vec())
                    .map_err(|_| PhantomError::Config("Tunnel DNS flow closed".to_string()))?;
            }
            DnsRoute::Local => {
                self.local_socket
                    .send_to(payload, self.direct_upstream())
                    .await
                    .map_err(PhantomError::Io)?;
            }
        }
        Ok(())
    }

    /// Register and send in one step.
    pub async fn forward(
        &self,
        payload: &[u8],
        ctx: DnsQueryContext,
        route: DnsRoute,
    ) -> Result<u16> {
        let id = self.register(payload, ctx, route).await?;
        self.send(payload, route).await?;
        Ok(id)
    }

    /// Resolve a response payload against the pending table.
    async fn take_pending(
        &self,
        payload: &[u8],
    ) -> Option<(DnsQueryContext, Option<String>, DnsRoute)> {
        let header = DnsHeader::decode(payload)?;
        let id = header.id;
        let ctx = self.pending.lock().await.remove(&id)?;
        let domain = self.query_domains.lock().await.remove(&id);
        let route = self
            .query_routes
            .lock()
            .await
            .remove(&id)
            .unwrap_or(DnsRoute::Local);
        Some((ctx, domain, route))
    }

    /// Run the direct-path response loop: replies from `local_upstream` arrive
    /// on the local socket and are handed to `on_response`.
    pub async fn run_local<F, Fut>(&self, mut on_response: F) -> Result<()>
    where
        F: FnMut(Bytes, DnsQueryContext, Option<String>, DnsRoute) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let mut buf = BytesMut::with_capacity(4096);
        loop {
            buf.clear();
            buf.resize(4096, 0);
            let (n, _peer) = self
                .local_socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| PhantomError::Io(e))?;
            let payload = buf[..n].to_vec();
            self.dispatch_response(payload, &mut on_response).await;
        }
    }

    /// Handle one response received over the tunnel-side flow.
    pub async fn handle_tunnel_response<F, Fut>(&self, payload: Vec<u8>, on_response: &mut F)
    where
        F: FnMut(Bytes, DnsQueryContext, Option<String>, DnsRoute) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        self.dispatch_response(payload, on_response).await;
    }

    async fn dispatch_response<F, Fut>(&self, payload: Vec<u8>, on_response: &mut F)
    where
        F: FnMut(Bytes, DnsQueryContext, Option<String>, DnsRoute) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        if let Some((ctx, domain, route)) = self.take_pending(&payload).await {
            if let Err(e) = on_response(Bytes::from(payload), ctx, domain, route).await {
                tracing::debug!("DNS response callback error: {}", e);
            }
        }
    }

    /// Resolver used through the tunnel (`client.dns`).
    pub fn upstream(&self) -> SocketAddr {
        *self
            .tunnel_upstream
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Resolver used over the physical network (`client.dns_direct`).
    pub fn direct_upstream(&self) -> SocketAddr {
        *self
            .local_upstream
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Retarget the tunnel resolver. Returns `true` when it actually changed.
    ///
    /// In-flight queries keep their pending entries, so responses that arrive
    /// from the previous upstream after the swap are still delivered. The
    pub fn set_upstream(&self, addr: SocketAddr) -> bool {
        let mut guard = self
            .tunnel_upstream
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if *guard == addr {
            return false;
        }
        tracing::info!("DNS upstream changed: {} -> {}", *guard, addr);
        *guard = addr;
        // The existing flow is bound to the old resolver; drop it so the next
        // tunnelled query establishes a fresh one.
        self.set_tunnel_sender(None);
        true
    }

    /// Retarget the direct resolver. Returns `true` when it actually changed.
    pub fn set_direct_upstream(&self, addr: SocketAddr) -> bool {
        let mut guard = self
            .local_upstream
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if *guard == addr {
            return false;
        }
        tracing::info!("DNS direct upstream changed: {} -> {}", *guard, addr);
        *guard = addr;
        true
    }
}

// ---------------------------------------------------------------------------
// DNS Cache: IP -> domain mapping extracted from A-record responses.
// ---------------------------------------------------------------------------

/// How many IP→domain mappings the reverse-lookup cache may hold.
///
/// Every DNS answer the tunnel sees used to add a permanent entry, so a device
/// that browses for a day accumulated tens of thousands of them. The cap bounds
/// that; eviction is least-recently-touched.
const DNS_CACHE_MAX_ENTRIES: usize = 4096;

/// How long a mapping stays usable after it was last touched.
///
/// This cache is not a resolver cache — it turns a destination IP back into the
/// domain the app resolved, which is what the whitelist matches on. Expiring an
/// entry early would silently change a routing decision (a domain that used to
/// be tunnelled starts being judged by its bare IP), so the retention floor is
/// deliberately generous: half an hour, longer than any A-record TTL we are
/// likely to see.
const DNS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// One reverse-lookup entry, with the bookkeeping LRU needs.
#[derive(Debug, Clone)]
struct DnsCacheEntry {
    domain: String,
    /// Last time the entry was written *or* matched. Drives both the TTL and
    /// the eviction order.
    touched_at: std::time::Instant,
}

#[derive(Debug, Clone, Default)]
pub struct DnsCache {
    /// Keyed by `IpAddr`, not `Ipv4Addr`: on a dual-stack mobile network the
    /// app connects to the AAAA it was handed, and a v4-only map turned every
    /// IPv6 destination into "no domain known" — which the router then treated
    /// as an unlisted destination and sent out over the physical link.
    inner: Arc<Mutex<HashMap<IpAddr, DnsCacheEntry>>>,
}

impl DnsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, ip: IpAddr, domain: String) {
        let mut map = self.inner.lock().await;
        if map.len() >= DNS_CACHE_MAX_ENTRIES && !map.contains_key(&ip) {
            // Evict the least recently touched entry: the one an active route is
            // least likely to need again.
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, entry)| entry.touched_at)
                .map(|(ip, _)| *ip)
            {
                map.remove(&oldest);
            }
        }
        map.insert(
            ip,
            DnsCacheEntry {
                domain,
                touched_at: std::time::Instant::now(),
            },
        );
    }

    /// Reverse-lookup an address, renewing its lease on a hit.
    ///
    /// A hit means this address is still being routed through its domain, which
    /// is exactly the entry worth keeping; renewing it also keeps it ahead of
    /// the eviction order.
    pub async fn lookup(&self, ip: IpAddr) -> Option<String> {
        let mut map = self.inner.lock().await;
        let expired = match map.get(&ip) {
            Some(entry) => entry.touched_at.elapsed() > DNS_CACHE_TTL,
            None => return None,
        };
        if expired {
            map.remove(&ip);
            return None;
        }
        let domain = map.get_mut(&ip)?;
        domain.touched_at = std::time::Instant::now();
        Some(domain.domain.clone())
    }

    /// Number of live entries (tests and metrics).
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// True once nothing is cached.
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }
}

/// Extract A (1) and AAAA (28) record addresses from a DNS response.
///
/// Both families matter on a dual-stack path: the app may well connect to the
/// AAAA it was given first, and the reverse lookup that drives the routing
/// decision has to know which domain that address belongs to.
pub fn extract_addr_records(buf: &[u8]) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    let header = match DnsHeader::decode(buf) {
        Some(h) => h,
        None => return ips,
    };
    if header.answer_rrs == 0 {
        return ips;
    }

    let mut offset = 12;
    // Skip question section(s).
    for _ in 0..header.questions {
        if offset >= buf.len() {
            return ips;
        }
        // Skip name.
        loop {
            if offset >= buf.len() {
                return ips;
            }
            let len = buf[offset] as usize;
            if len == 0 {
                offset += 1;
                break;
            }
            if len & 0xC0 == 0xC0 {
                offset += 2;
                break;
            }
            offset += 1 + len;
        }
        // Skip QTYPE + QCLASS.
        if offset + 4 > buf.len() {
            return ips;
        }
        offset += 4;
    }

    // Parse answer RRs.
    for _ in 0..header.answer_rrs {
        if offset >= buf.len() {
            break;
        }
        // Skip name (compression pointer or label sequence).
        if buf[offset] & 0xC0 == 0xC0 {
            offset += 2;
        } else {
            loop {
                if offset >= buf.len() {
                    return ips;
                }
                let len = buf[offset] as usize;
                offset += 1;
                if len == 0 {
                    break;
                }
                if len & 0xC0 == 0xC0 {
                    offset += 1;
                    break;
                }
                offset += len;
            }
        }
        if offset + 10 > buf.len() {
            break;
        }
        let rtype = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
        let rclass = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]);
        let rdlength = u16::from_be_bytes([buf[offset + 8], buf[offset + 9]]) as usize;
        offset += 10;
        if rtype == 1 && rclass == 1 && rdlength == 4 && offset + 4 <= buf.len() {
            ips.push(IpAddr::V4(Ipv4Addr::new(
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            )));
        } else if rtype == 28 && rclass == 1 && rdlength == 16 && offset + 16 <= buf.len() {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[offset..offset + 16]);
            ips.push(IpAddr::V6(Ipv6Addr::from(octets)));
        }
        offset += rdlength;
    }

    ips
}

/// IPv4 addresses only, in the order they appear.
///
/// The kernel-split ipset and the gateway plumbing are IPv4-only, so they take
/// this view of the same parse.
pub fn extract_a_records(buf: &[u8]) -> Vec<Ipv4Addr> {
    extract_addr_records(buf)
        .into_iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers for TUN packet construction
// ---------------------------------------------------------------------------

/// Build a raw UDP packet (IPv4 or IPv6, matching the querier) containing
/// `payload`, destined back to the original querier.
///
/// `payload` should be the raw DNS response bytes. The family is taken from
/// the query's addresses, so a query that arrived over IPv6 is answered over
/// IPv6 — answering it over IPv4 would produce a packet the app never sees.
pub fn build_dns_response_packet(payload: &[u8], ctx: &DnsQueryContext) -> Result<Vec<u8>> {
    use etherparse::PacketBuilder;

    match (ctx.dst_ip, ctx.src_ip) {
        (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) => {
            let builder = PacketBuilder::ipv4(src_ip.octets(), dst_ip.octets(), 64)
                .udp(ctx.dst_port, ctx.src_port);
            let mut pkt = Vec::with_capacity(20 + 8 + payload.len());
            builder
                .write(&mut pkt, payload)
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
            Ok(pkt)
        }
        (IpAddr::V6(src_ip), IpAddr::V6(dst_ip)) => {
            let builder = PacketBuilder::ipv6(src_ip.octets(), dst_ip.octets(), 64)
                .udp(ctx.dst_port, ctx.src_port);
            let mut pkt = Vec::with_capacity(40 + 8 + payload.len());
            builder
                .write(&mut pkt, payload)
                .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
            Ok(pkt)
        }
        // A v4 query answered from a v6 address (or vice versa) is a bug in
        // the caller, not something to paper over: say so instead of building
        // a packet the app cannot accept.
        (src, dst) => Err(PhantomError::Protocol(format!(
            "DNS query family mismatch: src {src} dst {dst}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal DNS response with one AAAA answer, built by hand so the parser
    /// tests do not depend on a resolver.
    fn aaaa_response(domain: &str, addr: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // id
        out.extend_from_slice(&0x8180u16.to_be_bytes()); // QR, RD, RA, RCODE=0
        out.extend_from_slice(&1u16.to_be_bytes()); // questions
        out.extend_from_slice(&1u16.to_be_bytes()); // answers
        out.extend_from_slice(&0u16.to_be_bytes()); // authority
        out.extend_from_slice(&0u16.to_be_bytes()); // additional
        for label in domain.split('.') {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
        out.extend_from_slice(&28u16.to_be_bytes()); // QTYPE = AAAA
        out.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
        out.extend_from_slice(&[0xC0, 0x0C]); // name -> question
        out.extend_from_slice(&28u16.to_be_bytes()); // TYPE = AAAA
        out.extend_from_slice(&1u16.to_be_bytes()); // CLASS = IN
        out.extend_from_slice(&300u32.to_be_bytes()); // TTL
        out.extend_from_slice(&16u16.to_be_bytes()); // RDLENGTH
        out.extend_from_slice(&addr.parse::<Ipv6Addr>().unwrap().octets());
        out
    }

    /// Reverse-lookup round trip, and the empty result a miss produces.
    ///
    /// The miss case is the routing-critical one: an unknown address must come
    /// back as `None` so `whitelist::decide` falls through to IP-based matching
    /// instead of guessing a domain.
    #[tokio::test]
    async fn lookup_round_trips_and_misses_cleanly() {
        let cache = DnsCache::new();
        let ip = IpAddr::V4(Ipv4Addr::new(142, 250, 72, 14));
        assert_eq!(cache.lookup(ip).await, None, "unknown IP must not guess");
        cache.insert(ip, "www.google.com".to_string()).await;
        assert_eq!(cache.lookup(ip).await.as_deref(), Some("www.google.com"));
    }

    /// The IPv6 half of the same contract.
    ///
    /// This is the fix for "Wi-Fi works, cellular does not": the app gets a
    /// real AAAA on a dual-stack mobile network and connects to it, and a
    /// v4-only cache meant that address had no domain — so the whitelist could
    /// not match and the connection was sent out over the physical link.
    #[tokio::test]
    async fn cache_maps_ipv6_addresses_to_their_domain() {
        let cache = DnsCache::new();
        let v6: Ipv6Addr = "2404:6800:4005:827::200e".parse().unwrap();
        assert_eq!(cache.lookup(IpAddr::V6(v6)).await, None);
        cache
            .insert(IpAddr::V6(v6), "ipv6.google.com".to_string())
            .await;
        assert_eq!(
            cache.lookup(IpAddr::V6(v6)).await.as_deref(),
            Some("ipv6.google.com")
        );
        // The families are distinct keys: the v4 address of the same domain is
        // a separate entry, not an accidental hit.
        assert_eq!(cache.lookup(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))).await, None);
    }

    /// The cache must be bounded: a long session fills it, and eviction picks
    /// the entry that has been idle the longest.
    #[tokio::test]
    async fn insert_beyond_capacity_evicts_least_recently_touched() {
        let cache = DnsCache::new();
        for i in 0..DNS_CACHE_MAX_ENTRIES as u32 {
            cache
                .insert(IpAddr::V4(Ipv4Addr::from(i)), format!("host-{i}.example"))
                .await;
        }
        assert_eq!(cache.len().await, DNS_CACHE_MAX_ENTRIES);

        // Touch one entry so it is clearly the most recently used, then force
        // an eviction and make sure *it* survived rather than the oldest.
        let touched = IpAddr::V4(Ipv4Addr::from(0));
        assert_eq!(
            cache.lookup(touched).await.as_deref(),
            Some("host-0.example")
        );
        cache
            .insert(
                IpAddr::V4(Ipv4Addr::from(DNS_CACHE_MAX_ENTRIES as u32)),
                "newest.example".to_string(),
            )
            .await;
        assert_eq!(cache.len().await, DNS_CACHE_MAX_ENTRIES);
        assert_eq!(
            cache.lookup(touched).await.as_deref(),
            Some("host-0.example"),
            "a just-touched entry must not be the one evicted"
        );
    }

    /// A hit renews the lease, so an entry in active use is not pushed out by
    /// newer — but untouched — ones.
    #[tokio::test]
    async fn lookup_renews_the_lease() {
        let cache = DnsCache::new();
        let first = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let second = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        cache.insert(first, "a.example".to_string()).await;
        cache.insert(second, "b.example".to_string()).await;
        // `first` is now older than `second`; touching it must make it survive a
        // single-entry eviction instead.
        assert_eq!(cache.lookup(first).await.as_deref(), Some("a.example"));
        cache
            .insert(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)), "c.example".to_string())
            .await;
        assert_eq!(cache.len().await, 3, "nothing evicted below the cap");
        assert_eq!(cache.lookup(second).await.as_deref(), Some("b.example"));
    }

    /// An AAAA answer must be parsed and mapped, and the IPv4-only view must
    /// keep ignoring it (the kernel-split ipset cannot hold v6 addresses).
    #[test]
    fn addr_extraction_reads_both_families_and_a_records_stay_v4() {
        let response = aaaa_response("ipv6.google.com", "2404:6800:4005:827::200e");
        let all = extract_addr_records(&response);
        assert_eq!(all.len(), 1);
        assert_eq!(
            all[0],
            IpAddr::V6("2404:6800:4005:827::200e".parse().unwrap())
        );
        assert!(extract_a_records(&response).is_empty());
    }

    /// QTYPE extraction has to account for the question's name length, and it
    /// is what lets the hijack tell an AAAA query apart from everything else.
    #[test]
    fn query_type_reads_the_question() {
        let response = aaaa_response("ipv6.google.com", "2404:6800:4005:827::200e");
        assert_eq!(extract_query_domain(&response).map(|(d, _)| d).as_deref(), Some("ipv6.google.com"));

        // The built packet ends with the AAAA rdata; its QTYPE is four bytes
        // before the question's end offset.
        let (_, question_end) = extract_query_domain(&response).expect("question");
        assert_eq!(dns_query_type(&response), Some(DNS_TYPE_AAAA));

        let mut as_a = response.clone();
        as_a[question_end - 4..question_end - 2].copy_from_slice(&1u16.to_be_bytes());
        assert_eq!(dns_query_type(&as_a), Some(1));
    }

    /// A withheld AAAA is NOERROR with no answers, not an error.
    ///
    /// Resolvers and apps treat NODATA as "this name has no such record" and go
    /// on to use the A record; an error code makes some of them give up on the
    /// name entirely.
    #[test]
    fn nodata_response_mirrors_the_question_without_answers() {
        let query = aaaa_response("ipv6.google.com", "2404:6800:4005:827::200e");
        let reply = build_nodata_response(&query).expect("nodata reply");
        let header = DnsHeader::decode(&reply).expect("header");
        let question_len = extract_query_domain(&query).expect("question").1;

        assert_eq!(header.answer_rrs, 0, "NODATA carries no answers");
        assert_eq!(header.flags & 0x8000, 0x8000, "QR set");
        assert_eq!(header.flags & 0x000F, 0, "RCODE = NOERROR");
        // The header itself is meant to differ (QR/RA set, ANCOUNT cleared);
        // the question section has to come through untouched.
        assert_eq!(
            &reply[12..question_len],
            &query[12..question_len],
            "question section echoed"
        );
    }

    /// A DNS answer that came in over IPv6 is answered over IPv6.
    ///
    /// Building the reply as an IPv4 packet (the old behaviour) produced
    /// something the app never saw, so a query sent to a v6 resolver looked
    /// like a silent drop.
    #[test]
    fn dns_response_packet_round_trips_over_ipv6() {
        let ctx = DnsQueryContext {
            src_ip: "fd00:8:8::2".parse().unwrap(),
            src_port: 41000,
            dst_ip: "2001:4860:4860::8888".parse().unwrap(),
            dst_port: 53,
        };
        let payload = b"\x00\x01\x81\x80";
        let pkt = build_dns_response_packet(payload, &ctx).expect("v6 response packet");
        let parsed = etherparse::Ipv6HeaderSlice::from_slice(&pkt).expect("ipv6 header");
        assert_eq!(IpAddr::V6(parsed.source_addr()), ctx.dst_ip);
        assert_eq!(IpAddr::V6(parsed.destination_addr()), ctx.src_ip);
    }

    /// Mixed families are a caller bug and must be reported, not guessed at.
    #[test]
    fn dns_response_packet_rejects_mixed_families() {
        let ctx = DnsQueryContext {
            src_ip: "10.8.0.2".parse().unwrap(),
            src_port: 41000,
            dst_ip: "2001:4860:4860::8888".parse().unwrap(),
            dst_port: 53,
        };
        assert!(build_dns_response_packet(b"x", &ctx).is_err());
    }

    #[test]
    fn parse_dns_addr_accepts_tls_prefix() {
        assert_eq!(
            parse_dns_addr("tls://8.8.8.8:853"),
            Some("8.8.8.8:853".parse().unwrap())
        );
        assert_eq!(
            parse_dns_addr("https://1.1.1.1:443"),
            Some("1.1.1.1:443".parse().unwrap())
        );
    }

    #[test]
    fn parse_dns_addr_defaults_to_port_53() {
        assert_eq!(
            parse_dns_addr("1.1.1.1"),
            Some("1.1.1.1:53".parse().unwrap())
        );
    }

    #[test]
    fn parse_dns_addr_rejects_garbage() {
        assert_eq!(parse_dns_addr("not-an-address"), None);
    }

    #[tokio::test]
    async fn set_upstream_swaps_resolver_once() {
        let proxy = DnsProxy::new(
            "8.8.8.8:53".parse().unwrap(),
            "223.5.5.5:53".parse().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(proxy.upstream(), "8.8.8.8:53".parse().unwrap());
        assert_eq!(proxy.direct_upstream(), "223.5.5.5:53".parse().unwrap());

        assert!(proxy.set_upstream("1.1.1.1:53".parse().unwrap()));
        assert_eq!(proxy.upstream(), "1.1.1.1:53".parse().unwrap());
        // Re-applying the same address is reported as "no change" so the
        // hot-reload watcher stays quiet on unrelated config edits.
        assert!(!proxy.set_upstream("1.1.1.1:53".parse().unwrap()));

        // The direct resolver is retargeted independently.
        assert!(proxy.set_direct_upstream("119.29.29.29:53".parse().unwrap()));
        assert_eq!(proxy.direct_upstream(), "119.29.29.29:53".parse().unwrap());
        assert!(!proxy.set_direct_upstream("119.29.29.29:53".parse().unwrap()));
        assert_eq!(proxy.upstream(), "1.1.1.1:53".parse().unwrap());
    }

    #[test]
    fn refused_response_mirrors_question() {
        let query = [
            0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, b'e',
            b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, 0x00, 0x01, 0x00,
            0x01,
        ];
        let response = build_refused_response(&query).unwrap();
        let header = DnsHeader::decode(&response).unwrap();
        assert_eq!(header.id, 0x1234);
        assert_eq!(header.flags & 0x8000, 0x8000, "QR bit set");
        assert_eq!(header.flags & 0x000F, 0x5, "RCODE=REFUSED");
        assert_eq!(header.answer_rrs, 0);
        // Question section is preserved so the client can match the reply.
        assert_eq!(&response[12..], &query[12..]);
    }

    #[test]
    fn decode_dns_header() {
        let raw = [
            0x12, 0x34, // ID
            0x01, 0x00, // flags
            0x00, 0x01, // questions
            0x00, 0x00, // answers
            0x00, 0x00, // authority
            0x00, 0x00, // additional
        ];
        let h = DnsHeader::decode(&raw).unwrap();
        assert_eq!(h.id, 0x1234);
        assert_eq!(h.questions, 1);
    }

    #[test]
    fn extract_domain_simple() {
        // DNS query for "example.com"
        let mut raw = vec![
            0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        // example.com labels
        raw.push(7);
        raw.extend_from_slice(b"example");
        raw.push(3);
        raw.extend_from_slice(b"com");
        raw.push(0);
        // QTYPE A, QCLASS IN
        raw.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);

        let (domain, consumed) = extract_query_domain(&raw).unwrap();
        assert_eq!(domain, "example.com");
        assert_eq!(consumed, 12 + 1 + 7 + 1 + 3 + 1 + 4);
    }

    /// Build a minimal A query for `domain` with the given transaction ID.
    fn a_query(id: u16, domain: &str) -> Vec<u8> {
        let mut raw = vec![
            (id >> 8) as u8,
            id as u8,
            0x01,
            0x00,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ];
        for label in domain.split('.') {
            raw.push(label.len() as u8);
            raw.extend_from_slice(label.as_bytes());
        }
        raw.push(0);
        raw.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        raw
    }

    fn query_ctx() -> DnsQueryContext {
        DnsQueryContext {
            src_ip: "10.8.0.2".parse().unwrap(),
            src_port: 40000,
            dst_ip: "8.8.8.8".parse().unwrap(),
            dst_port: 53,
        }
    }

    /// A directly-routed query must be answered by the physical-network
    /// resolver and come back tagged as `Local`.
    #[tokio::test]
    async fn local_route_is_answered_by_the_direct_resolver() {
        let resolver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let resolver_addr = resolver.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            if let Ok((n, peer)) = resolver.recv_from(&mut buf).await {
                let mut reply = buf[..n].to_vec();
                reply[2] |= 0x80; // QR: this is a response
                let _ = resolver.send_to(&reply, peer).await;
            }
        });

        // The tunnel resolver address is never dialled on this path.
        let proxy = Arc::new(
            DnsProxy::new("127.0.0.1:1".parse().unwrap(), resolver_addr)
                .await
                .unwrap(),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Option<String>, DnsRoute)>();
        let loop_proxy = Arc::clone(&proxy);
        tokio::spawn(async move {
            let mut on_response = move |_payload: Bytes,
                                        _ctx: DnsQueryContext,
                                        domain: Option<String>,
                                        route: DnsRoute| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send((domain, route));
                    Ok(())
                }
            };
            let _ = loop_proxy.run_local(&mut on_response).await;
        });

        let query = a_query(0x4321, "v.youku.com");
        proxy
            .forward(&query, query_ctx(), DnsRoute::Local)
            .await
            .unwrap();

        let (domain, route) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("direct resolver answer timed out")
            .unwrap();
        assert_eq!(domain.as_deref(), Some("v.youku.com"));
        assert_eq!(route, DnsRoute::Local);
    }

    /// Tunnelled queries go out through the installed flow sender, and a
    /// missing flow is reported instead of silently dropping the query.
    #[tokio::test]
    async fn tunnel_route_goes_through_the_udp_flow() {
        let proxy = DnsProxy::new(
            "9.9.9.9:53".parse().unwrap(),
            "127.0.0.1:1".parse().unwrap(),
        )
        .await
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        proxy.set_tunnel_sender(Some(tx));
        assert!(proxy.has_tunnel_flow());

        let query = a_query(0x0042, "www.google.com");
        proxy
            .forward(&query, query_ctx(), DnsRoute::Tunnel)
            .await
            .unwrap();
        let sent = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("query was never handed to the tunnel flow")
            .unwrap();
        assert_eq!(sent, query);

        proxy.set_tunnel_sender(None);
        assert!(!proxy.has_tunnel_flow());
        assert!(
            proxy.send(&query, DnsRoute::Tunnel).await.is_err(),
            "without a flow the caller must be told so it can fall back"
        );
    }
}
