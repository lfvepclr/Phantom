//! Server bootstrap: zero-config auto mode, interactive wizard, and URI-based
//! self-bootstrap.
//!
//! In auto mode (`phantom server` with no flags), the server:
//! 1. Loads `./server.key` from CWD if present, or generates a new one.
//! 2. Probes the bind port (default 443, fallback up to N attempts).
//! 3. Detects the public-facing IP (or uses an override).
//! 4. Writes `./server.toml` containing the resulting `phantom://` URI
//!    (as a top-of-file comment) plus bind/cipher/protocol and the inline
//!    `[[allowed_clients]]` whitelist array.
//! 5. Calls [`crate::run_with_options`] to start the Noise listener.
//!
//! In interactive mode (`phantom server -i`), the same flow is followed but
//! the user is prompted for port / IP / cipher / protocol on stdin.

use anyhow::{anyhow, bail, Context, Result};
use phantom_core::transport::{try_bind_quic_with_fallback, try_bind_tcp_with_fallback};

use phantom_core::{
    build_phantom_uri, parse_phantom_uri, CipherPreference, CongestionAlgorithm, KeyPair,
    ServerConfig, ServerEntry, TransportProtocol,
};
use std::io::{IsTerminal, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use qr2term::print_qr;

use crate::{run_from_uri as run_from_uri_impl, run_with_options, BootstrapOptions};

/// Default port to attempt first.
pub const DEFAULT_PORT: u16 = 443;
/// Default number of consecutive port attempts when probing.
pub const DEFAULT_MAX_PORT_TRIES: u16 = 10;
/// Default name fragment appended to the auto-generated URI.
pub const DEFAULT_URI_NAME: &str = "default";

/// Options the CLI passes into the bootstrap layer. The CLI fills these from
/// its `--port` / `--public-host` / `--cipher` / `--proto` flags; `None` means
/// "use the default".
#[derive(Debug, Clone, Default)]
pub struct AutoOptions {
    /// Public-facing host written into the URI comment in `server.toml`.
    /// Falls back to outbound IP detection, then to `0.0.0.0` (with a warning).
    pub public_host: Option<String>,
    /// First port to try. Defaults to 443.
    pub start_port: Option<u16>,
    /// Cipher override. Defaults to `Auto`.
    pub cipher: Option<CipherPreference>,
    /// Protocol override. Defaults to TCP.
    pub protocol: Option<TransportProtocol>,
    /// Number of consecutive ports to try before giving up. Defaults to 10.
    pub max_port_tries: Option<u16>,
}

impl AutoOptions {
    fn start_port(&self) -> u16 {
        self.start_port.unwrap_or(DEFAULT_PORT)
    }

    fn max_tries(&self) -> u16 {
        self.max_port_tries.unwrap_or(DEFAULT_MAX_PORT_TRIES)
    }

    fn cipher(&self) -> CipherPreference {
        self.cipher.unwrap_or_default()
    }

    fn protocol(&self) -> TransportProtocol {
        self.protocol.unwrap_or_default()
    }
}

/// Auto-bootstrap the server in CWD. See module docs for the full flow.
pub async fn run_auto(opts: AutoOptions) -> Result<()> {
    let paths = BootstrapPaths::resolve()?;
    let kp = load_or_generate_key(&paths.key_path)?;
    // Pre-existing server.toml (if any) may carry an inline whitelist; pick
    // that up so re-running auto mode preserves user edits.
    let allowed = load_allowed_clients_from_toml(&paths.toml_path);

    let cipher = opts.cipher();
    let protocol = opts.protocol();
    let start_port = opts.start_port();
    let max_tries = opts.max_tries();

    let (bound_port, _proto_ctx) = probe_port(protocol, start_port, max_tries)
        .await
        .with_context(|| {
            format!(
                "auto-bootstrap failed to find a free port starting at {start_port}"
            )
        })?;

    let public_host = resolve_public_host(opts.public_host.as_deref());
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), bound_port);

    let uri = build_phantom_uri(
        &kp.public_key_base64(),
        &format!("{public_host}:{bound_port}"),
        cipher,
        protocol,
        Some(DEFAULT_URI_NAME),
    );

    write_server_toml(
        &paths.toml_path,
        &paths.key_path,
        TomlSnapshot {
            bind,
            cipher,
            protocol,
            uri: &uri,
            allowed: &allowed,
        },
    )?;

    print_summary(SummaryInfo {
        key_path: &paths.key_path,
        toml_path: &paths.toml_path,
        bind,
        uri: &uri,
        allowed_count: allowed.len(),
    });

    let opts = BootstrapOptions {
        bind,
        secret_key: kp.secret,
        allowed_clients: allowed,
        cipher,
        protocol,
        quic_congestion: CongestionAlgorithm::default(),
        io_uring: false,
    };

    run_with_options(opts).await
}

