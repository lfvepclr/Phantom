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
            "{{\"up\":{},\"down\":{},\"udp_up\":{},\"udp_down\":{},\"conns\":{},\"route_direct\":{},\"route_proxy\":{}}}",
            self.tcp_bytes_up.load(Ordering::Relaxed),
            self.tcp_bytes_down.load(Ordering::Relaxed),
            self.udp_bytes_up.load(Ordering::Relaxed),
            self.udp_bytes_down.load(Ordering::Relaxed),
            self.tcp_connections.load(Ordering::Relaxed),
            self.route_direct.load(Ordering::Relaxed),
            self.route_proxy.load(Ordering::Relaxed),
        )
    }

    /// The same JSON shape with every counter at zero.
    ///
    /// Used before the first successful start, when no `TrafficStats` instance
    /// exists yet — the UI still wants a well-formed document to parse.
    pub fn zero_snapshot_json() -> String {
        "{\"up\":0,\"down\":0,\"udp_up\":0,\"udp_down\":0,\"conns\":0,\"route_direct\":0,\"route_proxy\":0}"
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

    /// Render stats in Prometheus exposition format.
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
             # HELP phantom_udp_datagrams_down Total UDP datagrams received downstream\n# TYPE phantom_udp_datagrams_down counter\nphantom_udp_datagrams_down {}\n",
            self.tcp_bytes_up.load(Ordering::Relaxed),
            self.tcp_bytes_down.load(Ordering::Relaxed),
            self.udp_bytes_up.load(Ordering::Relaxed),
            self.udp_bytes_down.load(Ordering::Relaxed),
            self.tcp_connections.load(Ordering::Relaxed),
            self.route_direct.load(Ordering::Relaxed),
            self.route_proxy.load(Ordering::Relaxed),
            self.udp_datagrams_up.load(Ordering::Relaxed),
            self.udp_datagrams_down.load(Ordering::Relaxed),
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
             \"conns\":1,\"route_direct\":2,\"route_proxy\":1}"
        );
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
