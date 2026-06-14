use std::net::SocketAddr;
use tokio::net::UdpSocket;

pub struct UdpEchoServer {
    pub addr: SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl UdpEchoServer {
    pub async fn start() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            let mut rx = rx;
            loop {
                let recv = socket.recv_from(&mut buf);
                tokio::select! {
                    result = recv => {
                        if let Ok((n, peer)) = result {
                            let _ = socket.send_to(&buf[..n], peer).await;
                        }
                    }
                    _ = &mut rx => break,
                }
            }
        });

        Self {
            addr,
            shutdown: Some(tx),
        }
    }
}

impl Drop for UdpEchoServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}