/// Interactive bootstrap: ask the user for port / IP / cipher / protocol on
/// stdin, then run the same flow as [`run_auto`]. Returns an error if stdin
/// is not a TTY.
pub async fn run_interactive(mut opts: AutoOptions) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "interactive mode requires a TTY on stdin. \
             Use `phantom server` (auto) or `phantom server -c <file>` instead."
        );
    }

    let paths = BootstrapPaths::resolve()?;

    // Port prompt (loops on fallback failure).
    let start_port = loop {
        let raw = prompt(&format!("Listen port [{DEFAULT_PORT}]: "))?;
        let parsed = match raw.as_str() {
            "" => DEFAULT_PORT,
            _ => match raw.parse::<u16>() {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("Invalid port: {raw}");
                    continue;
                }
            },
        };
        break parsed;
    };
    opts.start_port = Some(start_port);

    let ip_raw = prompt("Public host / IP [auto-detect]: ")?;
    if !ip_raw.is_empty() {
        opts.public_host = Some(ip_raw);
    }

    let cipher_raw = prompt("Cipher (auto / aes-256-gcm / aes-128-gcm / ascon-128 / chacha20-poly1305) [auto]: ")?;
    if !cipher_raw.is_empty() {
        opts.cipher = Some(parse_cipher_interactive(&cipher_raw)?);
    }

    let proto_raw = prompt("Protocol (tcp / quic) [tcp]: ")?;
    if !proto_raw.is_empty() {
        opts.protocol = Some(parse_protocol_interactive(&proto_raw)?);
    }

    opts.max_port_tries = Some(DEFAULT_MAX_PORT_TRIES);

    let kp = load_or_generate_key(&paths.key_path)?;
    let allowed = load_allowed_clients_from_toml(&paths.toml_path);

    let cipher = opts.cipher();
    let protocol = opts.protocol();
    let max_tries = opts.max_tries();

    let (bound_port, _) = match probe_port(protocol, start_port, max_tries).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            bail!("No free port found — please re-run and choose another port.");
        }
    };

    let public_host = resolve_public_host(opts.public_host.as_deref());
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), bound_port);

    let uri = build_phantom_uri(
        &kp.public_key_base64(),
        &format!("{public_host}:{bound_port}"),
        cipher,
        protocol,
        Some(DEFAULT_URI_NAME),
    );

    write_server_toml(
        &paths.toml_path,
        &paths.key_path,
        TomlSnapshot {
            bind,
            cipher,
            protocol,
            uri: &uri,
            allowed: &allowed,
        },
    )?;
    print_summary(SummaryInfo {
        key_path: &paths.key_path,
        toml_path: &paths.toml_path,
        bind,
        uri: &uri,
        allowed_count: allowed.len(),
    });

    let opts = BootstrapOptions {
        bind,
        secret_key: kp.secret,
        allowed_clients: allowed,
        cipher,
        protocol,
        quic_congestion: CongestionAlgorithm::default(),
        io_uring: false,
    };

    run_with_options(opts).await
}

