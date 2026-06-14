pub const PROTOCOL_VERSION: u8 = 2;
pub const CIPHER_NEGOTIATION_VERSION: u8 = 2;
pub const MAX_FRAME_PAYLOAD: usize = 16384;
pub const FRAME_HEADER_SIZE: usize = 8;
pub const NOISE_TAG_LEN: usize = 16;
pub const HANDSHAKE_TIMEOUT_SECS: u64 = 10;
pub const DEFAULT_BIND: &str = "127.0.0.1:1080";
pub const DEFAULT_SERVER_PORT: u16 = 443;
pub const NOISE_MAX_MSG_LEN: usize = MAX_FRAME_PAYLOAD + FRAME_HEADER_SIZE + NOISE_TAG_LEN + 2;

/// Magic prefix for the encrypted Hello/Hello-ACK control frames.
/// These frames use stream_id = 0 (reserved) inside an already-established
/// Noise session, so they never collide with SOCKS5 relay traffic which
/// starts at stream_id >= 1.
pub const HELLO_MAGIC: &[u8] = b"PH/HELLO";
pub const HELLO_ACK_MAGIC: &[u8] = b"PH/HELLO_ACK";
/// Default timeout for the client-side Hello verification.
pub const HELLO_TIMEOUT_SECS: u64 = 10;
/// Targets used by the server to prove it can reach the public internet.
/// The first reachable target is used; fallback to the second if the first fails.
pub const DEFAULT_HELLO_TARGETS: &[&str] = &[
    "http://captive.apple.com/hotspot-detect.html",
    "http://detectportal.firefox.com/success.txt",
];
