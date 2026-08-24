use crate::{PhantomError, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand_core::{OsRng, RngCore};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use x25519_dalek::{PublicKey, StaticSecret};

use zeroize::{Zeroize, ZeroizeOnDrop};

/// A 32-byte pre-shared key mixed into the Noise handshake.
///
/// The PSK is an authentication factor **in addition to** the static key pair,
/// never a replacement for the Diffie-Hellman exchange. On the TCP path it is
/// mixed into the key schedule via the `psk2` modifier; on the QUIC path it is
/// bound through the Noise prologue (see `core/src/transport/quic.rs`).
///
/// Two properties follow from this:
/// - Active probes without the PSK cannot produce a valid first handshake
///   message, so the server drops them without revealing anything.
/// - Leaking the server's static private key alone is not enough to connect.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Psk([u8; 32]);

impl Psk {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }

    pub fn from_base64(s: &str) -> Result<Self> {
        let decoded = STANDARD
            .decode(s.trim())
            .map_err(|e| PhantomError::Crypto(format!("PSK base64 decode failed: {}", e)))?;
        if decoded.len() != 32 {
            return Err(PhantomError::Crypto(format!(
                "PSK must be 32 bytes, got {}",
                decoded.len()
            )));
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&decoded);
        Ok(Self(bytes))
    }
}

impl std::fmt::Debug for Psk {
    /// Never print the key material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Psk(<redacted>)")
    }
}

/// The server's on-disk identity: static key pair plus handshake PSK.
pub struct ServerIdentity {
    pub keys: KeyPair,
    pub psk: Psk,
    /// True when the PSK was freshly generated because the key file predated
    /// PSK support. Callers should warn that previously distributed URIs are
    /// now stale.
    pub psk_generated: bool,
}

#[derive(Zeroize)]
pub struct KeyPair {
    #[zeroize(skip)] // Public key doesn't need zeroing
    pub public: [u8; 32],
    pub secret: [u8; 32],
}

impl KeyPair {
    pub fn generate() -> Result<Self> {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Ok(Self {
            public: public.to_bytes(),
            secret: secret.to_bytes(),
        })
    }

    pub fn public_key_base64(&self) -> String {
        STANDARD.encode(self.public)
    }

    pub fn secret_key_base64(&self) -> String {
        STANDARD.encode(self.secret)
    }

    pub fn from_secret_base64(s: &str) -> Result<Self> {
        let decoded = STANDARD
            .decode(s.trim())
            .map_err(|e| PhantomError::Crypto(format!("Base64 decode failed: {}", e)))?;
        if decoded.len() != 32 {
            return Err(PhantomError::Crypto(format!(
                "Secret key must be 32 bytes, got {}",
                decoded.len()
            )));
        }
        let mut secret_bytes = [0u8; 32];
        secret_bytes.copy_from_slice(&decoded);
        let secret = StaticSecret::from(secret_bytes);
        let public = PublicKey::from(&secret);
        Ok(Self {
            public: public.to_bytes(),
            secret: secret_bytes,
        })
    }

    pub fn save_secret_to_file(&self, path: &str) -> Result<()> {
        self.write_key_file(path, None)
    }

    /// Persist the key pair together with a handshake PSK.
    ///
    /// File layout: line 1 public key, line 2 secret key, line 3 PSK.
    pub fn save_secret_with_psk_to_file(&self, path: &str, psk: &Psk) -> Result<()> {
        self.write_key_file(path, Some(psk))
    }

