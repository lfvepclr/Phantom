use crate::{PhantomError, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand_core::OsRng;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use x25519_dalek::{PublicKey, StaticSecret};

use zeroize::Zeroize;

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
        use std::io::Write;
        let mut file = fs::File::create(path)
            .map_err(|e| PhantomError::Crypto(format!("Failed to create key file: {}", e)))?;
        file.lock()
            .map_err(|e| PhantomError::Crypto(format!("Failed to lock key file: {}", e)))?;
        let content = format!(
            "{}\n{}\n",
            self.public_key_base64(),
            self.secret_key_base64()
        );
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
}