/// Self-bootstrap helper: thin wrapper around [`crate::run_from_uri`] that
/// resolves the CWD-relative key path. The whitelist is read from
/// `server.toml` (if present) by the underlying `run_from_uri_impl`.
pub async fn run_from_uri(uri: &str) -> Result<()> {
    let paths = BootstrapPaths::resolve()?;
    let key_path_str = paths
        .key_path
        .to_str()
        .ok_or_else(|| anyhow!("non-UTF8 key path"))?;
    // First sanity-check: ensure URI's public key matches the local key.
    let entry: ServerEntry = parse_phantom_uri(uri)
        .map_err(|e| anyhow!("Failed to parse server URI: {}", e))?;
    if !paths.key_path.exists() {
        bail!(
            "URI bootstrap requires an existing key file at {}, but it does not exist",
            key_path_str
        );
    }
    let kp = KeyPair::load_secret_from_file(key_path_str)
        .map_err(|e| anyhow!("Failed to load {}: {}", key_path_str, e))?;
    if kp.public_key_base64() != entry.public_key {
        bail!(
            "Public key mismatch: URI has {}, local key has {}",
            entry.public_key,
            kp.public_key_base64()
        );
    }
    tracing::info!("URI public key matches local key — bootstrapping");

    run_from_uri_impl(uri, key_path_str).await
}

// === Internal helpers ===

struct BootstrapPaths {
    key_path: PathBuf,
    toml_path: PathBuf,
}

impl BootstrapPaths {
    fn resolve() -> Result<Self> {
        let cwd = std::env::current_dir().context("failed to read current directory")?;
        Ok(Self {
            key_path: cwd.join("server.key"),
            toml_path: cwd.join("server.toml"),
        })
    }
}

fn load_or_generate_key(path: &Path) -> Result<KeyPair> {
    if path.exists() {
        let p = path
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF8 key path: {}", path.display()))?;
        let kp = KeyPair::load_secret_from_file(p)
            .with_context(|| format!("failed to load existing key {}", path.display()))?;
        tracing::info!("Reusing existing key from {}", path.display());
        Ok(kp)
    } else {
        let kp = KeyPair::generate().context("failed to generate key pair")?;
        let p = path
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF8 key path: {}", path.display()))?;
        kp.save_secret_to_file(p)
            .with_context(|| format!("failed to write key to {}", path.display()))?;
        tracing::info!("Generated new key pair: {}", path.display());
        Ok(kp)
    }
}

/// Read the inline `[[allowed_clients]]` whitelist from a server.toml file.
/// Returns an empty vec (and logs a warning) if the file does not exist or
/// the section is empty / not present. Used by both auto and interactive
/// bootstrap so user edits to the whitelist survive restarts.
fn load_allowed_clients_from_toml(path: &Path) -> Vec<[u8; 32]> {
    if !path.exists() {
        return Vec::new(); // first run; auto-bootstrap will create the file
    }
    let p = match path.to_str() {
        Some(s) => s,
        None => {
            tracing::warn!("Non-UTF8 server.toml path; running in open mode");
            return Vec::new();
        }
    };
    match ServerConfig::load(p) {
        Ok(cfg) => match cfg.load_allowed_clients() {
            Ok(list) => {
                if list.is_empty() {
                    tracing::info!(
                        "{} has no [[allowed_clients]] entries — running in OPEN mode",
                        path.display()
                    );
                } else {
                    tracing::info!(
                        "Loaded {} allowed client key(s) from {}",
                        list.len(),
                        path.display()
                    );
                }
                list
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to load allowed_clients from {}: {}. Running in open mode.",
                    path.display(),
                    e
                );
                Vec::new()
            }
        },
        Err(e) => {
            tracing::warn!(
                "Failed to parse {}: {}. Running in open mode.",
                path.display(),
                e
            );
            Vec::new()
        }
    }
}

