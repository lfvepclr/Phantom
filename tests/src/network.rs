use std::cmp;
use std::net::SocketAddr;
use std::time::Duration;
use rand::Rng;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct NetworkCondition {
    pub latency_ms: u64,
    pub jitter_ms: u64,
    pub bandwidth_bps: u64,
    pub loss_rate: f64,
}

impl Default for NetworkCondition {
    fn default() -> Self { Self::normal() }
}

impl NetworkCondition {
    pub fn normal() -> Self { Self { latency_ms: 0, jitter_ms: 0, bandwidth_bps: 0, loss_rate: 0.0 } }
    pub fn high_latency(ms: u64) -> Self { Self { latency_ms: ms, jitter_ms: 0, bandwidth_bps: 0, loss_rate: 0.0 } }
    pub fn low_bandwidth(bps: u64) -> Self { Self { latency_ms: 0, jitter_ms: 0, bandwidth_bps: bps, loss_rate: 0.0 } }
    pub fn packet_loss(pct: f64) -> Self { Self { latency_ms: 0, jitter_ms: 0, bandwidth_bps: 0, loss_rate: pct / 100.0 } }
    pub fn combined(latency_ms: u64, bandwidth_bps: u64, loss_pct: f64) -> Self {
        Self { latency_ms, jitter_ms: latency_ms / 4, bandwidth_bps, loss_rate: loss_pct / 100.0 }
    }
    pub fn with_jitter(mut self, jitter_ms: u64) -> Self { self.jitter_ms = jitter_ms; self }
}

pub struct ThrottledProxy {
    pub listen_addr: SocketAddr,
    _shutdown: Option<oneshot::Sender<()>>,
}

impl ThrottledProxy {
    pub async fn start(target_addr: SocketAddr, condition: NetworkCondition) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind throttled proxy");
        let listen_addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            tokio::pin!(shutdown_rx);
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((client_stream, _peer)) => {
                                let target = target_addr;
                                let cond = condition.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = proxy_connection(client_stream, target, cond).await {
                                        tracing::debug!("Proxy connection error: {}", e);
                                    }
                                });
                            }
                            Err(e) => { tracing::error!("Proxy accept error: {}", e); }
                        }
                    }
                    _ = &mut shutdown_rx => { break; }
                }
            }
        });
        ThrottledProxy { listen_addr, _shutdown: Some(shutdown_tx) }
    }
}

impl Drop for ThrottledProxy {
    fn drop(&mut self) {
        if let Some(tx) = self._shutdown.take() { let _ = tx.send(()); }
    }
}

async fn proxy_connection(
    client_stream: tokio::net::TcpStream,
    target_addr: SocketAddr,
    condition: NetworkCondition,
) -> std::io::Result<()> {
    let server_stream = tokio::net::TcpStream::connect(target_addr).await?;
    let (mut client_read, mut client_write) = tokio::io::split(client_stream);
    let (mut server_read, mut server_write) = tokio::io::split(server_stream);
    let cond_c2s = condition.clone();
    let cond_s2c = condition.clone();
    let c2s = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut client_read, &mut buf).await?;
            if n == 0 { break; }
            let delay = effective_delay(&cond_c2s);
            if !delay.is_zero() { sleep(delay).await; }
            // Simulate packet loss by adding extra delay (retransmission timeout)
            // TCP requires reliable delivery, so we can't actually drop data.
            // Instead, loss manifests as increased latency (retransmission time).
            if cond_c2s.loss_rate > 0.0 && should_drop(cond_c2s.loss_rate) {
                // Simulate retransmission: add extra delay proportional to loss rate
                let retransmit_delay = Duration::from_millis((cond_c2s.latency_ms + 50).max(100));
                sleep(retransmit_delay).await;
            }
            if cond_c2s.bandwidth_bps > 0 {
                let chunk = cmp::min(n, (cond_c2s.bandwidth_bps / 100).max(1) as usize);
                for start in (0..n).step_by(chunk) {
                    let end = cmp::min(start + chunk, n);
                    tokio::io::AsyncWriteExt::write_all(&mut server_write, &buf[start..end]).await?;
                    let d = Duration::from_secs_f64((end - start) as f64 / cond_c2s.bandwidth_bps as f64);
                    if !d.is_zero() { sleep(d).await; }
                }
            } else {
                tokio::io::AsyncWriteExt::write_all(&mut server_write, &buf[..n]).await?;
            }
        }
        tokio::io::AsyncWriteExt::shutdown(&mut server_write).await?;
        Ok::<_, std::io::Error>(())
    };
    let s2c = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut server_read, &mut buf).await?;
            if n == 0 { break; }
            let delay = effective_delay(&cond_s2c);
            if !delay.is_zero() { sleep(delay).await; }
            if cond_s2c.loss_rate > 0.0 && should_drop(cond_s2c.loss_rate) {
                let retransmit_delay = Duration::from_millis((cond_s2c.latency_ms + 50).max(100));
                sleep(retransmit_delay).await;
            }
            if cond_s2c.bandwidth_bps > 0 {
                let chunk = cmp::min(n, (cond_s2c.bandwidth_bps / 100).max(1) as usize);
                for start in (0..n).step_by(chunk) {
                    let end = cmp::min(start + chunk, n);
                    tokio::io::AsyncWriteExt::write_all(&mut client_write, &buf[start..end]).await?;
                    let d = Duration::from_secs_f64((end - start) as f64 / cond_s2c.bandwidth_bps as f64);
                    if !d.is_zero() { sleep(d).await; }
                }
            } else {
                tokio::io::AsyncWriteExt::write_all(&mut client_write, &buf[..n]).await?;
            }
        }
        tokio::io::AsyncWriteExt::shutdown(&mut client_write).await?;
        Ok::<_, std::io::Error>(())
    };
    let _ = tokio::try_join!(c2s, s2c);
    Ok(())
}

fn effective_delay(cond: &NetworkCondition) -> Duration {
    let mut rng = rand::thread_rng();
    let jitter = if cond.jitter_ms > 0 {
        rng.gen_range(0..cond.jitter_ms * 2) as i64 - cond.jitter_ms as i64
    } else { 0 };
    let base = cond.latency_ms as i64 + jitter;
    Duration::from_millis(cmp::max(base, 0) as u64)
}

fn should_drop(loss_rate: f64) -> bool {
    let mut rng = rand::thread_rng();
    rng.r#gen::<f64>() < loss_rate
}
