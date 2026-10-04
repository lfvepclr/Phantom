//! Lightweight traffic statistics with atomic counters.
//! Can be exposed as Prometheus metrics via an HTTP endpoint.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct TrafficStats {
    pub tcp_bytes_up: AtomicU64,
    pub tcp_bytes_down: AtomicU64,
    pub udp_bytes_up: AtomicU64,
    pub udp_bytes_down: AtomicU64,
    pub tcp_connections: AtomicU64,
    /// Routing decisions, so "is my traffic really going direct?" is a metric
    /// rather than a guess (`phantom_route_direct_total`).
    pub route_direct: AtomicU64,
    pub route_proxy: AtomicU64,
    pub udp_datagrams_up: AtomicU64,
    pub udp_datagrams_down: AtomicU64,

    // ---- TUN path health -------------------------------------------------
    //
    // Only meaningful in TUN mode (the SOCKS5 path never writes IP packets),
    // and the reason they exist: a stalled video stream and a healthy one look
    // identical in `down`, because a wedged flow "transfers" a lot of bytes —
    // all of them retransmissions.
    /// Bytes injected into the app a second time (retransmissions).
    pub tcp_dup_bytes: AtomicU64,
    /// Duplicate-ACK events observed (each one is a "the app is missing
    /// something" signal).
    pub dup_acks: AtomicU64,
    /// Cumulative time spent waiting for the TUN device to accept a write.
    pub tun_write_wait_ms: AtomicU64,
    /// Worst single wait for TUN writability, in milliseconds.
    ///
    /// This is the number that explains "the tunnel is fine but the app is
    /// stalled": while the reader held the device, every ACK queued behind it.
    pub tun_write_wait_max_ms: AtomicU64,
    /// Peak depth of the TUN write queue, in bytes.
    pub tun_txq_peak: AtomicU64,
    /// Retransmissions suppressed by the guard/budget logic.
    pub retransmit_suppressed: AtomicU64,
    /// Flows killed because they exceeded the duplicate-injection budget.
    pub retransmit_budget_rst: AtomicU64,
    /// Direct connections that failed or timed out and had to be retried
    /// through the tunnel. Each one cost the app `DIRECT_FALLBACK_TIMEOUT`.
    pub route_direct_failed: AtomicU64,
    /// Network-epoch bumps. Every bump invalidates in-flight flows, so on a
    /// lossy mobile link this counter is the difference between "the link was
    /// bad" and "we kept tearing the link down ourselves".
    pub net_epoch_bumps: AtomicU64,
    /// Flows given up on after a stall (no acknowledgement progress).
    ///
    /// Distinct from `retransmit_budget_rst`: a stall give-up is the client
    /// telling the app "start over", which is only acceptable when the path is
    /// genuinely dead. A flutter of these means the tolerance window is wrong.
    pub flow_stall_drops: AtomicU64,
    /// Outer tunnel connections opened (TCP or QUIC).
    pub tunnel_connects: AtomicU64,
    /// Outer tunnel connects that never produced a usable session.
    pub tunnel_connect_failures: AtomicU64,

    // ---- Direct vs tunnel byte split -------------------------------------
    //
    // "Is my non-VPN traffic being encrypted?" is a question that deserves a
    // number rather than an opinion. Direct bytes ride a plain `TcpStream` to
    // the destination; only the tunnel counters are AEAD-protected. Comparing
    // the two against the server's own accounting is the proof.
    /// Bytes the LAN sent to a destination over a **plaintext** direct socket.
    pub direct_bytes_up: AtomicU64,
    pub direct_bytes_down: AtomicU64,
    /// Bytes relayed through the encrypted tunnel.
    pub tunnel_bytes_up: AtomicU64,
    pub tunnel_bytes_down: AtomicU64,

    // ---- Kernel-split gateway --------------------------------------------
    /// How many entries the whitelist ipset currently holds.
    pub whitelist_ipset_entries: AtomicU64,
    /// 1 when the gateway marks only whitelisted destinations into the TUN
    /// (kernel-split), 0 when every LAN packet is relayed in user space.
    pub gateway_kernel_split: AtomicU64,
    /// Packets that reached the TUN for an unknown flow that was not a SYN.
    /// They are answered with RST so the app reconnects at once instead of
    /// waiting out its own retransmit timers.
    pub unknown_flow_rst: AtomicU64,
}