async fn probe_port(
    protocol: TransportProtocol,
    start_port: u16,
    max_tries: u16,
) -> Result<(u16, ProtocolContext)> {
    let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), start_port);
    match protocol {
        TransportProtocol::Tcp => {
            let (_listener, bound) = try_bind_tcp_with_fallback(start, max_tries)
                .await
                .map_err(|e| anyhow!("{e}"))?;
            // _listener is dropped at the end of this scope, releasing the
            // bind. The actual `run_with_options` rebinds it via its own
            // listener path.
            Ok((bound.port(), ProtocolContext::Tcp))
        }
        TransportProtocol::Quic => {
            let (_listener, bound) =
                try_bind_quic_with_fallback(start, max_tries, CongestionAlgorithm::default())
                    .await
                    .map_err(|e| anyhow!("{e}"))?;
            Ok((bound.port(), ProtocolContext::Quic))
        }
    }
}

enum ProtocolContext {
    Tcp,
    Quic,
}

/// Snapshot of the values we emit into `server.toml` after bootstrap.
struct TomlSnapshot<'a> {
    bind: SocketAddr,
    cipher: CipherPreference,
    protocol: TransportProtocol,
    uri: &'a str,
    allowed: &'a [[u8; 32]],
}

/// Write the generated `server.toml`. The file is intentionally annotated
/// with the URI quick-link at the top so the operator can copy it for
/// clients without digging through the rest of the file.
fn write_server_toml(
    path: &Path,
    key_path: &Path,
    snap: TomlSnapshot<'_>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create directory {}", parent.display())
            })?;
        }
    }

    let bind_ip = snap.bind.ip().to_string();
    let port = snap.bind.port();
    let cipher = cipher_to_toml(snap.cipher);
    let protocol = protocol_to_toml(snap.protocol);

    let mut body = String::new();
    body.push_str("# Phantom server config (auto-bootstrap generated)\n");
    body.push_str("# ===============================================\n");
    body.push_str("#\n");
    body.push_str("# Quick link URI (distribute to clients):\n");
    body.push_str(&format!("#   {}\n", snap.uri));
    body.push_str("#\n");
    body.push_str(&format!(
        "# The private key lives in {} (mode 600, do not share).\n",
        key_path.display()
    ));
    body.push_str(
        "# Edit bind / cipher / protocol below and restart the server to apply changes.\n",
    );
    body.push_str("# Delete this file to reset to zero-config behaviour on the next start.\n");
    body.push_str("\n");
    body.push_str(&format!("bind = \"{bind_ip}:{port}\"\n"));
    body.push_str(&format!("cipher = \"{cipher}\"\n"));
    body.push_str(&format!("protocol = \"{protocol}\"\n"));
    body.push_str("\n");
    body.push_str("# --- Client whitelist ----------------------------------------------------\n");
    body.push_str(
        "# Add base64 X25519 public keys below. Empty list = OPEN mode (any client\n",
    );
    body.push_str(
        "# with the server's public key can connect). Reload by restarting the server.\n",
    );
    body.push_str("# Example:\n");
    body.push_str("#   [[allowed_clients]]\n");
    body.push_str("#   public_key = \"abc123...44chars...\"\n");
    body.push_str("#   name = \"alice-laptop\"   # optional human-readable label\n");
    body.push_str("# --------------------------------------------------------------------------\n");
    if snap.allowed.is_empty() {
        body.push_str("# (no clients whitelisted; server is in OPEN mode)\n");
    } else {
        for key in snap.allowed {
            let b64 = B64.encode(key);
            body.push_str("\n[[allowed_clients]]\n");
            body.push_str(&format!("public_key = \"{b64}\"\n"));
        }
    }

    let p = path
        .to_str()
        .ok_or_else(|| anyhow!("non-UTF8 server.toml path: {}", path.display()))?;
    std::fs::write(p, body)
        .with_context(|| format!("failed to write server.toml to {}", path.display()))?;
    tracing::info!("Wrote server.toml to {}", path.display());
    Ok(())
}

