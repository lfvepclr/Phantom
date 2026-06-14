use hkdf::Hkdf;
use sha2::Sha256;

use crate::constants::CIPHER_NEGOTIATION_VERSION;
use crate::{PhantomError, Result};

use crate::crypto::aead_state::AeadState;
use crate::crypto::cipher::CipherSuite;
use crate::crypto::reader::SessionReader;
use crate::crypto::writer::SessionWriter;

pub const CIPHER_OFFER_VERSION: u8 = CIPHER_NEGOTIATION_VERSION;

/// Client's cipher offer, embedded in the first Noise IK handshake message.
#[derive(Debug)]
pub struct CipherOffer {
    pub version: u8,
    pub ciphers: Vec<CipherSuite>,
}

impl CipherOffer {
    pub fn new(ciphers: Vec<CipherSuite>) -> Self {
        Self {
            version: CIPHER_OFFER_VERSION,
            ciphers,
        }
    }

    pub fn default_offer() -> Self {
        Self::new(CipherSuite::default_offer())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(2 + self.ciphers.len());
        buf.push(self.version);
        buf.push(self.ciphers.len() as u8);
        for &cs in &self.ciphers {
            buf.push(cs.to_u8());
        }
        buf
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 2 {
            return Err(PhantomError::CipherNegotiation(
                "Cipher offer too short".into(),
            ));
        }
        let version = data[0];
        if version != CIPHER_OFFER_VERSION {
            return Err(PhantomError::CipherNegotiation(format!(
                "Unsupported cipher negotiation version: {}",
                version
            )));
        }
        let count = data[1] as usize;
        if data.len() < 2 + count {
            return Err(PhantomError::CipherNegotiation(
                "Cipher offer truncated".into(),
            ));
        }
        let ciphers: Vec<CipherSuite> = data[2..2 + count]
            .iter()
            .filter_map(|&b| CipherSuite::from_u8(b))
            .collect();
        if ciphers.is_empty() {
            return Err(PhantomError::CipherNegotiation(
                "No valid ciphers in offer".into(),
            ));
        }
        Ok(Self { version, ciphers })
    }
}

/// Server's cipher accept, embedded in the Noise IK response message.
#[derive(Debug)]
pub struct CipherAccept {
    pub version: u8,
    pub cipher: CipherSuite,
}

impl CipherAccept {
    pub fn new(cipher: CipherSuite) -> Self {
        Self {
            version: CIPHER_OFFER_VERSION,
            cipher,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        vec![self.version, self.cipher.to_u8()]
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 2 {
            return Err(PhantomError::CipherNegotiation(
                "Cipher accept too short".into(),
            ));
        }
        let version = data[0];
        if version != CIPHER_OFFER_VERSION {
            return Err(PhantomError::CipherNegotiation(format!(
                "Unsupported cipher negotiation version: {}",
                version
            )));
        }
        let cipher = CipherSuite::from_u8(data[1]).ok_or_else(|| {
            PhantomError::CipherNegotiation(format!("Unknown cipher id: {}", data[1]))
        })?;
        Ok(Self { version, cipher })
    }
}

/// Select the best cipher from the intersection of client offer and server support.
pub fn negotiate(offer: &CipherOffer, supported: &[CipherSuite]) -> Result<CipherSuite> {
    for &offered in &offer.ciphers {
        if supported.contains(&offered) {
            return Ok(offered);
        }
    }
    Err(PhantomError::CipherNegotiation(
        "No common cipher suite".into(),
    ))
}

/// Session keys derived from Noise split keys via HKDF.
pub struct SessionKeys {
    pub write_key: Vec<u8>,
    pub read_key: Vec<u8>,
    pub write_nonce_prefix: [u8; 4],
    pub read_nonce_prefix: [u8; 4],
    pub cipher: CipherSuite,
}

impl SessionKeys {
    /// Derive session keys from Noise split keys.
    /// `k1` = initiator→responder key, `k2` = responder→initiator key.
    /// `is_initiator` determines which key is for writing vs reading.
    pub fn derive(k1: &[u8; 32], k2: &[u8; 32], cipher: CipherSuite, is_initiator: bool) -> Self {
        Self::derive_internal(k1, k2, cipher, is_initiator, None)
    }

    /// Derive per-stream session keys from the parent connection's Noise split keys.
    /// Uses stream_id in the HKDF info to guarantee key isolation between streams.
    pub fn derive_stream(
        k1: &[u8; 32],
        k2: &[u8; 32],
        cipher: CipherSuite,
        is_initiator: bool,
        stream_id: u32,
    ) -> Self {
        Self::derive_internal(k1, k2, cipher, is_initiator, Some(stream_id))
    }

    fn derive_internal(
        k1: &[u8; 32],
        k2: &[u8; 32],
        cipher: CipherSuite,
        is_initiator: bool,
        stream_id: Option<u32>,
    ) -> Self {
        let info_prefix = match stream_id {
            Some(id) => format!("phantom-v3-stream-{}-{}", id, cipher.name()),
            None => format!("phantom-v2-{}", cipher.name()),
        };

        let c2s_key = hkdf_expand(k1, &format!("{}-c2s", info_prefix), cipher.key_len());
        let s2c_key = hkdf_expand(k2, &format!("{}-s2c", info_prefix), cipher.key_len());
        let c2s_nonce_prefix = hkdf_expand_4(k1, &format!("{}-c2s-nonce", info_prefix));
        let s2c_nonce_prefix = hkdf_expand_4(k2, &format!("{}-s2c-nonce", info_prefix));

        let (write_key, read_key, write_nonce_prefix, read_nonce_prefix) = if is_initiator {
            (c2s_key, s2c_key, c2s_nonce_prefix, s2c_nonce_prefix)
        } else {
            (s2c_key, c2s_key, s2c_nonce_prefix, c2s_nonce_prefix)
        };

        Self {
            write_key,
            read_key,
            write_nonce_prefix,
            read_nonce_prefix,
            cipher,
        }
    }
}

fn hkdf_expand(ikm: &[u8], info: &str, len: usize) -> Vec<u8> {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut out = vec![0u8; len];
    hk.expand(info.as_bytes(), &mut out)
        .expect("HKDF expand should not fail with valid length");
    out
}

fn hkdf_expand_4(ikm: &[u8], info: &str) -> [u8; 4] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut out = [0u8; 4];
    hk.expand(info.as_bytes(), &mut out)
        .expect("HKDF expand should not fail with 4-byte output");
    out
}

