use cfg_if::cfg_if;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CipherSuite {
    /// HW-accelerated AES (AES-NI / ARMv8 CE with the `aes_armv8` build flag):
    /// ~1.6-3 GB/s with intrinsics; pure-software fallback is only ~177 MiB/s
    Aes256Gcm = 0x01,
    /// Balanced: mid-range ARM with AES CE
    Aes128Gcm = 0x02,
    /// NIST lightweight AEAD, software-only, ~540-640 MiB/s
    Ascon128 = 0x03,
    /// NEON-accelerated software cipher (`chacha20_force_neon` build flag):
    /// ~650-730 MiB/s; fastest option without hardware AES
    ChaCha20Poly = 0x04,
}

impl CipherSuite {
    /// Auto-select the best cipher for this build + CPU.
    ///
    /// Requires BOTH compile-time and runtime evidence of hardware AES:
    /// the `aes` crate only ships its ARMv8 intrinsics backend when built
    /// with `RUSTFLAGS="--cfg=aes_armv8"` (see .cargo/config.toml) — without
    /// the flag the runtime CPU probe alone would select AES-256-GCM and then
    /// run the software fixslice backend (~177 MiB/s, slower than ChaCha).
    ///
    /// Fallback order (no usable hardware AES): ChaCha20-Poly1305 — with the
    /// `chacha20_force_neon` build flag its NEON backend is the fastest
    /// software path (measured ~650-730 MiB/s vs ASCON ~540-640 MiB/s).
    pub fn auto_detect() -> Self {
        cfg_if! {
            if #[cfg(any(target_arch = "x86_64", target_arch = "x86"))] {
                // The aes crate's x86 AES-NI backend is enabled by default
                // (runtime-detected), so the CPU probe alone is sufficient.
                if is_x86_feature_detected!("aes") {
                    return CipherSuite::Aes256Gcm;
                }
            }
        }
        cfg_if! {
            if #[cfg(target_arch = "aarch64")] {
                if cfg!(aes_armv8) && std::arch::is_aarch64_feature_detected!("aes") {
                    return CipherSuite::Aes256Gcm;
                }
            }
        }
        CipherSuite::ChaCha20Poly
    }

    pub fn key_len(self) -> usize {
        match self {
            Self::Aes256Gcm => 32,
            Self::Aes128Gcm => 16,
            Self::Ascon128 => 16,
            Self::ChaCha20Poly => 32,
        }
    }

    pub fn nonce_len(self) -> usize {
        match self {
            Self::Aes256Gcm | Self::Aes128Gcm | Self::ChaCha20Poly => 12,
            Self::Ascon128 => 16,
        }
    }

    pub fn tag_len(self) -> usize {
        16 // All supported AEADs produce 16-byte auth tags
    }

    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0x01 => Some(Self::Aes256Gcm),
            0x02 => Some(Self::Aes128Gcm),
            0x03 => Some(Self::Ascon128),
            0x04 => Some(Self::ChaCha20Poly),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Aes256Gcm => "AES-256-GCM",
            Self::Aes128Gcm => "AES-128-GCM",
            Self::Ascon128 => "ASCON-128",
            Self::ChaCha20Poly => "ChaCha20-Poly1305",
        }
    }

    /// Priority-ordered list for cipher offers.
    pub fn all_ordered() -> &'static [CipherSuite] {
        &[
            CipherSuite::Aes256Gcm,
            CipherSuite::Aes128Gcm,
            CipherSuite::Ascon128,
            CipherSuite::ChaCha20Poly,
        ]
    }

    /// Build a default offer list based on local capabilities.
    pub fn default_offer() -> Vec<CipherSuite> {
        let detected = Self::auto_detect();
        let mut offer = vec![detected];
        for &cs in Self::all_ordered() {
            if cs != detected {
                offer.push(cs);
            }
        }
        offer
    }
}

impl std::fmt::Display for CipherSuite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_u8() {
        for &cs in CipherSuite::all_ordered() {
            assert_eq!(CipherSuite::from_u8(cs.to_u8()), Some(cs));
        }
        assert_eq!(CipherSuite::from_u8(0x00), None);
        assert_eq!(CipherSuite::from_u8(0xFF), None);
    }

    #[test]
    fn key_nonce_tag_lengths() {
        assert_eq!(CipherSuite::Aes256Gcm.key_len(), 32);
        assert_eq!(CipherSuite::Aes256Gcm.nonce_len(), 12);
        assert_eq!(CipherSuite::Aes128Gcm.key_len(), 16);
        assert_eq!(CipherSuite::Ascon128.key_len(), 16);
        assert_eq!(CipherSuite::Ascon128.nonce_len(), 16);
        assert_eq!(CipherSuite::ChaCha20Poly.key_len(), 32);
        for &cs in CipherSuite::all_ordered() {
            assert_eq!(cs.tag_len(), 16);
        }
    }

    #[test]
    fn default_offer_starts_with_detected() {
        let offer = CipherSuite::default_offer();
        assert_eq!(offer[0], CipherSuite::auto_detect());
        assert_eq!(offer.len(), 4);
    }

    #[test]
    fn display_names() {
        assert_eq!(CipherSuite::Aes256Gcm.to_string(), "AES-256-GCM");
        assert_eq!(CipherSuite::Ascon128.to_string(), "ASCON-128");
    }
}
