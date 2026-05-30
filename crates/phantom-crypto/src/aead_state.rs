use aead::{Aead, AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use ascon_aead::{AsconAead128, AsconAead128Nonce, AsconAead128Tag};
use chacha20poly1305::ChaCha20Poly1305;
use phantom_core::{PhantomError, Result};
use zeroize::Zeroize;

use crate::cipher::CipherSuite;

/// Owned cipher state, storing the initialized AEAD object for zero-cost dispatch.
/// Each direction (read/write) gets its own AeadState with independent nonce counter.
pub struct AeadState {
    cipher: OwnedCipher,
    cipher_suite: CipherSuite,
    nonce_counter: u64,
    nonce_prefix: [u8; 4],
}

/// Enum of owned cipher objects — avoids recreating key schedules on every call.
enum OwnedCipher {
    Aes256Gcm(Aes256Gcm),
    Aes128Gcm(Aes128Gcm),
    Ascon128(AsconAead128),
    ChaCha20Poly(ChaCha20Poly1305),
}

impl AeadState {
    pub fn new(cipher: CipherSuite, key: &[u8], nonce_prefix: [u8; 4]) -> Self {
        assert_eq!(key.len(), cipher.key_len(), "key length mismatch for {:?}", cipher);
        let owned = match cipher {
            CipherSuite::Aes256Gcm => {
                OwnedCipher::Aes256Gcm(Aes256Gcm::new_from_slice(key).unwrap())
            }
            CipherSuite::Aes128Gcm => {
                OwnedCipher::Aes128Gcm(Aes128Gcm::new_from_slice(key).unwrap())
            }
            CipherSuite::Ascon128 => {
                OwnedCipher::Ascon128(AsconAead128::new_from_slice(key).unwrap())
            }
            CipherSuite::ChaCha20Poly => {
                OwnedCipher::ChaCha20Poly(ChaCha20Poly1305::new_from_slice(key).unwrap())
            }
        };
        Self {
            cipher: owned,
            cipher_suite: cipher,
            nonce_counter: 0,
            nonce_prefix,
        }
    }

    pub fn cipher(&self) -> CipherSuite {
        self.cipher_suite
    }

    /// Encrypt plaintext, returning ciphertext with auth tag appended.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce = self.next_nonce();
        match &self.cipher {
            OwnedCipher::Aes256Gcm(c) => {
                let n = aes_gcm::Nonce::from_slice(&nonce[..12]);
                c.encrypt(n, plaintext).map_err(|e| PhantomError::Crypto(format!("AES-256-GCM encrypt: {}", e)))
            }
            OwnedCipher::Aes128Gcm(c) => {
                let n = aes_gcm::Nonce::from_slice(&nonce[..12]);
                c.encrypt(n, plaintext).map_err(|e| PhantomError::Crypto(format!("AES-128-GCM encrypt: {}", e)))
            }
            OwnedCipher::Ascon128(c) => {
                let n = AsconAead128Nonce::from_slice(&nonce[..16]);
                c.encrypt(n, plaintext).map_err(|e| PhantomError::Crypto(format!("ASCON-128 encrypt: {}", e)))
            }
            OwnedCipher::ChaCha20Poly(c) => {
                let n = chacha20poly1305::Nonce::from_slice(&nonce[..12]);
                c.encrypt(n, plaintext).map_err(|e| PhantomError::Crypto(format!("ChaCha20 encrypt: {}", e)))
            }
        }
    }

    /// In-place encrypt: plaintext buffer is encrypted and auth tag is appended.
    pub fn encrypt_in_place(&mut self, payload: &mut Vec<u8>) -> Result<()> {
        let nonce = self.next_nonce();
        let tag_len = self.cipher_suite.tag_len();
        let payload_len = payload.len();

        match &self.cipher {
            OwnedCipher::Aes256Gcm(c) => {
                let n = aes_gcm::Nonce::from_slice(&nonce[..12]);
                let tag = c.encrypt_in_place_detached(n, b"", payload).map_err(|e| PhantomError::Crypto(format!("AES-256-GCM encrypt_in_place: {}", e)))?;
                payload.extend_from_slice(&tag);
            }
            OwnedCipher::Aes128Gcm(c) => {
                let n = aes_gcm::Nonce::from_slice(&nonce[..12]);
                let tag = c.encrypt_in_place_detached(n, b"", payload).map_err(|e| PhantomError::Crypto(format!("AES-128-GCM encrypt_in_place: {}", e)))?;
                payload.extend_from_slice(&tag);
            }
            OwnedCipher::Ascon128(c) => {
                let n = AsconAead128Nonce::from_slice(&nonce[..16]);
                let tag = c.encrypt_in_place_detached(n, b"", payload).map_err(|e| PhantomError::Crypto(format!("ASCON-128 encrypt_in_place: {}", e)))?;
                payload.extend_from_slice(&tag);
            }
            OwnedCipher::ChaCha20Poly(c) => {
                let n = chacha20poly1305::Nonce::from_slice(&nonce[..12]);
                let tag = c.encrypt_in_place_detached(n, b"", payload).map_err(|e| PhantomError::Crypto(format!("ChaCha20 encrypt_in_place: {}", e)))?;
                payload.extend_from_slice(&tag);
            }
        }
        debug_assert_eq!(payload.len(), payload_len + tag_len);
        Ok(())
    }

    /// Decrypt ciphertext (including auth tag), returning plaintext.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let nonce = self.next_nonce();
        match &self.cipher {
            OwnedCipher::Aes256Gcm(c) => {
                let n = aes_gcm::Nonce::from_slice(&nonce[..12]);
                c.decrypt(n, ciphertext).map_err(|e| PhantomError::Crypto(format!("AES-256-GCM decrypt: {}", e)))
            }
            OwnedCipher::Aes128Gcm(c) => {
                let n = aes_gcm::Nonce::from_slice(&nonce[..12]);
                c.decrypt(n, ciphertext).map_err(|e| PhantomError::Crypto(format!("AES-128-GCM decrypt: {}", e)))
            }
            OwnedCipher::Ascon128(c) => {
                let n = AsconAead128Nonce::from_slice(&nonce[..16]);
                c.decrypt(n, ciphertext).map_err(|e| PhantomError::Crypto(format!("ASCON-128 decrypt: {}", e)))
            }
            OwnedCipher::ChaCha20Poly(c) => {
                let n = chacha20poly1305::Nonce::from_slice(&nonce[..12]);
                c.decrypt(n, ciphertext).map_err(|e| PhantomError::Crypto(format!("ChaCha20 decrypt: {}", e)))
            }
        }
    }

    /// In-place decrypt: ciphertext buffer is decrypted in place (tag stripped).
    pub fn decrypt_in_place(&mut self, payload: &mut Vec<u8>) -> Result<()> {
        let nonce = self.next_nonce();
        let tag_len = self.cipher_suite.tag_len();
        if payload.len() < tag_len {
            return Err(PhantomError::Crypto("Ciphertext too short".into()));
        }

        let tag_pos = payload.len() - tag_len;
        // Stack-allocated tag buffer - avoids a Vec allocation per frame.
        let mut tag_bytes = [0u8; 16];
        tag_bytes[..tag_len].copy_from_slice(&payload[tag_pos..]);
        payload.truncate(tag_pos);

        match &self.cipher {
            OwnedCipher::Aes256Gcm(c) => {
                let n = aes_gcm::Nonce::from_slice(&nonce[..12]);
                let tag = aes_gcm::Tag::from_slice(&tag_bytes[..tag_len]);
                c.decrypt_in_place_detached(n, b"", payload, &tag).map_err(|e| PhantomError::Crypto(format!("AES-256-GCM decrypt_in_place: {}", e)))?;
            }
            OwnedCipher::Aes128Gcm(c) => {
                let n = aes_gcm::Nonce::from_slice(&nonce[..12]);
                let tag = aes_gcm::Tag::from_slice(&tag_bytes[..tag_len]);
                c.decrypt_in_place_detached(n, b"", payload, &tag).map_err(|e| PhantomError::Crypto(format!("AES-128-GCM decrypt_in_place: {}", e)))?;
            }
            OwnedCipher::Ascon128(c) => {
                let n = AsconAead128Nonce::from_slice(&nonce[..16]);
                let tag = AsconAead128Tag::from_slice(&tag_bytes[..tag_len]);
                c.decrypt_in_place_detached(n, b"", payload, &tag).map_err(|e| PhantomError::Crypto(format!("ASCON-128 decrypt_in_place: {}", e)))?;
            }
            OwnedCipher::ChaCha20Poly(c) => {
                let n = chacha20poly1305::Nonce::from_slice(&nonce[..12]);
                let tag = chacha20poly1305::Tag::from_slice(&tag_bytes[..tag_len]);
                c.decrypt_in_place_detached(n, b"", payload, &tag).map_err(|e| PhantomError::Crypto(format!("ChaCha20 decrypt_in_place: {}", e)))?;
            }
        }
        Ok(())
    }

    /// Generate the next nonce using a stack-allocated array.
    /// Returns [u8; 16] - the maximum nonce length (ASCON-128 uses 16 bytes;
    /// AES-GCM and ChaCha20 use only the first 12 bytes).
    fn next_nonce(&mut self) -> [u8; 16] {
        let mut nonce = [0u8; 16];
        let nonce_len = self.cipher_suite.nonce_len();
        nonce[..4].copy_from_slice(&self.nonce_prefix);
        let counter_bytes = self.nonce_counter.to_be_bytes();
        nonce[nonce_len - 8..nonce_len].copy_from_slice(&counter_bytes);
        self.nonce_counter += 1;
        nonce
    }
}