impl TrafficStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Snapshot the counters as a flat JSON object.
    ///
    /// Single source of truth for every UI bridge (Android JNI, HarmonyOS NAPI,
    /// macOS C FFI): the shape is stable and positional parsing is unnecessary,
    /// so a new counter can be added without touching the platform shells.
    /// Unset counters read as `0`, which is what an idle client should report.
    pub fn snapshot_json(&self) -> String {
        format!(
            "{{\"up\":{},\"down\":{},\"udp_up\":{},\"udp_down\":{},\"conns\":{},\"route_direct\":{},\"route_proxy\":{},\
             \"tcp_dup\":{},\"dup_acks\":{},\"tun_wq_ms\":{},\"tun_wq_max_ms\":{},\"tun_txq_peak\":{},\
             \"retx_suppressed\":{},\"retx_budget_rst\":{},\"route_direct_failed\":{},\
             \"net_epoch_bumps\":{},\"flow_stall_drops\":{},\"tunnel_connects\":{},\"tunnel_connect_failures\":{},\
             \"direct_up\":{},\"direct_down\":{},\"tunnel_up\":{},\"tunnel_down\":{},\
             \"ipset_entries\":{},\"kernel_split\":{},\"unknown_flow_rst\":{}}}",
            self.tcp_bytes_up.load(Ordering::Relaxed),
            self.tcp_bytes_down.load(Ordering::Relaxed),
            self.udp_bytes_up.load(Ordering::Relaxed),
            self.udp_bytes_down.load(Ordering::Relaxed),
            self.tcp_connections.load(Ordering::Relaxed),
            self.route_direct.load(Ordering::Relaxed),
            self.route_proxy.load(Ordering::Relaxed),
            self.tcp_dup_bytes.load(Ordering::Relaxed),
            self.dup_acks.load(Ordering::Relaxed),
            self.tun_write_wait_ms.load(Ordering::Relaxed),
            self.tun_write_wait_max_ms.load(Ordering::Relaxed),
            self.tun_txq_peak.load(Ordering::Relaxed),
            self.retransmit_suppressed.load(Ordering::Relaxed),
            self.retransmit_budget_rst.load(Ordering::Relaxed),
            self.route_direct_failed.load(Ordering::Relaxed),
            self.net_epoch_bumps.load(Ordering::Relaxed),
            self.flow_stall_drops.load(Ordering::Relaxed),
            self.tunnel_connects.load(Ordering::Relaxed),
            self.tunnel_connect_failures.load(Ordering::Relaxed),
            self.direct_bytes_up.load(Ordering::Relaxed),
            self.direct_bytes_down.load(Ordering::Relaxed),
            self.tunnel_bytes_up.load(Ordering::Relaxed),
            self.tunnel_bytes_down.load(Ordering::Relaxed),
            self.whitelist_ipset_entries.load(Ordering::Relaxed),
            self.gateway_kernel_split.load(Ordering::Relaxed),
            self.unknown_flow_rst.load(Ordering::Relaxed),
        )
    }

    /// The same JSON shape with every counter at zero.
    ///
    /// Used before the first successful start, when no `TrafficStats` instance
    /// exists yet — the UI still wants a well-formed document to parse.
    pub fn zero_snapshot_json() -> String {
        "{\"up\":0,\"down\":0,\"udp_up\":0,\"udp_down\":0,\"conns\":0,\"route_direct\":0,\"route_proxy\":0,\
         \"tcp_dup\":0,\"dup_acks\":0,\"tun_wq_ms\":0,\"tun_wq_max_ms\":0,\"tun_txq_peak\":0,\
         \"retx_suppressed\":0,\"retx_budget_rst\":0,\"route_direct_failed\":0,\
         \"net_epoch_bumps\":0,\"flow_stall_drops\":0,\"tunnel_connects\":0,\"tunnel_connect_failures\":0,\
         \"direct_up\":0,\"direct_down\":0,\"tunnel_up\":0,\"tunnel_down\":0,\
         \"ipset_entries\":0,\"kernel_split\":0,\"unknown_flow_rst\":0}"
            .to_string()
    }

    pub fn record_tcp_up(&self, bytes: u64) {
        self.tcp_bytes_up.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_tcp_down(&self, bytes: u64) {
        self.tcp_bytes_down.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_tcp_connect(&self) {
        self.tcp_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_route_direct(&self) {
        self.route_direct.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_route_proxy(&self) {
        self.route_proxy.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_udp_up(&self, bytes: u64) {
        self.udp_bytes_up.fetch_add(bytes, Ordering::Relaxed);
        self.udp_datagrams_up.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_udp_down(&self, bytes: u64) {
        self.udp_bytes_down.fetch_add(bytes, Ordering::Relaxed);
        self.udp_datagrams_down.fetch_add(1, Ordering::Relaxed);
    }

    /// Payload written into the app a second (or third…) time.
    pub fn record_tcp_dup(&self, bytes: u64) {
        self.tcp_dup_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// One duplicate ACK seen from the app.
    pub fn record_dup_ack(&self) {
        self.dup_acks.fetch_add(1, Ordering::Relaxed);
    }

    /// Time a packet spent waiting for the TUN write side.
    pub fn record_tun_write_wait(&self, millis: u64) {
        self.tun_write_wait_ms.fetch_add(millis, Ordering::Relaxed);
        self.tun_write_wait_max_ms
            .fetch_max(millis, Ordering::Relaxed);
    }

    /// Observe the current TUN write-queue depth.
    pub fn record_tun_queue_depth(&self, bytes: u64) {
        self.tun_txq_peak.fetch_max(bytes, Ordering::Relaxed);
    }

    pub fn record_retransmit_suppressed(&self, count: u64) {
        self.retransmit_suppressed.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_retransmit_budget_rst(&self) {
        self.retransmit_budget_rst.fetch_add(1, Ordering::Relaxed);
    }

    /// A direct connection attempt failed (censored destinations blackhole
    /// instead of refusing, so these show up as timeouts).
    pub fn record_route_direct_failed(&self) {
        self.route_direct_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// One network-epoch bump: every in-flight flow was invalidated.
    pub fn record_net_epoch_bump(&self) {
        self.net_epoch_bumps.fetch_add(1, Ordering::Relaxed);
    }

    /// One flow given up on after a stall (RST sent to the app).
    pub fn record_flow_stall_drop(&self) {
        self.flow_stall_drops.fetch_add(1, Ordering::Relaxed);
    }

    /// One outer tunnel connection attempt (TCP or QUIC).
    pub fn record_tunnel_connect(&self) {
        self.tunnel_connects.fetch_add(1, Ordering::Relaxed);
    }

    /// One outer tunnel connect that failed before a session existed.
    pub fn record_tunnel_connect_failure(&self) {
        self.tunnel_connect_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_direct_up(&self, bytes: u64) {
        self.direct_bytes_up.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_direct_down(&self, bytes: u64) {
        self.direct_bytes_down.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_tunnel_up(&self, bytes: u64) {
        self.tunnel_bytes_up.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_tunnel_down(&self, bytes: u64) {
        self.tunnel_bytes_down.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn set_whitelist_ipset_entries(&self, n: u64) {
        self.whitelist_ipset_entries.store(n, Ordering::Relaxed);
    }

    pub fn set_gateway_kernel_split(&self, on: bool) {
        self.gateway_kernel_split
            .store(u64::from(on), Ordering::Relaxed);
    }

    pub fn record_unknown_flow_rst(&self) {
        self.unknown_flow_rst.fetch_add(1, Ordering::Relaxed);
    }

    /// Render stats in Prometheus exposition format.
    ///
    /// The TUN-health counters are included on purpose: "the tunnel is up but
    /// video stalls" is invisible in the byte counters alone (a wedged flow
    /// still transfers megabytes — all of them retransmissions), and a router
    /// has no UI to fall back on.
    pub fn render_prometheus(&self) -> String {
        format!(
            "# HELP phantom_tcp_bytes_up Total TCP bytes sent upstream\n# TYPE phantom_tcp_bytes_up counter\nphantom_tcp_bytes_up {}\n\
             # HELP phantom_tcp_bytes_down Total TCP bytes received downstream\n# TYPE phantom_tcp_bytes_down counter\nphantom_tcp_bytes_down {}\n\
             # HELP phantom_udp_bytes_up Total UDP bytes sent upstream\n# TYPE phantom_udp_bytes_up counter\nphantom_udp_bytes_up {}\n\
             # HELP phantom_udp_bytes_down Total UDP bytes received downstream\n# TYPE phantom_udp_bytes_down counter\nphantom_udp_bytes_down {}\n\
             # HELP phantom_tcp_connections Total TCP connections\n# TYPE phantom_tcp_connections counter\nphantom_tcp_connections {}\n\
             # HELP phantom_route_direct_total Connections routed directly (bypassing the tunnel)\n# TYPE phantom_route_direct_total counter\nphantom_route_direct_total {}\n\
             # HELP phantom_route_proxy_total Connections routed through the tunnel\n# TYPE phantom_route_proxy_total counter\nphantom_route_proxy_total {}\n\
             # HELP phantom_udp_datagrams_up Total UDP datagrams sent upstream\n# TYPE phantom_udp_datagrams_up counter\nphantom_udp_datagrams_up {}\n\
             # HELP phantom_udp_datagrams_down Total UDP datagrams received downstream\n# TYPE phantom_udp_datagrams_down counter\nphantom_udp_datagrams_down {}\n\
             # HELP phantom_tcp_dup_bytes Retransmitted payload written into the app\n# TYPE phantom_tcp_dup_bytes counter\nphantom_tcp_dup_bytes {}\n\
             # HELP phantom_dup_acks_total Duplicate ACKs seen from the app\n# TYPE phantom_dup_acks_total counter\nphantom_dup_acks_total {}\n\
             # HELP phantom_tun_write_wait_ms_total Cumulative wait for TUN writability\n# TYPE phantom_tun_write_wait_ms_total counter\nphantom_tun_write_wait_ms_total {}\n\
             # HELP phantom_tun_write_wait_max_ms Worst single wait for TUN writability\n# TYPE phantom_tun_write_wait_max_ms gauge\nphantom_tun_write_wait_max_ms {}\n\
             # HELP phantom_tun_txq_peak_bytes Peak TUN write-queue depth\n# TYPE phantom_tun_txq_peak_bytes gauge\nphantom_tun_txq_peak_bytes {}\n\
             # HELP phantom_retransmit_suppressed_total Retransmissions suppressed by the budget guard\n# TYPE phantom_retransmit_suppressed_total counter\nphantom_retransmit_suppressed_total {}\n\
             # HELP phantom_retransmit_budget_rst_total Flows reset for exceeding the duplicate-injection budget\n# TYPE phantom_retransmit_budget_rst_total counter\nphantom_retransmit_budget_rst_total {}\n\
             # HELP phantom_route_direct_failed_total Direct connects that timed out and fell back to the tunnel\n# TYPE phantom_route_direct_failed_total counter\nphantom_route_direct_failed_total {}\n\
             # HELP phantom_net_epoch_bumps_total Network changes that invalidated every in-flight flow\n# TYPE phantom_net_epoch_bumps_total counter\nphantom_net_epoch_bumps_total {}\n\
             # HELP phantom_flow_stall_drops_total Flows given up on after a stall (no acknowledgement progress)\n# TYPE phantom_flow_stall_drops_total counter\nphantom_flow_stall_drops_total {}\n\
             # HELP phantom_tunnel_connects_total Outer tunnel connections opened\n# TYPE phantom_tunnel_connects_total counter\nphantom_tunnel_connects_total {}\n\
             # HELP phantom_tunnel_connect_failures_total Outer tunnel connects that never produced a session\n# TYPE phantom_tunnel_connect_failures_total counter\nphantom_tunnel_connect_failures_total {}\n\
             # HELP phantom_direct_bytes_up Bytes taken from the LAN and sent to a destination **without** the tunnel (plaintext)\n# TYPE phantom_direct_bytes_up counter\nphantom_direct_bytes_up {}\n\
             # HELP phantom_direct_bytes_down Bytes received from a direct destination (plaintext)\n# TYPE phantom_direct_bytes_down counter\nphantom_direct_bytes_down {}\n\
             # HELP phantom_tunnel_bytes_up Bytes sent into the encrypted tunnel\n# TYPE phantom_tunnel_bytes_up counter\nphantom_tunnel_bytes_up {}\n\
             # HELP phantom_tunnel_bytes_down Bytes received from the encrypted tunnel\n# TYPE phantom_tunnel_bytes_down counter\nphantom_tunnel_bytes_down {}\n\
             # HELP phantom_whitelist_ipset_entries Destinations currently marked into the TUN by the kernel-split gateway\n# TYPE phantom_whitelist_ipset_entries gauge\nphantom_whitelist_ipset_entries {}\n\
             # HELP phantom_gateway_kernel_split 1 when only whitelisted destinations enter the TUN, 0 when every packet is relayed in user space\n# TYPE phantom_gateway_kernel_split gauge\nphantom_gateway_kernel_split {}\n\
             # HELP phantom_tun_unknown_flow_rst_total Non-SYN packets for unknown flows answered with RST\n# TYPE phantom_tun_unknown_flow_rst_total counter\nphantom_tun_unknown_flow_rst_total {}\n",
            self.tcp_bytes_up.load(Ordering::Relaxed),
            self.tcp_bytes_down.load(Ordering::Relaxed),
            self.udp_bytes_up.load(Ordering::Relaxed),
            self.udp_bytes_down.load(Ordering::Relaxed),
            self.tcp_connections.load(Ordering::Relaxed),
            self.route_direct.load(Ordering::Relaxed),
            self.route_proxy.load(Ordering::Relaxed),
            self.udp_datagrams_up.load(Ordering::Relaxed),
            self.udp_datagrams_down.load(Ordering::Relaxed),
            self.tcp_dup_bytes.load(Ordering::Relaxed),
            self.dup_acks.load(Ordering::Relaxed),
            self.tun_write_wait_ms.load(Ordering::Relaxed),
            self.tun_write_wait_max_ms.load(Ordering::Relaxed),
            self.tun_txq_peak.load(Ordering::Relaxed),
            self.retransmit_suppressed.load(Ordering::Relaxed),
            self.retransmit_budget_rst.load(Ordering::Relaxed),
            self.route_direct_failed.load(Ordering::Relaxed),
            self.net_epoch_bumps.load(Ordering::Relaxed),
            self.flow_stall_drops.load(Ordering::Relaxed),
            self.tunnel_connects.load(Ordering::Relaxed),
            self.tunnel_connect_failures.load(Ordering::Relaxed),
            self.direct_bytes_up.load(Ordering::Relaxed),
            self.direct_bytes_down.load(Ordering::Relaxed),
            self.tunnel_bytes_up.load(Ordering::Relaxed),
            self.tunnel_bytes_down.load(Ordering::Relaxed),
            self.whitelist_ipset_entries.load(Ordering::Relaxed),
            self.gateway_kernel_split.load(Ordering::Relaxed),
            self.unknown_flow_rst.load(Ordering::Relaxed),
        )
    }
}

/// Serve Prometheus metrics over HTTP.
///
/// Shared by the SOCKS5-only and TUN runtimes. A bind failure (port already
/// in use, sandbox restrictions) is logged at debug level and the task simply
/// returns: observability must never take down the data plane.
pub async fn serve_metrics(stats: Arc<TrafficStats>, listen: std::net::SocketAddr) {
    use tokio::io::AsyncWriteExt;
    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!("Metrics server bind failed on {}: {}", listen, e);
            return;
        }
    };
    tracing::info!("Metrics endpoint: http://{}/metrics", listen);
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let body = stats.render_prometheus();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_prometheus_format() {
        let stats = TrafficStats::new();
        stats.record_tcp_connect();
        stats.record_tcp_up(100);
        stats.record_tcp_down(200);
        stats.record_udp_up(50);
        let output = stats.render_prometheus();
        assert!(output.contains("phantom_tcp_connections 1"));
        assert!(output.contains("phantom_tcp_bytes_up 100"));
        assert!(output.contains("phantom_tcp_bytes_down 200"));
        assert!(output.contains("phantom_udp_bytes_up 50"));
        assert!(output.contains("# TYPE phantom_tcp_bytes_up counter"));
    }

    /// A wedged flow still moves bytes, so the byte counters alone cannot tell
    /// "healthy" from "stalled". These are the ones a headless router needs.
    #[test]
    fn render_prometheus_exposes_tun_health() {
        let stats = TrafficStats::new();
        stats.record_tcp_dup(1400);
        stats.record_dup_ack();
        stats.record_tun_write_wait(120);
        stats.record_tun_queue_depth(65536);
        stats.record_retransmit_suppressed(3);
        stats.record_retransmit_budget_rst();
        stats.record_route_direct_failed();
        stats.record_net_epoch_bump();
        stats.record_net_epoch_bump();
        stats.record_flow_stall_drop();
        stats.record_tunnel_connect();
        stats.record_tunnel_connect_failure();

        let output = stats.render_prometheus();
        for name in [
            "phantom_tcp_dup_bytes 1400",
            "phantom_dup_acks_total 1",
            "phantom_tun_write_wait_ms_total 120",
            "phantom_tun_write_wait_max_ms 120",
            "phantom_tun_txq_peak_bytes 65536",
            "phantom_retransmit_suppressed_total 3",
            "phantom_retransmit_budget_rst_total 1",
            "phantom_route_direct_failed_total 1",
            "phantom_net_epoch_bumps_total 2",
            "phantom_flow_stall_drops_total 1",
            "phantom_tunnel_connects_total 1",
            "phantom_tunnel_connect_failures_total 1",
        ] {
            assert!(output.contains(name), "missing {name} in:\n{output}");
        }
    }

    #[test]
    fn counters_only_increment() {
        let stats = TrafficStats::new();
        stats.record_tcp_connect();
        stats.record_tcp_connect();
        stats.record_tcp_connect();
        let output = stats.render_prometheus();
        assert!(output.contains("phantom_tcp_connections 3"));
    }

    #[test]
    fn snapshot_json_reports_every_counter() {
        let stats = TrafficStats::new();
        stats.record_tcp_up(1024);
        stats.record_tcp_down(2048);
        stats.record_udp_up(16);
        stats.record_udp_down(32);
        stats.record_tcp_connect();
        stats.record_route_proxy();
        stats.record_route_direct();
        stats.record_route_direct();

        assert_eq!(
            stats.snapshot_json(),
            "{\"up\":1024,\"down\":2048,\"udp_up\":16,\"udp_down\":32,\
             \"conns\":1,\"route_direct\":2,\"route_proxy\":1,\
             \"tcp_dup\":0,\"dup_acks\":0,\"tun_wq_ms\":0,\"tun_wq_max_ms\":0,\"tun_txq_peak\":0,\
             \"retx_suppressed\":0,\"retx_budget_rst\":0,\"route_direct_failed\":0,\
             \"net_epoch_bumps\":0,\"flow_stall_drops\":0,\"tunnel_connects\":0,\"tunnel_connect_failures\":0,\
             \"direct_up\":0,\"direct_down\":0,\"tunnel_up\":0,\"tunnel_down\":0,\
             \"ipset_entries\":0,\"kernel_split\":0,\"unknown_flow_rst\":0}"
        );
    }

    #[test]
    fn path_split_counters_separate_direct_from_tunnel() {
        // The whole point of the split: prove on the device that "direct"
        // traffic is plaintext and never touches the tunnel counters.
        let stats = TrafficStats::new();
        stats.record_direct_up(1_000);
        stats.record_direct_down(9_000);
        stats.record_tunnel_up(500);
        stats.record_tunnel_down(700);
        stats.set_whitelist_ipset_entries(142);
        stats.set_gateway_kernel_split(true);

        let out = stats.render_prometheus();
        for line in [
            "phantom_direct_bytes_up 1000",
            "phantom_direct_bytes_down 9000",
            "phantom_tunnel_bytes_up 500",
            "phantom_tunnel_bytes_down 700",
            "phantom_whitelist_ipset_entries 142",
            "phantom_gateway_kernel_split 1",
        ] {
            assert!(out.contains(line), "missing `{line}` in:\n{out}");
        }
        let json = stats.snapshot_json();
        assert!(json.contains("\"direct_up\":1000"));
        assert!(json.contains("\"ipset_entries\":142"));
        assert!(json.contains("\"kernel_split\":1"));
    }

    #[test]
    fn tun_path_counters_are_reported() {
        let stats = TrafficStats::new();
        stats.record_tcp_dup(1400);
        stats.record_dup_ack();
        stats.record_dup_ack();
        stats.record_tun_write_wait(30);
        stats.record_tun_write_wait(120);
        stats.record_tun_queue_depth(65536);
        stats.record_retransmit_suppressed(3);
        stats.record_retransmit_budget_rst();

        let json = stats.snapshot_json();
        assert!(json.contains("\"tcp_dup\":1400"), "{json}");
        assert!(json.contains("\"dup_acks\":2"), "{json}");
        // Cumulative vs peak — the peak is what exposes a stalled writer.
        assert!(json.contains("\"tun_wq_ms\":150"), "{json}");
        assert!(json.contains("\"tun_wq_max_ms\":120"), "{json}");
        assert!(json.contains("\"tun_txq_peak\":65536"), "{json}");
        assert!(json.contains("\"retx_suppressed\":3"), "{json}");
        assert!(json.contains("\"retx_budget_rst\":1"), "{json}");
    }

    #[test]
    fn zero_snapshot_matches_live_shape() {
        let live = TrafficStats::new().snapshot_json();
        let zero = TrafficStats::zero_snapshot_json();
        // Same keys in the same order — only the values differ.
        let keys = |json: &str| {
            json.trim_matches(['{', '}'])
                .split(',')
                .map(|kv| kv.split(':').next().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&live), keys(&zero));
    }
}