/// Split a stream after handshake into SessionReader/SessionWriter.
pub fn split_after_handshake<S>(
    stream: S,
    split_keys: ([u8; 32], [u8; 32]),
    cipher: CipherSuite,
    is_initiator: bool,
) -> (
    SessionReader<tokio::io::ReadHalf<S>>,
    SessionWriter<tokio::io::WriteHalf<S>>,
)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    split_stream(stream, split_keys, cipher, is_initiator, None)
}

/// Split a stream using per-stream derived keys for multiplexed connections.
/// Does NOT perform a Noise handshake; keys are derived from the parent
/// connection's split_keys via HKDF with stream_id.
pub fn split_for_stream<S>(
    stream: S,
    split_keys: &([u8; 32], [u8; 32]),
    cipher: CipherSuite,
    is_initiator: bool,
    stream_id: u32,
) -> (
    SessionReader<tokio::io::ReadHalf<S>>,
    SessionWriter<tokio::io::WriteHalf<S>>,
)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    split_stream(stream, *split_keys, cipher, is_initiator, Some(stream_id))
}

fn split_stream<S>(
    stream: S,
    split_keys: ([u8; 32], [u8; 32]),
    cipher: CipherSuite,
    is_initiator: bool,
    stream_id: Option<u32>,
) -> (
    SessionReader<tokio::io::ReadHalf<S>>,
    SessionWriter<tokio::io::WriteHalf<S>>,
)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let session_keys = match stream_id {
        Some(id) => {
            SessionKeys::derive_stream(&split_keys.0, &split_keys.1, cipher, is_initiator, id)
        }
        None => SessionKeys::derive(&split_keys.0, &split_keys.1, cipher, is_initiator),
    };
    let (read_half, write_half) = tokio::io::split(stream);

    let read_state = AeadState::new(
        cipher,
        &session_keys.read_key,
        session_keys.read_nonce_prefix,
    );
    let write_state = AeadState::new(
        cipher,
        &session_keys.write_key,
        session_keys.write_nonce_prefix,
    );

    let reader = SessionReader::new(read_half, read_state);
    let writer = SessionWriter::new(write_half, write_state);
    (reader, writer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cipher_offer_roundtrip() {
        let offer = CipherOffer::new(vec![CipherSuite::Aes256Gcm, CipherSuite::Ascon128]);
        let encoded = offer.encode();
        let decoded = CipherOffer::decode(&encoded).unwrap();
        assert_eq!(decoded.version, CIPHER_OFFER_VERSION);
        assert_eq!(decoded.ciphers, offer.ciphers);
    }

    #[test]
    fn cipher_accept_roundtrip() {
        let accept = CipherAccept::new(CipherSuite::Ascon128);
        let encoded = accept.encode();
        let decoded = CipherAccept::decode(&encoded).unwrap();
        assert_eq!(decoded.cipher, CipherSuite::Ascon128);
    }

    #[test]
    fn negotiate_selects_first_match() {
        let offer = CipherOffer::new(vec![CipherSuite::Ascon128, CipherSuite::ChaCha20Poly]);
        let supported = [CipherSuite::ChaCha20Poly, CipherSuite::Ascon128];
        let chosen = negotiate(&offer, &supported).unwrap();
        assert_eq!(chosen, CipherSuite::Ascon128); // first in offer that's also supported
    }

    use std::assert_matches;

    #[test]
    fn negotiate_no_match() {
        let offer = CipherOffer::new(vec![CipherSuite::Ascon128]);
        let supported = [CipherSuite::Aes256Gcm];
        assert_matches!(negotiate(&offer, &supported), Err(_));
    }

    #[test]
    fn session_keys_deterministic() {
        let k1 = [0x11u8; 32];
        let k2 = [0x22u8; 32];
        let sk1 = SessionKeys::derive(&k1, &k2, CipherSuite::Aes256Gcm, true);
        let sk2 = SessionKeys::derive(&k1, &k2, CipherSuite::Aes256Gcm, true);
        assert_eq!(sk1.write_key, sk2.write_key);
        assert_eq!(sk1.read_key, sk2.read_key);
    }

    #[test]
    fn session_keys_initiator_responder_complementary() {
        let k1 = [0x11u8; 32];
        let k2 = [0x22u8; 32];
        let initiator = SessionKeys::derive(&k1, &k2, CipherSuite::Aes256Gcm, true);
        let responder = SessionKeys::derive(&k1, &k2, CipherSuite::Aes256Gcm, false);
        // Initiator's write key = responder's read key
        assert_eq!(initiator.write_key, responder.read_key);
        // Initiator's read key = responder's write key
        assert_eq!(initiator.read_key, responder.write_key);
    }
}
