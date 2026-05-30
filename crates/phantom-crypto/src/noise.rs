use phantom_core::{PhantomError, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cipher::CipherSuite;
use crate::session::{negotiate, CipherAccept, CipherOffer};

const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

pub struct NoiseInitiator {
    local_secret: [u8; 32],
    remote_public: [u8; 32],
}

impl NoiseInitiator {
    pub fn new(local_secret: &[u8; 32], remote_public: &[u8; 32]) -> Self {
        Self {
            local_secret: *local_secret,
            remote_public: *remote_public,
        }
    }

    pub async fn handshake<S>(
        &self,
        mut stream: S,
        offer: &CipherOffer,
    ) -> Result<HandshakeResult<S>>
    where
        S: AsyncReadExt + AsyncWriteExt + Unpin,
    {
        let builder = snow::Builder::new(
            NOISE_PATTERN
                .parse()
                .map_err(|e| PhantomError::Handshake(format!("Invalid noise pattern: {}", e)))?,
        )
        .local_private_key(&self.local_secret)
        .remote_public_key(&self.remote_public);

        let mut handshake = builder
            .build_initiator()
            .map_err(|e| PhantomError::Handshake(format!("Build initiator failed: {}", e)))?;

        let mut buf = vec![0u8; 65535];

        // IK pattern: -> e, es, s, ss (client sends first with cipher offer payload)
        let offer_bytes = offer.encode();
        let len = handshake
            .write_message(&offer_bytes, &mut buf)
            .map_err(|e| PhantomError::Handshake(format!("Write handshake msg failed: {}", e)))?;

        let len_be = (len as u16).to_be_bytes();
        stream
            .write_all(&len_be)
            .await
            .map_err(|e| PhantomError::Handshake(format!("Write length failed: {}", e)))?;
        stream
            .write_all(&buf[..len])
            .await
            .map_err(|e| PhantomError::Handshake(format!("Write handshake failed: {}", e)))?;
        stream
            .flush()
            .await
            .map_err(|e| PhantomError::Handshake(format!("Flush failed: {}", e)))?;

        // Read response: <- e, ee, se (with cipher accept payload)
        let mut resp_len_buf = [0u8; 2];
        stream
            .read_exact(&mut resp_len_buf)
            .await
            .map_err(|e| PhantomError::Handshake(format!("Read response length failed: {}", e)))?;
        let resp_len = u16::from_be_bytes(resp_len_buf) as usize;

        if resp_len > buf.len() {
            return Err(PhantomError::Handshake(format!(
                "Response too large: {}",
                resp_len
            )));
        }

        let mut resp_buf = vec![0u8; resp_len];
        stream
            .read_exact(&mut resp_buf)
            .await
            .map_err(|e| PhantomError::Handshake(format!("Read response failed: {}", e)))?;

        // read_message returns the plaintext length, payload is in buf
        let payload_len = handshake
            .read_message(&resp_buf, &mut buf)
            .map_err(|e| PhantomError::Handshake(format!("Read handshake msg failed: {}", e)))?;

        let accept = CipherAccept::decode(&buf[..payload_len])
            .map_err(|e| PhantomError::Handshake(format!("Parse cipher accept: {}", e)))?;

        // Extract raw split keys — returns plain tuple, not Result
        let (k1, k2) = handshake.dangerously_get_raw_split();

        Ok(HandshakeResult {
            stream,
            split_keys: (k1, k2),
            chosen_cipher: accept.cipher,
            is_initiator: true,
            remote_static_key: [0u8; 32], // Not needed on initiator side
        })
    }
}

pub struct NoiseResponder {
    local_secret: [u8; 32],
}

impl NoiseResponder {
    pub fn new(local_secret: &[u8; 32]) -> Self {
        Self {
            local_secret: *local_secret,
        }
    }

    pub async fn handshake<S>(
        &self,
        mut stream: S,
        supported_ciphers: &[CipherSuite],
    ) -> Result<HandshakeResult<S>>
    where
        S: AsyncReadExt + AsyncWriteExt + Unpin,
    {
        let builder = snow::Builder::new(
            NOISE_PATTERN
                .parse()
                .map_err(|e| PhantomError::Handshake(format!("Invalid noise pattern: {}", e)))?,
        )
        .local_private_key(&self.local_secret);

        let mut handshake = builder
            .build_responder()
            .map_err(|e| PhantomError::Handshake(format!("Build responder failed: {}", e)))?;

        let mut buf = vec![0u8; 65535];

        // Read first handshake message: -> e, es, s, ss (with cipher offer payload)
        let mut req_len_buf = [0u8; 2];
        stream
            .read_exact(&mut req_len_buf)
            .await
            .map_err(|e| PhantomError::Handshake(format!("Read request length failed: {}", e)))?;
        let req_len = u16::from_be_bytes(req_len_buf) as usize;

        if req_len > buf.len() {
            return Err(PhantomError::Handshake(format!(
                "Request too large: {}",
                req_len
            )));
        }

        let mut req_buf = vec![0u8; req_len];
        stream
            .read_exact(&mut req_buf)
            .await
            .map_err(|e| PhantomError::Handshake(format!("Read request failed: {}", e)))?;

        // read_message returns plaintext length
        let payload_len = handshake
            .read_message(&req_buf, &mut buf)
            .map_err(|e| PhantomError::Handshake(format!("Read handshake msg failed: {}", e)))?;

        // Extract client static public key
        let remote_static_key = match handshake.get_remote_static() {
            Some(key) => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(key);
                arr
            }
            None => {
                return Err(PhantomError::Handshake(
                    "No remote static key in IK handshake".to_string(),
                ));
            }
        };

        // Parse cipher offer and negotiate
        let offer = CipherOffer::decode(&buf[..payload_len])
            .map_err(|e| PhantomError::Handshake(format!("Parse cipher offer: {}", e)))?;
        let chosen = negotiate(&offer, supported_ciphers)
            .map_err(|e| PhantomError::Handshake(format!("Cipher negotiation: {}", e)))?;

        // Write response: <- e, ee, se (with cipher accept payload)
        let accept = CipherAccept::new(chosen);
        let accept_bytes = accept.encode();
        let len = handshake
            .write_message(&accept_bytes, &mut buf)
            .map_err(|e| PhantomError::Handshake(format!("Write response msg failed: {}", e)))?;

        let len_be = (len as u16).to_be_bytes();
        stream
            .write_all(&len_be)
            .await
            .map_err(|e| PhantomError::Handshake(format!("Write response length failed: {}", e)))?;
        stream
            .write_all(&buf[..len])
            .await
            .map_err(|e| PhantomError::Handshake(format!("Write response failed: {}", e)))?;
        stream
            .flush()
            .await
            .map_err(|e| PhantomError::Handshake(format!("Flush failed: {}", e)))?;

        // Extract raw split keys — returns plain tuple
        let (k1, k2) = handshake.dangerously_get_raw_split();

        Ok(HandshakeResult {
            stream,
            split_keys: (k1, k2),
            chosen_cipher: chosen,
            is_initiator: false,
            remote_static_key,
        })
    }
}