fn cipher_to_toml(c: CipherPreference) -> &'static str {
    match c {
        CipherPreference::Auto => "auto",
        CipherPreference::Aes256Gcm => "aes-256-gcm",
        CipherPreference::Aes128Gcm => "aes-128-gcm",
        CipherPreference::Ascon128 => "ascon-128",
        CipherPreference::ChaCha20Poly1305 => "chacha20-poly1305",
    }
}

fn protocol_to_toml(p: TransportProtocol) -> &'static str {
    match p {
        TransportProtocol::Tcp => "tcp",
        TransportProtocol::Quic => "quic",
    }
}

struct SummaryInfo<'a> {
    key_path: &'a Path,
    toml_path: &'a Path,
    bind: SocketAddr,
    uri: &'a str,
    allowed_count: usize,
}

fn print_summary(info: SummaryInfo<'_>) {
    let mode = if info.allowed_count == 0 {
        "OPEN (no whitelist)"
    } else {
        "WHITELIST"
    };
    println!();
    println!("=== Phantom server bootstrapped ===");
    println!("  key file     : {}", info.key_path.display());
    println!("  config file  : {}", info.toml_path.display());
    println!("  whitelist    : {} ({})", info.toml_path.display(), mode);
    println!("  bind address : {}", info.bind);
    println!("  URI link     : {}", info.uri);
    println!();
    println!("Scan the QR code below with a Phantom client to import the URI:");
    print_qr_code(info.uri);
    println!();
    println!("Or copy it manually:");
    println!("  phantom client --server \"{}\"", info.uri);
    println!();
}

/// Render the URI as a Unicode-block QR code on stdout.
///
/// Failure is non-fatal: a broken QR never blocks server bootstrap.  We log
/// the error and let the operator copy the text URI from the line above.
/// Public so the load-mode entry point in `lib::run` can also emit one.
pub fn print_qr_code(uri: &str) {
    if let Err(e) = print_qr(uri) {
        tracing::warn!("failed to render QR code: {e}");
    }
}

fn resolve_public_host(override_host: Option<&str>) -> String {
    if let Some(h) = override_host {
        if !h.is_empty() {
            return h.to_string();
        }
    }
    match detect_outbound_ip() {
        Some(ip) => ip.to_string(),
        None => {
            tracing::warn!(
                "Could not detect public IP; defaulting to 0.0.0.0 in server.toml. \
                 Override with --public-host."
            );
            "0.0.0.0".to_string()
        }
    }
}

/// Detect the local outbound IP by opening a UDP socket and "connecting" to
/// a public address (no packets are actually sent). Returns `None` on failure.
fn detect_outbound_ip() -> Option<IpAddr> {
    use std::net::ToSocketAddrs;
    let target = "8.8.8.8:80".to_socket_addrs().ok()?.next()?;
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect(target).ok()?;
    let local = socket.local_addr().ok()?;
    Some(local.ip())
}

fn prompt(question: &str) -> Result<String> {
    let mut stdout = std::io::stdout();
    stdout.write_all(question.as_bytes()).ok();
    stdout.flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("failed to read from stdin")?;
    Ok(line.trim().to_string())
}

fn parse_cipher_interactive(s: &str) -> Result<CipherPreference> {
    match s.trim() {
        "auto" => Ok(CipherPreference::Auto),
        "aes-256-gcm" => Ok(CipherPreference::Aes256Gcm),
        "aes-128-gcm" => Ok(CipherPreference::Aes128Gcm),
        "ascon-128" => Ok(CipherPreference::Ascon128),
        "chacha20-poly1305" => Ok(CipherPreference::ChaCha20Poly1305),
        other => bail!("Unknown cipher: {other}"),
    }
}

fn parse_protocol_interactive(s: &str) -> Result<TransportProtocol> {
    match s.trim() {
        "tcp" => Ok(TransportProtocol::Tcp),
        "quic" => Ok(TransportProtocol::Quic),
        other => bail!("Unknown protocol: {other}"),
    }
}

