use crate::error::{PhantomError, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use std::fs;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CipherPreference {
    Auto,
    Aes256Gcm,
    Aes128Gcm,
    Ascon128,
    ChaCha20Poly1305,
}

impl Default for CipherPreference {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CongestionAlgorithm {
    Cubic,
    Bbr,
    NewReno,
}

impl Default for CongestionAlgorithm {
    fn default() -> Self {
        Self::Cubic
    }
}

// === Client Config ===

#[derive(Debug, Clone, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
    #[serde(default)]
    pub client: ClientSettings,
    #[serde(default)]
    pub failover: FailoverConfig,
    #[serde(default)]
    pub rules: RulesConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientSettings {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_dns")]
    pub dns: String,
    #[serde(default = "default_proxy_mode")]
    pub mode: ProxyMode,
    #[serde(default)]
    pub cipher: CipherPreference,
}

fn default_listen() -> String {
    "127.0.0.1:1080".to_string()
}

fn default_dns() -> String {
    "tls://8.8.8.8:853".to_string()
}

fn default_proxy_mode() -> ProxyMode {
    ProxyMode::Smart
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            dns: default_dns(),
            mode: default_proxy_mode(),
            cipher: CipherPreference::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    Auto,
    Direct,
    Proxy,
    Smart,
}

impl Default for ProxyMode {
    fn default() -> Self {
        default_proxy_mode()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    #[default]
    Proxy,
    Direct,
    Reject,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum RulePattern {
    DomainFull { value: String },
    DomainSuffix { value: String },
    DomainKeyword { value: String },
    DomainRegex { value: String },
    IpCidr { value: String },
    #[cfg(feature = "geoip")]
    GeoIp { value: String },
    Port { value: u16 },
    Final,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientRule {
    pub pattern: RulePattern,
    pub action: RuleAction,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RulesConfig {
    #[serde(default)]
    pub rules: Vec<ClientRule>,
    #[serde(default = "default_rules_final_action")]
    pub final_action: RuleAction,
}

fn default_rules_final_action() -> RuleAction {
    RuleAction::Proxy
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransportProtocol {
    #[default]
    Tcp,
    Quic,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ServerEntry {
    pub name: String,
    pub address: String,
    pub public_key: String,
    #[serde(default)]
    pub cipher: CipherPreference,
    #[serde(default)]
    pub protocol: TransportProtocol,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FailoverConfig {
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval: u64,
    #[serde(default = "default_health_check_timeout")]
    pub health_check_timeout: u64,
    #[serde(default = "default_failover_threshold")]
    pub failover_threshold: u32,
    #[serde(default = "default_graceful_migration")]
    pub graceful_migration: bool,
}

fn default_health_check_interval() -> u64 {
    30
}

fn default_health_check_timeout() -> u64 {
    5
}

fn default_failover_threshold() -> u32 {
    3
}

fn default_graceful_migration() -> bool {
    true
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            health_check_interval: default_health_check_interval(),
            health_check_timeout: default_health_check_timeout(),
            failover_threshold: default_failover_threshold(),
            graceful_migration: default_graceful_migration(),
        }
    }
}

impl ClientConfig {
    pub fn load(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| PhantomError::Config(format!("Failed to read {}: {}", path, e)))?;
        toml::from_str(&content)
            .map_err(|e| PhantomError::Config(format!("Failed to parse config: {}", e)))
    }
}

// === Server Config ===

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    pub private_key: String,
    pub clients: String,
    #[serde(default)]
    pub cipher: CipherPreference,
    #[serde(default)]
    pub quic: QuicConfig,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
}

fn default_bind() -> String {
    "0.0.0.0:443".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuicConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default = "default_max_streams")]
    pub max_streams: u32,
    #[serde(default = "default_keep_alive")]
    pub keep_alive_interval: u64,
    #[serde(default)]
    pub congestion: CongestionAlgorithm,
}

fn default_max_streams() -> u32 {
    100
}

fn default_keep_alive() -> u64 {
    45
}

impl Default for QuicConfig {
    fn default() -> Self {
        Self {
            enable: false,
            max_streams: default_max_streams(),
            keep_alive_interval: default_keep_alive(),
            congestion: CongestionAlgorithm::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub cert: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub disguise: bool,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            cert: None,
            key: None,
            disguise: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PerformanceConfig {
    #[serde(default)]
    pub io_uring: bool,
    #[serde(default)]
    pub zero_copy: bool,
    #[serde(default = "default_workers")]
    pub workers: u32,
}

fn default_workers() -> u32 {
    0
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            io_uring: false,
            zero_copy: false,
            workers: default_workers(),
        }
    }
}

impl ServerConfig {
    pub fn load(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| PhantomError::Config(format!("Failed to read {}: {}", path, e)))?;
        toml::from_str(&content)
            .map_err(|e| PhantomError::Config(format!("Failed to parse config: {}", e)))
    }

    /// Load the server key pair (public + secret) from the key file
    pub fn load_key_pair(&self) -> Result<([u8; 32], [u8; 32])> {
        let mut file = fs::File::open(&self.private_key)
            .map_err(|e| PhantomError::Config(format!("Failed to open key file: {}", e)))?;
        file.lock_shared()
            .map_err(|e| PhantomError::Config(format!("Failed to lock key file: {}", e)))?;
        let mut content = String::new();
        std::io::Read::read_to_string(&mut file, &mut content)
            .map_err(|e| PhantomError::Config(format!("Failed to read key file: {}", e)))?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() < 2 {
            return Err(PhantomError::Config(
                "Key file must have public key on line 1 and secret key on line 2".to_string(),
            ));
        }
        let public = decode_key(lines[0])?;
        let secret = decode_key(lines[1])?;
        Ok((public, secret))
    }

    /// Load the client public key whitelist
    pub fn load_allowed_clients(&self) -> Result<Vec<[u8; 32]>> {
        let content = fs::read_to_string(&self.clients)
            .map_err(|e| PhantomError::Config(format!("Failed to read clients file: {}", e)))?;
        let mut keys = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            keys.push(decode_key(trimmed)?);
        }
        Ok(keys)
    }
}

fn decode_key(s: &str) -> Result<[u8; 32]> {
    let decoded = STANDARD
        .decode(s.trim())
        .map_err(|e| PhantomError::Config(format!("Base64 decode failed: {}", e)))?;
    if decoded.len() != 32 {
        return Err(PhantomError::Config(format!(
            "Key must be 32 bytes, got {}",
            decoded.len()
        )));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&decoded);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_client_config_minimal() {
        let toml = r#"
[[servers]]
name = "primary"
address = "example.com:443"
public_key = "dGVzdA=="
"#;
        let config: ClientConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.client.listen, "127.0.0.1:1080");
        assert_eq!(config.client.dns, "tls://8.8.8.8:853");
    }

    #[test]
    fn parse_client_config_full() {
        let toml = r#"
[[servers]]
name = "primary"
address = "example.com:443"
public_key = "dGVzdA=="

[client]
listen = "127.0.0.1:1080"
dns = "tls://1.1.1.1:853"
mode = "smart"

[failover]
health_check_interval = 30
health_check_timeout = 5
failover_threshold = 3
graceful_migration = true
"#;
        let config: ClientConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.client.mode, ProxyMode::Smart);
        assert!(config.failover.graceful_migration);
    }

    #[test]
    fn parse_server_config() {
        let toml = r#"
bind = "0.0.0.0:443"
private_key = "/etc/phantom/keys/server_private"
clients = "/etc/phantom/keys/clients_allowed"

[quic]
max_streams = 100
keep_alive_interval = 45

[performance]
io_uring = true
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.bind, "0.0.0.0:443");
        assert!(!config.quic.enable);
        assert!(config.performance.io_uring);
        assert!(!config.tls.disguise);
    }

    #[test]
    fn default_failover_config() {
        let config = FailoverConfig::default();
        assert_eq!(config.health_check_interval, 30);
        assert_eq!(config.health_check_timeout, 5);
        assert_eq!(config.failover_threshold, 3);
        assert!(config.graceful_migration);
    }
}
