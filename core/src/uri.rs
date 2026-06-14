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

use crate::{CipherPreference, PhantomError, Result, ServerEntry, TransportProtocol};
use base64::{Engine, engine::general_purpose::STANDARD};

/// Parse a phantom:// URI into a `ServerEntry`.
pub fn parse_phantom_uri(uri: &str) -> Result<ServerEntry> {
    let rest = uri
        .strip_prefix("phantom://")
        .ok_or_else(|| PhantomError::Config("URI must start with phantom://".to_string()))?;

    // Split userinfo and the rest.
    let (userinfo, rest) = rest
        .split_once('@')
        .ok_or_else(|| PhantomError::Config("URI missing @ separator".to_string()))?;

    // Decode base64 public key.
    let public_key_bytes = STANDARD
        .decode(userinfo.trim())
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
    // URI format: host:port?query#fragment
    // Split fragment first (it comes last), then query.
    let (before_fragment, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (addr_part, query_part) = before_fragment
        .split_once('?')
        .unwrap_or((before_fragment, ""));
    let query = if query_part.is_empty() {
        None
    } else {
        Some(query_part)
    };
    let name = if fragment.is_empty() {
        None
    } else {
        Some(fragment.to_string())
    };
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

/// Reverse mapping of [`parse_cipher`]: convert a [`CipherPreference`] to its
/// kebab-case URI form. Used by [`build_phantom_uri`].
fn cipher_to_str(cipher: CipherPreference) -> &'static str {
    match cipher {
        CipherPreference::Auto => "auto",
        CipherPreference::Aes256Gcm => "aes-256-gcm",
        CipherPreference::Aes128Gcm => "aes-128-gcm",
        CipherPreference::Ascon128 => "ascon-128",
        CipherPreference::ChaCha20Poly1305 => "chacha20-poly1305",
    }
}

/// Reverse mapping of [`parse_protocol`]: convert a [`TransportProtocol`] to
/// its URI form.
fn protocol_to_str(protocol: TransportProtocol) -> &'static str {
    match protocol {
        TransportProtocol::Tcp => "tcp",
        TransportProtocol::Quic => "quic",
    }
}

/// Build a `phantom://` URI from its parts. This is the inverse of
/// [`parse_phantom_uri`] and is used by the server bootstrap to emit the
/// `server.toml` quick-link comment.
///
/// Format:
///   `phantom://<base64_public_key>@<host>:<port>?cipher=<c>&proto=<p>[#<name>]`
///
/// When `name` is `None` or empty, no fragment is appended.
pub fn build_phantom_uri(
    public_key_base64: &str,
    address: &str,
    cipher: CipherPreference,
    protocol: TransportProtocol,
    name: Option<&str>,
) -> String {
    let mut uri = format!(
        "phantom://{}@{}?cipher={}&proto={}",
        public_key_base64,
        address,
        cipher_to_str(cipher),
        protocol_to_str(protocol),
    );
    if let Some(n) = name {
        if !n.is_empty() {
            uri.push('#');
            uri.push_str(n);
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_uri() {
        // base64("abcdefghijklmnopqrstuvwxyz123456") = 32 bytes, valid key
        let uri = "phantom://YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=@example.com:443";
        let entry = parse_phantom_uri(uri).unwrap();
        assert_eq!(entry.name, "default");
        assert_eq!(entry.address, "example.com:443");
        assert_eq!(
            entry.public_key,
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY="
        );
        assert_eq!(entry.cipher, CipherPreference::Auto);
        assert_eq!(entry.protocol, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_full_uri() {
        let uri = "phantom://YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=@example.com:443?cipher=aes-256-gcm&proto=quic#primary";
        let entry = parse_phantom_uri(uri).unwrap();
        assert_eq!(entry.name, "primary");
        assert_eq!(entry.address, "example.com:443");
        assert_eq!(
            entry.public_key,
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY="
        );
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

    #[test]
    fn parse_wrong_scheme_fails() {
        let uri = "ss://dGVzdA==@example.com:443";
        assert!(parse_phantom_uri(uri).is_err());
    }

    #[test]
    fn parse_short_key_fails() {
        // "dGVzdA==" decodes to "test" (4 bytes), not 32
        let uri = "phantom://dGVzdA==@example.com:443";
        assert!(parse_phantom_uri(uri).is_err());
    }

    #[test]
    fn build_minimal_uri() {
        // No name → no fragment; cipher defaults to auto / proto tcp via call-site.
        let uri = build_phantom_uri(
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=",
            "example.com:443",
            CipherPreference::Auto,
            TransportProtocol::Tcp,
            None,
        );
        assert_eq!(
            uri,
            "phantom://YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=@example.com:443?cipher=auto&proto=tcp"
        );
    }

    #[test]
    fn build_full_uri() {
        let uri = build_phantom_uri(
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=",
            "example.com:443",
            CipherPreference::Aes256Gcm,
            TransportProtocol::Quic,
            Some("primary"),
        );
        assert_eq!(
            uri,
            "phantom://YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=@example.com:443?cipher=aes-256-gcm&proto=quic#primary"
        );
    }

    #[test]
    fn build_uri_no_name() {
        let uri = build_phantom_uri(
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=",
            "1.2.3.4:443",
            CipherPreference::ChaCha20Poly1305,
            TransportProtocol::Tcp,
            None,
        );
        assert!(!uri.contains('#'));
        assert!(uri.contains("cipher=chacha20-poly1305"));
    }

    #[test]
    fn build_uri_empty_name_omits_fragment() {
        let uri = build_phantom_uri(
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=",
            "example.com:443",
            CipherPreference::Auto,
            TransportProtocol::Tcp,
            Some(""),
        );
        assert!(!uri.ends_with('#'));
    }

    #[test]
    fn build_uri_roundtrip() {
        // Build then parse; all fields should agree.
        let original = build_phantom_uri(
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=",
            "example.com:443",
            CipherPreference::Ascon128,
            TransportProtocol::Quic,
            Some("primary"),
        );
        let parsed = parse_phantom_uri(&original).expect("round-trip parse");
        assert_eq!(parsed.name, "primary");
        assert_eq!(parsed.address, "example.com:443");
        assert_eq!(
            parsed.public_key,
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY="
        );
        assert_eq!(parsed.cipher, CipherPreference::Ascon128);
        assert_eq!(parsed.protocol, TransportProtocol::Quic);
    }

    #[test]
    fn cipher_to_str_kebab_case() {
        assert_eq!(cipher_to_str(CipherPreference::Auto), "auto");
        assert_eq!(cipher_to_str(CipherPreference::Aes256Gcm), "aes-256-gcm");
        assert_eq!(cipher_to_str(CipherPreference::Aes128Gcm), "aes-128-gcm");
        assert_eq!(cipher_to_str(CipherPreference::Ascon128), "ascon-128");
        assert_eq!(
            cipher_to_str(CipherPreference::ChaCha20Poly1305),
            "chacha20-poly1305"
        );
    }
}