    fn write_key_file(&self, path: &str, psk: Option<&Psk>) -> Result<()> {
        use std::io::Write;
        let mut file = fs::File::create(path)
            .map_err(|e| PhantomError::Crypto(format!("Failed to create key file: {}", e)))?;
        file.lock()
            .map_err(|e| PhantomError::Crypto(format!("Failed to lock key file: {}", e)))?;
        let mut content = format!(
            "{}\n{}\n",
            self.public_key_base64(),
            self.secret_key_base64()
        );
        if let Some(psk) = psk {
            content.push_str(&psk.to_base64());
            content.push('\n');
        }
        file.write_all(content.as_bytes())
            .map_err(|e| PhantomError::Crypto(format!("Failed to write key file: {}", e)))?;
        let mut perms = fs::metadata(path)
            .map_err(|e| PhantomError::Crypto(format!("Failed to stat key file: {}", e)))?
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms).map_err(|e| {
            PhantomError::Crypto(format!("Failed to set key file permissions: {}", e))
        })?;
        Ok(())
    }

    pub fn save_public_to_file(&self, path: &str) -> Result<()> {
        fs::write(path, self.public_key_base64() + "\n")
            .map_err(|e| PhantomError::Crypto(format!("Failed to write public key file: {}", e)))?;
        Ok(())
    }

    pub fn load_secret_from_file(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| PhantomError::Crypto(format!("Failed to read key file: {}", e)))?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() < 2 {
            return Err(PhantomError::Crypto(
                "Key file must contain public key on line 1 and secret key on line 2".to_string(),
            ));
        }

        let public_decoded = STANDARD
            .decode(lines[0].trim())
            .map_err(|e| PhantomError::Crypto(format!("Public key decode failed: {}", e)))?;
        let secret_decoded = STANDARD
            .decode(lines[1].trim())
            .map_err(|e| PhantomError::Crypto(format!("Secret key decode failed: {}", e)))?;

        if public_decoded.len() != 32 || secret_decoded.len() != 32 {
            return Err(PhantomError::Crypto(
                "Keys must be 32 bytes each".to_string(),
            ));
        }

        let mut public = [0u8; 32];
        let mut secret = [0u8; 32];
        public.copy_from_slice(&public_decoded);
        secret.copy_from_slice(&secret_decoded);

        Ok(Self { public, secret })
    }

    /// Load the server identity (key pair + PSK) from a key file.
    ///
    /// Key files written before PSK support only have two lines. Rather than
    /// failing, a PSK is generated and appended so an existing deployment keeps
    /// its stable public key; `psk_generated` is set so the caller can warn that
    /// already-distributed URIs no longer work.
    pub fn load_server_identity(path: &str) -> Result<ServerIdentity> {
        let keys = Self::load_secret_from_file(path)?;
        let content = fs::read_to_string(path)
            .map_err(|e| PhantomError::Crypto(format!("Failed to read key file: {}", e)))?;

        match content.lines().nth(2).map(str::trim) {
            Some(line) if !line.is_empty() => Ok(ServerIdentity {
                psk: Psk::from_base64(line)?,
                keys,
                psk_generated: false,
            }),
            _ => {
                let psk = Psk::generate();
                keys.save_secret_with_psk_to_file(path, &psk)?;
                Ok(ServerIdentity {
                    keys,
                    psk,
                    psk_generated: true,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_keypair_roundtrip() {
        let kp = KeyPair::generate().unwrap();
        assert!(!kp.public_key_base64().is_empty());
        assert!(!kp.secret_key_base64().is_empty());
    }

    #[test]
    fn from_secret_derives_public() {
        let kp = KeyPair::generate().unwrap();
        let kp2 = KeyPair::from_secret_base64(&kp.secret_key_base64()).unwrap();
        assert_eq!(kp.public, kp2.public);
        assert_eq!(kp.secret, kp2.secret);
    }

    #[test]
    fn save_load_roundtrip() {
        let kp = KeyPair::generate().unwrap();
        let dir = std::env::temp_dir().join("phantom_test_keys");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.key");
        kp.save_secret_to_file(path.to_str().unwrap()).unwrap();
        let loaded = KeyPair::load_secret_from_file(path.to_str().unwrap()).unwrap();
        assert_eq!(kp.public, loaded.public);
        assert_eq!(kp.secret, loaded.secret);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn psk_base64_roundtrip() {
        let psk = Psk::generate();
        let restored = Psk::from_base64(&psk.to_base64()).unwrap();
        assert_eq!(psk.as_bytes(), restored.as_bytes());
    }

    #[test]
    fn psk_rejects_wrong_length_and_garbage() {
        // "test" decodes to 4 bytes.
        assert!(Psk::from_base64("dGVzdA==").is_err());
        assert!(Psk::from_base64("!!!not-base64!!!").is_err());
    }

    #[test]
    fn two_generated_psks_differ() {
        assert_ne!(Psk::generate().as_bytes(), Psk::generate().as_bytes());
    }

    #[test]
    fn psk_debug_does_not_leak_key_material() {
        let psk = Psk::generate();
        let rendered = format!("{:?}", psk);
        assert_eq!(rendered, "Psk(<redacted>)");
        assert!(!rendered.contains(&psk.to_base64()));
    }

    #[test]
    fn server_identity_roundtrip_preserves_psk() {
        let kp = KeyPair::generate().unwrap();
        let psk = Psk::generate();
        let dir = std::env::temp_dir().join(format!("phantom_psk_rt_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("server.key");
        let p = path.to_str().unwrap();

        kp.save_secret_with_psk_to_file(p, &psk).unwrap();
        let identity = KeyPair::load_server_identity(p).unwrap();

        assert_eq!(identity.keys.public, kp.public);
        assert_eq!(identity.psk.as_bytes(), psk.as_bytes());
        assert!(
            !identity.psk_generated,
            "an existing PSK must not be regenerated"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A two-line key file predates PSK support: the loader must mint a PSK,
    /// persist it, keep the original key pair, and flag the upgrade.
    #[test]
    fn server_identity_upgrades_legacy_two_line_key_file() {
        let kp = KeyPair::generate().unwrap();
        let dir = std::env::temp_dir().join(format!("phantom_psk_up_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("server.key");
        let p = path.to_str().unwrap();

        kp.save_secret_to_file(p).unwrap();
        assert_eq!(std::fs::read_to_string(p).unwrap().lines().count(), 2);

        let identity = KeyPair::load_server_identity(p).unwrap();
        assert_eq!(identity.keys.public, kp.public, "public key must be stable");
        assert!(identity.psk_generated);

        // The generated PSK must have been written back, so a second load is stable.
        let reloaded = KeyPair::load_server_identity(p).unwrap();
        assert!(!reloaded.psk_generated);
        assert_eq!(reloaded.psk.as_bytes(), identity.psk.as_bytes());
        assert_eq!(std::fs::read_to_string(p).unwrap().lines().count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_identity_rejects_corrupt_psk_line() {
        let kp = KeyPair::generate().unwrap();
        let dir = std::env::temp_dir().join(format!("phantom_psk_bad_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("server.key");
        let p = path.to_str().unwrap();

        // A malformed PSK must surface an error rather than being silently
        // replaced, which would hand out a URI that no client can use.
        std::fs::write(
            p,
            format!(
                "{}\n{}\nnot-a-valid-psk\n",
                kp.public_key_base64(),
                kp.secret_key_base64()
            ),
        )
        .unwrap();
        assert!(KeyPair::load_server_identity(p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
