//! phantom:// URI parser for single-string server configuration.
//!
//! Format:
//!   phantom://<base64_public_key>@<host>:<port>[?<query>][#<name>]
//!
//! Query params:
//!   - cipher=auto|aes-256-gcm|aes-128-gcm|ascon-128|chacha20-poly1305
//!   - proto=tcp|quic
//!   - congestion=cubic|bbr|new-reno
//!
//! Example:
//!   phantom://dGVzdA==@example.com:443?cipher=auto&proto=quic#primary

use crate::{
    CipherPreference, PhantomError, Result, ServerEntry, TransportProtocol,
};
use base64::{engine::general_purpose::STANDARD, Engine};

/// Parse a phantom:// URI into a `ServerEntry`.
pub fn parse_phantom_uri(uri: &str) -> Result<ServerEntry> {
    let rest = uri.strip_prefix("phantom://")
        .ok_or_else(|| PhantomError::Config("URI must start with phantom://".to_string()))?;

    // Split userinfo and the rest.
    let (userinfo, rest) = rest.split_once('@')
        .ok_or_else(|| PhantomError::Config("URI missing @ separator".to_string()))?;

    // Decode base64 public key.
    let public_key_bytes = STANDARD.decode(userinfo.trim())
        .map_err(|e| PhantomError::Config(format!("Invalid base64 public key: {}", e)))?;
    if public_key_bytes.len() != 32 {
        return Err(PhantomError::Config(format!(
            "Public key must be 32 bytes, got {}",
            public_key_bytes.len()
        )));
    }
    let public_key = STANDARD.encode(&public_key_bytes);

    // Split host:port from query and fragment.
    let (addr_part, query, name) = parse_addr_query_fragment(rest);

    // Parse query params.
    let mut cipher = CipherPreference::Auto;
    let mut protocol = TransportProtocol::Tcp;
    if let Some(q) = query {
        for param in q.split('&') {
            if let Some((key, value)) = param.split_once('=') {
                match key {
                    "cipher" => cipher = parse_cipher(value)?,
                    "proto" => protocol = parse_protocol(value)?,
                    _ => {}
                }
            }
        }
    }

    Ok(ServerEntry {
        name: name.unwrap_or_else(|| "default".to_string()),
        address: addr_part.to_string(),
        public_key,
        cipher,
        protocol,
    })
}

fn parse_addr_query_fragment(rest: &str) -> (&str, Option<&str>, Option<String>) {
    let (addr_part, rest) = rest.split_once('?').unwrap_or((rest, ""));
    let (addr_part, fragment) = addr_part.split_once('#').unwrap_or((addr_part, ""));
    let query = if rest.is_empty() { None } else { Some(rest) };
    let name = if fragment.is_empty() { None } else { Some(fragment.to_string()) };
    (addr_part, query, name)
}

fn parse_cipher(value: &str) -> Result<CipherPreference> {
    match value {
        "auto" => Ok(CipherPreference::Auto),
        "aes-256-gcm" => Ok(CipherPreference::Aes256Gcm),
        "aes-128-gcm" => Ok(CipherPreference::Aes128Gcm),
        "ascon-128" => Ok(CipherPreference::Ascon128),
        "chacha20-poly1305" => Ok(CipherPreference::ChaCha20Poly1305),
        _ => Err(PhantomError::Config(format!("Unknown cipher: {}", value))),
    }
}

fn parse_protocol(value: &str) -> Result<TransportProtocol> {
    match value {
        "tcp" => Ok(TransportProtocol::Tcp),
        "quic" => Ok(TransportProtocol::Quic),
        _ => Err(PhantomError::Config(format!("Unknown protocol: {}", value))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_uri() {
        // base64("test") = "dGVzdA=="
        let uri = "phantom://dGVzdA==@example.com:443";
        let entry = parse_phantom_uri(uri).unwrap();
        assert_eq!(entry.name, "default");
        assert_eq!(entry.address, "example.com:443");
        assert_eq!(entry.public_key, "dGVzdA==");
        assert_eq!(entry.cipher, CipherPreference::Auto);
        assert_eq!(entry.protocol, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_full_uri() {
        let uri = "phantom://dGVzdA==@example.com:443?cipher=aes-256-gcm&proto=quic#primary";
        let entry = parse_phantom_uri(uri).unwrap();
        assert_eq!(entry.name, "primary");
        assert_eq!(entry.address, "example.com:443");
        assert_eq!(entry.public_key, "dGVzdA==");
        assert_eq!(entry.cipher, CipherPreference::Aes256Gcm);
        assert_eq!(entry.protocol, TransportProtocol::Quic);
    }

    #[test]
    fn parse_invalid_scheme_fails() {
        let uri = "ss://dGVzdA==@example.com:443";
        assert!(parse_phantom_uri(uri).is_err());
    }

    #[test]
    fn parse_bad_base64_fails() {
        let uri = "phantom://!!!@example.com:443";
        assert!(parse_phantom_uri(uri).is_err());
    }
}