// Expose helpers for tests.
#[cfg(test)]
mod test_helpers {
    use super::load_allowed_clients_from_toml;
    use std::path::Path;

    /// Thin wrapper used by tests: returns the inline whitelist parsed from
    /// a server.toml, or an empty vec if the file is missing.
    pub(crate) fn load_allowed_clients_or_empty(path: &Path) -> Vec<[u8; 32]> {
        load_allowed_clients_from_toml(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::path::PathBuf;

    /// Helper: build a SocketAddr for tests without actually binding.
    fn dummy_bind() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 8443)
    }

    #[test]
    fn auto_options_defaults() {
        let o = AutoOptions::default();
        assert_eq!(o.start_port(), DEFAULT_PORT);
        assert_eq!(o.max_tries(), DEFAULT_MAX_PORT_TRIES);
        assert_eq!(o.cipher(), CipherPreference::Auto);
        assert_eq!(o.protocol(), TransportProtocol::Tcp);
    }

    #[test]
    fn auto_options_overrides() {
        let o = AutoOptions {
            start_port: Some(9000),
            max_port_tries: Some(3),
            cipher: Some(CipherPreference::Aes256Gcm),
            protocol: Some(TransportProtocol::Quic),
            public_host: Some("example.com".to_string()),
        };
        assert_eq!(o.start_port(), 9000);
        assert_eq!(o.max_tries(), 3);
        assert_eq!(o.cipher(), CipherPreference::Aes256Gcm);
        assert_eq!(o.protocol(), TransportProtocol::Quic);
    }

