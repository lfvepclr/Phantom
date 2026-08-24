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
    pub udp_datagrams_up: AtomicU64,
    pub udp_datagrams_down: AtomicU64,
}

impl TrafficStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
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
             # HELP phantom_udp_datagrams_up Total UDP datagrams sent upstream\n# TYPE phantom_udp_datagrams_up counter\nphantom_udp_datagrams_up {}\n\
             # HELP phantom_udp_datagrams_down Total UDP datagrams received downstream\n# TYPE phantom_udp_datagrams_down counter\nphantom_udp_datagrams_down {}\n",
            self.tcp_bytes_up.load(Ordering::Relaxed),
            self.tcp_bytes_down.load(Ordering::Relaxed),
            self.udp_bytes_up.load(Ordering::Relaxed),
            self.udp_bytes_down.load(Ordering::Relaxed),
            self.tcp_connections.load(Ordering::Relaxed),
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
}