impl Drop for AeadState {
    fn drop(&mut self) {
        self.nonce_counter.zeroize();
        self.nonce_prefix.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_roundtrip(cipher: CipherSuite) {
        let key = vec![0x42u8; cipher.key_len()];
        let prefix = [0xAA, 0xBB, 0xCC, 0xDD];
        let mut state = AeadState::new(cipher, &key, prefix);

        let plaintext = b"hello phantom tunnel";
        let ciphertext = state.encrypt(plaintext).unwrap();
        let mut dec_state = AeadState::new(cipher, &key, prefix);
        let decrypted = dec_state.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn roundtrip_aes256gcm() { test_roundtrip(CipherSuite::Aes256Gcm); }

    #[test]
    fn roundtrip_aes128gcm() { test_roundtrip(CipherSuite::Aes128Gcm); }

    #[test]
    fn roundtrip_ascon128() { test_roundtrip(CipherSuite::Ascon128); }

    #[test]
    fn roundtrip_chacha20poly() { test_roundtrip(CipherSuite::ChaCha20Poly); }

    #[test]
    fn in_place_roundtrip() {
        let key = vec![0x42u8; 32];
        let prefix = [0x11, 0x22, 0x33, 0x44];

        for &cipher in CipherSuite::all_ordered() {
            let mut enc = AeadState::new(cipher, &key[..cipher.key_len()], prefix);
            let mut dec = AeadState::new(cipher, &key[..cipher.key_len()], prefix);

            let mut buf = b"hello in-place phantom".to_vec();
            enc.encrypt_in_place(&mut buf).unwrap();
            dec.decrypt_in_place(&mut buf).unwrap();
            assert_eq!(&buf, b"hello in-place phantom", "failed for {:?}", cipher);
        }
    }

    #[test]
    fn multiple_messages_roundtrip() {
        let key = vec![0x42u8; 32];
        let prefix = [0x11, 0x22, 0x33, 0x44];
        let mut enc = AeadState::new(CipherSuite::Aes256Gcm, &key, prefix);
        let mut dec = AeadState::new(CipherSuite::Aes256Gcm, &key, prefix);

        for i in 0..10 {
            let msg = format!("message-{}", i);
            let ct = enc.encrypt(msg.as_bytes()).unwrap();
            let pt = dec.decrypt(&ct).unwrap();
            assert_eq!(pt, msg.as_bytes());
        }
    }
}
