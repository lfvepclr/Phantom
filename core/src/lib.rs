pub mod buf_pool;
pub mod config;
pub mod constants;
pub mod crypto;
pub mod error;
pub mod protocol;
pub mod transport;
pub mod uri;

pub use buf_pool::BufferPool;
pub use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
pub use config::{
    AllowedClient, CipherPreference, ClientConfig, ClientRule, ClientSettings, CongestionAlgorithm, FailoverConfig,
    PerformanceConfig, ProxyMode, QuicConfig, RuleAction, RulePattern, RulesConfig, ServerConfig, ServerEntry,
    TlsConfig, TransportProtocol,
};
pub use error::{PhantomError, Result};
pub use uri::{build_phantom_uri, parse_phantom_uri};

// Re-exports from merged crypto module
pub use crypto::{CipherSuite, KeyPair, HandshakeResult, NoiseInitiator, NoiseResponder, SessionReader, SessionWriter,
    split_after_handshake, split_for_stream};
// Re-exports from merged protocol module
pub use protocol::{Frame, FrameFlags, TargetAddr, FrameReader, FrameWriter};
// Re-exports from merged transport module
pub use transport::{Transport, TransportListener, QuicStream, QuicTransport};
