use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy)]
pub enum EchoMode {
    Echo,
    Sink,
}

pub struct EchoServer {
    pub addr: SocketAddr,
    pub mode: EchoMode,
    shutdown: Option<oneshot::Sender<()>>,
}

impl EchoServer {
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for EchoServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub async fn start_echo_server(mode: EchoMode) -> EchoServer {
    start_echo_server_on("127.0.0.1:0", mode).await
}

pub async fn start_echo_server_on(bind: &str, mode: EchoMode) -> EchoServer {
    let listener = TcpListener::bind(bind)
        .await
        .expect("Failed to bind echo server");
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::pin!(rx);
        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, peer)) => {
                            let m = mode;
                            tokio::spawn(async move {
                                if let Err(e) = handle_echo_client(stream, m).await {
                                    tracing::debug!("Echo client {} error: {}", peer, e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("Echo accept error: {}", e);
                        }
                    }
                }
                _ = &mut rx => {
                    break;
                }
            }
        }
    });
    EchoServer { addr, mode, shutdown: Some(tx) }
}

async fn handle_echo_client(mut stream: tokio::net::TcpStream, mode: EchoMode) -> std::io::Result<()> {
    let mut buf = vec![0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 { break; }
        match mode {
            EchoMode::Echo => { stream.write_all(&buf[..n]).await?; }
            EchoMode::Sink => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP Echo Server (for E2E scene tests)
// ---------------------------------------------------------------------------

use axum::{
    extract::Path,
    routing::{get, post},
    Router,
};

pub struct HttpEchoServer {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
}

impl HttpEchoServer {
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for HttpEchoServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub async fn start_http_echo_server() -> HttpEchoServer {
    let app = Router::new()
        .route("/ip", get(|| async { "127.0.0.1" }))
        .route("/echo", post(|body: String| async { body }))
        .route(
            "/delay/:ms",
            get(|Path(ms): Path<u64>| async move {
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                "ok"
            }),
        );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind HTTP echo server");
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let server = axum::serve(listener, app);
        tokio::select! {
            _ = server => {},
            _ = rx => {},
        }
    });

    HttpEchoServer {
        addr,
        shutdown: Some(tx),
    }
}
