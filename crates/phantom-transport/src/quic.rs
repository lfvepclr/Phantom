use async_trait::async_trait;
use phantom_core::CongestionAlgorithm;
use phantom_core::{PhantomError, Result};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Mutex;

use crate::traits::{Transport, TransportListener};

pub struct QuicTransport {
    connect_timeout: std::time::Duration,
    server_name: String,
    congestion: CongestionAlgorithm,
}

impl QuicTransport {
    pub fn new(connect_timeout: std::time::Duration, server_name: &str) -> Self {
        Self {
            connect_timeout,
            server_name: server_name.to_string(),
            congestion: CongestionAlgorithm::default(),
        }
    }

    pub fn with_congestion(mut self, congestion: CongestionAlgorithm) -> Self {
        self.congestion = congestion;
        self
    }
}

#[async_trait]
impl Transport for QuicTransport {
    type Stream = QuicStream;

    async fn connect(&self, addr: &SocketAddr) -> Result<Self::Stream> {
        let endpoint = create_client_endpoint(self.congestion)?;

        let connecting = endpoint.connect(*addr, &self.server_name)
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        let conn = tokio::time::timeout(self.connect_timeout, connecting)
            .await
            .map_err(|_| PhantomError::Timeout)?
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, e)))?;

        let (send, recv) = conn.open_bi()
            .await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        Ok(QuicStream::new(send, recv))
    }

    fn name(&self) -> &str {
        "quic"
    }
}

pub struct QuicListener {
    endpoint: quinn::Endpoint,
}

impl QuicListener {
    pub async fn bind(addr: &SocketAddr, congestion: CongestionAlgorithm) -> Result<Self> {
        let server_config = create_server_config(congestion)?;
        let endpoint = quinn::Endpoint::server(server_config, *addr)
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::AddrInUse, e)))?;
        Ok(Self { endpoint })
    }
}

#[async_trait]
impl TransportListener for QuicListener {
    type Stream = QuicStream;

    async fn accept(&self) -> Result<(Self::Stream, SocketAddr)> {
        let incoming = self.endpoint.accept().await
            .ok_or(PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, "No incoming connection")))?;
        let conn = incoming.await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        let remote = conn.remote_address();
        let (send, recv) = conn.accept_bi()
            .await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        Ok((QuicStream::new(send, recv), remote))
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint.local_addr().map_err(PhantomError::Io)
    }
}

/// A QUIC stream that properly implements AsyncRead + AsyncWrite.
///
/// Send and recv directions use separate mutexes so reads and writes
/// can proceed concurrently. After `tokio::io::split`, the ReadHalf
/// only locks recv and the WriteHalf only locks send — zero contention.
pub struct QuicStream {
    send: Arc<Mutex<Option<quinn::SendStream>>>,
    recv: Arc<Mutex<Option<quinn::RecvStream>>>,
}

impl QuicStream {
    pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self {
            send: Arc::new(Mutex::new(Some(send))),
            recv: Arc::new(Mutex::new(Some(recv))),
        }
    }
}

impl AsyncRead for QuicStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let recv = self.recv.clone();
        let mut guard = match recv.try_lock() {
            Ok(g) => g,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };

        let Some(ref mut recv_stream) = *guard else {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "QUIC recv stream closed",
            )));
        };

        // Use tokio's AsyncRead trait explicitly
        AsyncRead::poll_read(Pin::new(recv_stream), cx, buf)
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let send = self.send.clone();
        let mut guard = match send.try_lock() {
            Ok(g) => g,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };

        let Some(ref mut send_stream) = *guard else {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "QUIC send stream closed",
            )));
        };

        // Use tokio's AsyncWrite trait explicitly
        AsyncWrite::poll_write(Pin::new(send_stream), cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        // quinn's poll_flush is a no-op
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let send = self.send.clone();
        let mut guard = match send.try_lock() {
            Ok(g) => g,
            Err(_) => {
                return Poll::Ready(Ok(()));
            }
        };

        if let Some(ref mut send_stream) = *guard {
            let _ = send_stream.finish();
        }
        *guard = None;
        Poll::Ready(Ok(()))
    }
}

/// Build a transport config with the specified congestion control algorithm.
pub fn build_transport_config(congestion: CongestionAlgorithm) -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    match congestion {
        CongestionAlgorithm::Bbr => {
            transport.congestion_controller_factory(Arc::new(
                quinn::congestion::BbrConfig::default(),
            ));
        }
        CongestionAlgorithm::Cubic => {
            transport.congestion_controller_factory(Arc::new(
                quinn::congestion::CubicConfig::default(),
            ));
        }
        CongestionAlgorithm::NewReno => {
            transport.congestion_controller_factory(Arc::new(
                quinn::congestion::NewRenoConfig::default(),
            ));
        }
    }
    Arc::new(transport)
}

pub fn create_client_endpoint(congestion: CongestionAlgorithm) -> Result<quinn::Endpoint> {
    let mut endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())
        .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    let rustls_config = quinn::rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();

    let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(rustls_config)
        .map_err(|e| PhantomError::Crypto(format!("QUIC client config error: {:?}", e)))?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
    client_config.transport_config(build_transport_config(congestion));
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

pub fn create_server_config(congestion: CongestionAlgorithm) -> Result<quinn::ServerConfig> {
    let cert = generate_self_signed_cert();
    let rustls_config = quinn::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.0], cert.1)
        .map_err(|e| PhantomError::Crypto(format!("TLS config error: {}", e)))?;

    let quic_config = quinn::crypto::rustls::QuicServerConfig::try_from(Arc::new(rustls_config))
        .map_err(|e| PhantomError::Crypto(format!("QUIC server config error: {:?}", e)))?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
    server_config.transport_config(build_transport_config(congestion));
    Ok(server_config)
}

fn generate_self_signed_cert() -> (quinn::rustls::pki_types::CertificateDer<'static>, quinn::rustls::pki_types::PrivateKeyDer<'static>) {
    let params = rcgen::CertificateParams::default();
    let key_pair = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    let cert_der = quinn::rustls::pki_types::CertificateDer::from(cert);
    let key_der = quinn::rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der());
    (cert_der, quinn::rustls::pki_types::PrivateKeyDer::from(key_der))
}

#[derive(Debug)]
struct NoVerifier;

impl quinn::rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &quinn::rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[quinn::rustls::pki_types::CertificateDer<'_>],
        _server_name: &quinn::rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: quinn::rustls::pki_types::UnixTime,
    ) -> std::result::Result<quinn::rustls::client::danger::ServerCertVerified, quinn::rustls::Error> {
        Ok(quinn::rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &quinn::rustls::pki_types::CertificateDer<'_>,
        _dss: &quinn::rustls::DigitallySignedStruct,
    ) -> std::result::Result<quinn::rustls::client::danger::HandshakeSignatureValid, quinn::rustls::Error> {
        Ok(quinn::rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &quinn::rustls::pki_types::CertificateDer<'_>,
        _dss: &quinn::rustls::DigitallySignedStruct,
    ) -> std::result::Result<quinn::rustls::client::danger::HandshakeSignatureValid, quinn::rustls::Error> {
        Ok(quinn::rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<quinn::rustls::SignatureScheme> {
        vec![
            quinn::rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            quinn::rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            quinn::rustls::SignatureScheme::ED25519,
            quinn::rustls::SignatureScheme::RSA_PKCS1_SHA256,
            quinn::rustls::SignatureScheme::RSA_PKCS1_SHA384,
            quinn::rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}
