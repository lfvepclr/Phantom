//! DNS proxy for TUN mode.
//!
//! Intercepts UDP:53 queries from the TUN device, forwards them to a
//! configurable upstream DNS server (e.g. 8.8.8.8:53) over a normal UDP
//! socket, and writes the response back into the TUN device.
//!
//! This avoids GFW DNS pollution while keeping the implementation simple.
//! Future iterations can upgrade to DNS-over-TLS or tunnel the UDP packets
//! through Phantom's encrypted stream.

use bytes::{Bytes, BytesMut};
use phantom_core::{PhantomError, Result};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

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

/// Shared DNS proxy state.
pub struct DnsProxy {
    socket: Arc<UdpSocket>,
    upstream: SocketAddr,
    /// Pending queries: DNS transaction ID -> original TUN context.
    pending: Arc<Mutex<std::collections::HashMap<u16, DnsQueryContext>>>,
    /// Query domain names tracked so we can populate the DNS cache from responses.
    query_domains: Arc<Mutex<std::collections::HashMap<u16, String>>>,
}

impl DnsProxy {
    /// Create a new DNS proxy bound to a local ephemeral port.
    pub async fn new(upstream_addr: SocketAddr) -> Result<Self> {
        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let socket = UdpSocket::bind(bind_addr)
            .await
            .map_err(|e| PhantomError::Io(e))?;
        tracing::info!(
            "DNS proxy bound to {}, upstream = {}",
            socket.local_addr()?,
            upstream_addr
        );
        Ok(Self {
            socket: Arc::new(socket),
            upstream: upstream_addr,
            pending: Arc::new(Mutex::new(std::collections::HashMap::new())),
            query_domains: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    /// Forward a DNS query (UDP payload) to the upstream server.
    /// Returns the transaction ID so the caller can await the response.
    pub async fn forward(&self, payload: &[u8], ctx: DnsQueryContext) -> Result<u16> {
        let header = DnsHeader::decode(payload)
            .ok_or_else(|| PhantomError::Protocol("Malformed DNS query".to_string()))?;
        let id = header.id;

        if let Some((domain, _)) = extract_query_domain(payload) {
            self.query_domains.lock().await.insert(id, domain);
        }
        self.pending.lock().await.insert(id, ctx);
        self.socket
            .send_to(payload, self.upstream)
            .await
            .map_err(|e| PhantomError::Io(e))?;
        Ok(id)
    }

    /// Run the response-receiver loop.  Whenever an upstream response arrives,
    /// invoke `on_response` with the raw UDP payload + original context + optional domain.
    pub async fn run<F, Fut>(&self, mut on_response: F) -> Result<()>
    where
        F: FnMut(Bytes, DnsQueryContext, Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let mut buf = BytesMut::with_capacity(4096);
        loop {
            buf.clear();
            buf.resize(4096, 0);
            let (n, _peer) = self
                .socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| PhantomError::Io(e))?;
            let payload = buf[..n].to_vec();

            let header = match DnsHeader::decode(&payload) {
                Some(h) => h,
                None => continue,
            };
            let id = header.id;

            let ctx = match self.pending.lock().await.remove(&id) {
                Some(c) => c,
                None => continue,
            };
            let domain = self.query_domains.lock().await.remove(&id);

            if let Err(e) = on_response(Bytes::from(payload), ctx, domain).await {
                tracing::debug!("DNS response callback error: {}", e);
            }
        }
    }

    pub fn upstream(&self) -> SocketAddr {
        self.upstream
    }
}

// ---------------------------------------------------------------------------
// DNS Cache: IP -> domain mapping extracted from A-record responses.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct DnsCache {
    inner: Arc<Mutex<HashMap<Ipv4Addr, String>>>,
}

impl DnsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, ip: Ipv4Addr, domain: String) {
        self.inner.lock().await.insert(ip, domain);
    }

    pub async fn lookup(&self, ip: Ipv4Addr) -> Option<String> {
        self.inner.lock().await.get(&ip).cloned()
    }
}

/// Extract IPv4 addresses from A-record answers in a DNS response.
/// Returns the first A-record IP found (MVP).  Does not follow compression
/// pointers deeply.
pub fn extract_a_records(buf: &[u8]) -> Vec<Ipv4Addr> {
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
            let ip = Ipv4Addr::new(
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            );
            ips.push(ip);
        }
        offset += rdlength;
    }

    ips
}

// ---------------------------------------------------------------------------
// Helpers for TUN packet construction
// ---------------------------------------------------------------------------

/// Build a raw IPv4/UDP packet containing `payload` destined back to the
/// original querier.  `payload` should be the raw DNS response bytes.
pub fn build_dns_response_packet(payload: &[u8], ctx: &DnsQueryContext) -> Result<Vec<u8>> {
    use etherparse::PacketBuilder;

    let src_ip = match ctx.dst_ip {
        IpAddr::V4(v4) => v4,
        _ => {
            return Err(PhantomError::Protocol(
                "IPv6 DNS not yet supported".to_string(),
            ));
        }
    };
    let dst_ip = match ctx.src_ip {
        IpAddr::V4(v4) => v4,
        _ => {
            return Err(PhantomError::Protocol(
                "IPv6 DNS not yet supported".to_string(),
            ));
        }
    };

    let builder =
        PacketBuilder::ipv4(src_ip.octets(), dst_ip.octets(), 64).udp(ctx.dst_port, ctx.src_port);

    let mut pkt = Vec::with_capacity(20 + 8 + payload.len());
    builder
        .write(&mut pkt, payload)
        .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    Ok(pkt)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
