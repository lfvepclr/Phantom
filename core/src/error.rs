use thiserror::Error;

#[derive(Error, Debug)]
pub enum PhantomError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Crypto error: {0}")]
    Crypto(String),
    #[error("Handshake failed: {0}")]
    Handshake(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Config error: {0}")]
    Config(String),
    #[error("Connection timeout")]
    Timeout,
    #[error("Server unreachable: {name}")]
    ServerUnreachable { name: String },
    #[error("All servers failed")]
    AllServersFailed,
    #[error("Cipher negotiation failed: {0}")]
    CipherNegotiation(String),
    #[error("Hello verification failed: {0}")]
    HelloVerification(String),
}

pub type Result<T> = std::result::Result<T, PhantomError>;
