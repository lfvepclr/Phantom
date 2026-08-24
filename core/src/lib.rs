pub mod config;
pub mod constants;
pub mod crypto;
pub mod error;
pub mod protocol;
pub mod transport;
pub mod uri;

pub use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
pub use config::{
    AllowedClient, CipherPreference, ClientConfig, ClientRule, ClientSettings, CongestionAlgorithm,
    FailoverConfig, HelloConfig, PerformanceConfig, ProxyAuthConfig, ProxyMode, QuicConfig,
    RuleAction, RulePattern, RulesConfig, ServerConfig, ServerEntry, TlsConfig, TransportProtocol,
};
pub use error::{PhantomError, Result};
pub use uri::{build_phantom_uri, parse_phantom_uri};

// Re-exports from merged crypto module
pub use crypto::{
    CipherSuite, HandshakeResult, KeyPair, NoiseInitiator, NoiseResponder, Psk, ServerIdentity,
    SessionReader, SessionWriter, split_after_handshake,
};
// Re-exports from merged protocol module
pub use protocol::{
    Frame, FrameFlags, FrameReader, FrameWriter, MessageRead, MessageWrite, PlainMessageReader,
    PlainMessageWriter, TargetAddr,
};
// Re-exports from merged transport module
pub use transport::{QuicAuth, QuicStream, Transport, TransportListener};