    #[test]
    fn parse_allowed_clients_from_toml_empty_section() {
        // server.toml with no [[allowed_clients]] -> 0 keys.
        let dir = std::env::temp_dir().join(format!(
            "phantom_allowed_empty_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("server.toml");
        std::fs::write(
            &path,
            r#"bind = "0.0.0.0:443"
cipher = "auto"
"#,
        )
        .unwrap();
        let parsed = test_helpers::load_allowed_clients_or_empty(&path);
        assert!(parsed.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_allowed_clients_from_toml_real_key() {
        // base64 of 32 zero bytes.
        let b64_str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        assert_eq!(b64_str.len(), 44);
        let key = [0u8; 32];
        let dir = std::env::temp_dir().join(format!(
            "phantom_allowed_real_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("server.toml");
        std::fs::write(
            &path,
            format!(
                r#"bind = "0.0.0.0:443"

[[allowed_clients]]
public_key = "{b64_str}"
name = "alice"
"#,
            ),
        )
        .unwrap();
        let parsed = test_helpers::load_allowed_clients_or_empty(&path);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], key);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_allowed_clients_from_toml_missing_file_is_open() {
        let dir = std::env::temp_dir().join(format!(
            "phantom_allowed_missing_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("server.toml"); // never created
        let parsed = test_helpers::load_allowed_clients_or_empty(&path);
        assert!(parsed.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bootstrap_paths_use_cwd() {
        // We can't change CWD safely in a test, but we can at least
        // confirm the returned paths have the right file names.
        let p = BootstrapPaths::resolve().unwrap();
        assert_eq!(p.key_path.file_name().unwrap(), "server.key");
        assert_eq!(p.toml_path.file_name().unwrap(), "server.toml");
    }

    #[test]
    fn write_server_toml_writes_uri_header_and_whitelist() {
        let dir = std::env::temp_dir().join(format!(
            "phantom_bootstrap_toml_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("server.toml");
        let key_path = PathBuf::from("./server.key");
        let uri = "phantom://abc@example.com:443?cipher=auto&proto=tcp#default";
        let allowed: Vec<[u8; 32]> = vec![[0u8; 32]];
        write_server_toml(
            &path,
            &key_path,
            TomlSnapshot {
                bind: dummy_bind(),
                cipher: CipherPreference::Auto,
                protocol: TransportProtocol::Tcp,
                uri,
                allowed: &allowed,
            },
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Header must contain the URI quick link.
        assert!(body.contains(uri), "URI quick link missing from server.toml");
        // Header must point at the key file.
        assert!(body.contains("server.key"));
        // The bind field must reflect dummy_bind (0.0.0.0:8443).
        assert!(body.contains("bind = \"0.0.0.0:8443\""));
        // cipher + protocol defaults must be present.
        assert!(body.contains("cipher = \"auto\""));
        assert!(body.contains("protocol = \"tcp\""));
        // The whitelist must be emitted as [[allowed_clients]] entries.
        assert!(body.contains("[[allowed_clients]]"));
        assert!(body.contains("public_key ="));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_server_toml_open_mode_omits_entries() {
        let dir = std::env::temp_dir().join(format!(
            "phantom_bootstrap_open_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("server.toml");
        let key_path = PathBuf::from("./server.key");
        let uri = "phantom://abc@example.com:443?cipher=auto&proto=tcp#default";
        let empty: Vec<[u8; 32]> = Vec::new();
        write_server_toml(
            &path,
            &key_path,
            TomlSnapshot {
                bind: dummy_bind(),
                cipher: CipherPreference::Auto,
                protocol: TransportProtocol::Tcp,
                uri,
                allowed: &empty,
            },
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("(no clients whitelisted; server is in OPEN mode)"));
        // The doc comment shows an example `[[allowed_clients]]` table,
        // so we cannot assert its absence. Instead check that no real
        // (uncommented) `public_key =` line was emitted.
        for line in body.lines() {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.starts_with("public_key ="),
                "OPEN mode must not emit a public_key line, found: {line}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_public_host_uses_override() {
        let host = resolve_public_host(Some("my.example.com"));
        assert_eq!(host, "my.example.com");
    }

    #[test]
    fn resolve_public_host_empty_override_detects() {
        // Empty override should not be treated as a real value; falls back
        // to detection (which on most envs returns a real IP).
        let host = resolve_public_host(Some(""));
        assert!(!host.is_empty());
    }

    #[test]
    fn parse_cipher_interactive_accepts_all() {
        assert_eq!(
            parse_cipher_interactive("auto").unwrap(),
            CipherPreference::Auto
        );
        assert_eq!(
            parse_cipher_interactive("aes-256-gcm").unwrap(),
            CipherPreference::Aes256Gcm
        );
        assert_eq!(
            parse_cipher_interactive("ascon-128").unwrap(),
            CipherPreference::Ascon128
        );
        assert!(parse_cipher_interactive("nonsense").is_err());
    }

    #[test]
    fn parse_protocol_interactive_accepts_both() {
        assert_eq!(
            parse_protocol_interactive("tcp").unwrap(),
            TransportProtocol::Tcp
        );
        assert_eq!(
            parse_protocol_interactive("quic").unwrap(),
            TransportProtocol::Quic
        );
        assert!(parse_protocol_interactive("ws").is_err());
    }

    #[test]
    fn detect_outbound_ip_returns_something() {
        // Best-effort: on most networks this returns a non-loopback IP.
        // We accept any result (including None) to keep the test robust.
        let _ = detect_outbound_ip();
    }

    #[test]
    fn summary_info_constructs() {
        // Smoke test that the struct is constructible.
        let p = PathBuf::from("server.key");
        let _ = SummaryInfo {
            key_path: &p,
            toml_path: &p,
            bind: dummy_bind(),
            uri: "phantom://x@y:1?cipher=auto&proto=tcp#n",
            allowed_count: 0,
        };
    }

    /// Smoke test for QR rendering — just make sure it doesn't panic on a
    /// representative URI. Visual correctness is verified manually by scanning
    /// the printed QR with a phone after running `phantom server`.
    #[test]
    fn print_qr_code_does_not_panic_on_valid_uri() {
        let uri = "phantom://YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=@example.com:443?cipher=auto&proto=tcp#test";
        super::print_qr_code(uri);
    }
}