pub struct HandshakeResult<S> {
    pub stream: S,
    pub split_keys: ([u8; 32], [u8; 32]),
    pub chosen_cipher: CipherSuite,
    pub is_initiator: bool,
    pub remote_static_key: [u8; 32],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeyPair;

    #[tokio::test]
    async fn handshake_loopback_with_negotiation() {
        let server_kp = KeyPair::generate().unwrap();
        let client_kp = KeyPair::generate().unwrap();

        let (client_stream, server_stream) = tokio::io::duplex(65536);

        let server_secret = server_kp.secret;
        let client_secret = client_kp.secret;
        let server_public = server_kp.public;

        let offer = CipherOffer::new(CipherSuite::default_offer());
        let supported: Vec<CipherSuite> = CipherSuite::all_ordered().to_vec();

        let server_handle = tokio::spawn(async move {
            let responder = NoiseResponder::new(&server_secret);
            responder.handshake(server_stream, &supported).await.unwrap()
        });

        let client_handle = tokio::spawn(async move {
            let initiator = NoiseInitiator::new(&client_secret, &server_public);
            initiator.handshake(client_stream, &offer).await.unwrap()
        });

        let (server_result, client_result) = (
            server_handle.await.unwrap(),
            client_handle.await.unwrap(),
        );

        // Verify cipher negotiation succeeded
        assert_eq!(server_result.chosen_cipher, client_result.chosen_cipher);

        // Verify server extracted client's public key
        assert_eq!(server_result.remote_static_key, client_kp.public);

        // Verify split keys are identical
        assert_eq!(server_result.split_keys, client_result.split_keys);

        // Verify is_initiator flags
        assert!(client_result.is_initiator);
        assert!(!server_result.is_initiator);
    }
}
