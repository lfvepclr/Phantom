use std::net::SocketAddr;
use phantom_core::CipherPreference;
use phantom_crypto::KeyPair;
use phantom_server::handler::handle_connection;
use phantom_transport::tcp::TcpListener;
use phantom_transport::TransportListener;
use tokio::sync::oneshot;
use crate::echo::{EchoMode, EchoServer, start_echo_server};

pub struct TestFixture {
    pub target_addr: SocketAddr,
    pub server_addr: SocketAddr,
    pub client_key: KeyPair,
    pub server_key: KeyPair,
    pub cipher_preference: CipherPreference,
    pub allowed_clients: Vec<[u8; 32]>,
    pub echo_server: EchoServer,
    _server_shutdown: Option<oneshot::Sender<()>>,
}

impl TestFixture {
    pub async fn new(cipher: CipherPreference) -> Self {
        TestFixtureBuilder::new().cipher(cipher).build().await
    }
    pub async fn new_with_mode(cipher: CipherPreference, mode: EchoMode) -> Self {
        TestFixtureBuilder::new().cipher(cipher).echo_mode(mode).build().await
    }
}

impl Drop for TestFixture {
    fn drop(&mut self) {
        if let Some(tx) = self._server_shutdown.take() {
            let _ = tx.send(());
        }
    }
}

pub struct TestFixtureBuilder {
    cipher: CipherPreference,
    echo_mode: EchoMode,
    allowed_clients: Vec<[u8; 32]>,
}

impl TestFixtureBuilder {
    pub fn new() -> Self {
        Self { cipher: CipherPreference::Auto, echo_mode: EchoMode::Echo, allowed_clients: Vec::new() }
    }
    pub fn cipher(mut self, cipher: CipherPreference) -> Self { self.cipher = cipher; self }
    pub fn echo_mode(mut self, mode: EchoMode) -> Self { self.echo_mode = mode; self }
    pub fn allowed_client(mut self, key: [u8; 32]) -> Self { self.allowed_clients.push(key); self }

    pub async fn build(self) -> TestFixture {
        let server_key = KeyPair::generate().expect("Failed to generate server key");
        let client_key = KeyPair::generate().expect("Failed to generate client key");
        let echo_server = start_echo_server(self.echo_mode).await;
        let target_addr = echo_server.addr;
        let server_listener = TcpListener::bind(&"127.0.0.1:0".parse().unwrap())
            .await.expect("Failed to bind phantom server");
        let server_addr = server_listener.local_addr().unwrap();
        let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel::<()>();
        let server_secret = server_key.secret;
        let allowed = self.allowed_clients.clone();
        let cipher_pref = self.cipher;
        tokio::spawn(async move {
            tokio::pin!(server_shutdown_rx);
            loop {
                tokio::select! {
                    accept_result = server_listener.accept() => {
                        match accept_result {
                            Ok((stream, _peer)) => {
                                let sk = server_secret;
                                let allowed_clone = allowed.clone();
                                let cp = cipher_pref;
                                tokio::spawn(async move {
                                    handle_connection(stream, sk, &allowed_clone, cp).await;
                                });
                            }
                            Err(e) => { tracing::error!("Server accept error: {}", e); }
                        }
                    }
                    _ = &mut server_shutdown_rx => { break; }
                }
            }
        });
        TestFixture {
            target_addr, server_addr, client_key, server_key,
            cipher_preference: self.cipher,
            allowed_clients: self.allowed_clients,
            echo_server,
            _server_shutdown: Some(server_shutdown_tx),
        }
    }
}
