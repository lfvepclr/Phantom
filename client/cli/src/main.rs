use anyhow::Result;
use clap::{Parser, Subcommand};
use phantom_client::TunRuntimeOptions;
use phantom_core::{CipherPreference, ClientConfig, TransportProtocol, parse_phantom_uri};
use phantom_server::bootstrap::{AutoOptions, run_auto, run_interactive};
use std::net::Ipv4Addr;

#[derive(Parser)]
#[command(name = "phantom", version, about = "Phantom proxy tool (幽灵)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as client. Default mode is a local SOCKS5 proxy; `--tun` switches to
    /// transparent proxying, and on Linux `--gateway` additionally routes LAN
    /// traffic through it (router deployments).
    ///
    /// Use `--server <URI>` for the quick link flow (no TOML required); use
    /// `-c <file>` to load a full client.toml.
    Client {
        /// Optional client.toml path. If omitted, the URI from `--server` is
        /// used standalone (default settings).
        #[arg(short, long)]
        config: Option<String>,
        /// Server URI (phantom://<base64_key>@host:port?cipher=auto&proto=quic#name)
        #[arg(short, long)]
        server: Option<String>,
        /// Run a TUN transparent proxy in addition to the SOCKS5 listener.
        /// Requires root.
        #[arg(long)]
        tun: bool,
        /// TUN interface name. Default: utun7 (macOS) / phantom0 (Linux).
        #[arg(long, requires = "tun")]
        tun_name: Option<String>,
        /// TUN local address in CIDR form. Default: 10.7.0.1/24.
        #[arg(long, requires = "tun")]
        tun_addr: Option<String>,
        /// TUN MTU. Default: 1500.
        #[arg(long, requires = "tun")]
        tun_mtu: Option<u16>,
        /// Linux only: install policy routing + firewall rules so forwarded LAN
        /// traffic goes through the tunnel. Reverted on exit.
        #[arg(long, requires = "tun")]
        gateway: bool,
        /// LAN interface whose forwarded traffic is tunnelled. Repeatable.
        /// Default: br0.
        #[arg(long = "lan-interface", requires = "gateway")]
        lan_interfaces: Vec<String>,
        /// Destination CIDR that keeps using the main routing table.
        /// Repeatable; replaces the built-in private-range bypass list.
        #[arg(long = "bypass", requires = "gateway")]
        bypass_cidrs: Vec<String>,
        /// Routing table id for the tunnel default route. Default: 200.
        #[arg(long, requires = "gateway")]
        table: Option<u32>,
        /// Do not redirect LAN port-53 traffic into the tunnel.
        #[arg(long, requires = "gateway")]
        no_lan_dns_hijack: bool,
    },
    /// Run as server (auto / load / interactive)
    Server {
        /// Load configuration from this TOML file (load mode).
        /// Mutually exclusive with `-i` / `--interactive`.
        #[arg(short, long)]
        config: Option<String>,
        /// Run an interactive setup wizard before starting (interactive mode).
        /// Mutually exclusive with `-c`.
        #[arg(short, long)]
        interactive: bool,
        /// Override the public host written into `./server.toml` (auto / interactive).
        #[arg(long)]
        public_host: Option<String>,
        /// Override the starting port (auto / interactive). Default: 443.
        #[arg(long)]
        port: Option<u16>,
        /// Cipher override: auto / aes-256-gcm / aes-128-gcm / ascon-128 / chacha20-poly1305
        #[arg(long)]
        cipher: Option<String>,
        /// Protocol override: tcp / quic
        #[arg(long)]
        proto: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Client {
            config,
            server,
            tun,
            tun_name,
            tun_addr,
            tun_mtu,
            gateway,
            lan_interfaces,
            bypass_cidrs,
            table,
            no_lan_dns_hijack,
        } => {
            let config_path = config.clone();
            let mut config = match config_path.as_deref() {
                Some(path) => ClientConfig::load(path)?,
                None => ClientConfig::default(),
            };
            if let Some(uri) = server {
                let entry = parse_phantom_uri(&uri)
                    .map_err(|e| anyhow::anyhow!("Failed to parse server URI: {}", e))?;
                config.servers = vec![entry];
            }
            if config.servers.is_empty() {
                return Err(anyhow::anyhow!(
                    "No servers configured. Use --server or provide a client.toml with [[servers]]"
                ));
            }
            init_tracing("info");
            let client = phantom_client::PhantomClient::new(config)?;
            if tun {
                let mut opts = TunRuntimeOptions {
                    config_path,
                    ..TunRuntimeOptions::default()
                };
                if let Some(name) = tun_name {
                    opts.tun.name = name;
                }
                if let Some(cidr) = tun_addr {
                    let (address, netmask) = parse_tun_cidr(&cidr)?;
                    opts.tun.address = address;
                    opts.tun.netmask = netmask;
                }
                if let Some(mtu) = tun_mtu {
                    opts.tun.mtu = mtu;
                }
                apply_gateway_options(
                    &mut opts,
                    gateway,
                    lan_interfaces,
                    bypass_cidrs,
                    table,
                    !no_lan_dns_hijack,
                )?;
                client.run_tun(opts).await?;
            } else {
                client.run().await?;
            }
        }
        Commands::Server {
            config,
            interactive,
            public_host,
            port,
            cipher,
            proto,
        } => {
            init_tracing("info");

            // Mutual exclusion: -c and -i cannot both be set.
            if config.is_some() && interactive {
                return Err(anyhow::anyhow!(
                    "`-c <file>` and `-i` / `--interactive` are mutually exclusive"
                ));
            }

            if let Some(path) = config {
                // Load mode: original TOML behavior, unchanged.
                phantom_server::run(&path).await?;
            } else {
                // Auto or interactive mode. Build AutoOptions.
                let opts = AutoOptions {
                    public_host,
                    start_port: port,
                    cipher: match cipher.as_deref() {
                        None | Some("") => None,
                        Some("auto") => Some(CipherPreference::Auto),
                        Some("aes-256-gcm") => Some(CipherPreference::Aes256Gcm),
                        Some("aes-128-gcm") => Some(CipherPreference::Aes128Gcm),
                        Some("ascon-128") => Some(CipherPreference::Ascon128),
                        Some("chacha20-poly1305") => Some(CipherPreference::ChaCha20Poly1305),
                        Some(other) => {
                            return Err(anyhow::anyhow!(
                                "Unknown cipher: {other} (valid: auto, aes-256-gcm, aes-128-gcm, ascon-128, chacha20-poly1305)"
                            ));
                        }
                    },
                    protocol: match proto.as_deref() {
                        None | Some("") => None,
                        Some("tcp") => Some(TransportProtocol::Tcp),
                        Some("quic") => Some(TransportProtocol::Quic),
                        Some(other) => {
                            return Err(anyhow::anyhow!(
                                "Unknown protocol: {other} (valid: tcp, quic)"
                            ));
                        }
                    },
                    max_port_tries: None,
                    // CLI keeps the historical CWD behaviour; the HarmonyOS
                    // embedded server passes its sandbox directory instead.
                    work_dir: None,
                };
                if interactive {
                    run_interactive(opts).await?;
                } else {
                    run_auto(opts).await?;
                }
            }
        }
    }

    Ok(())
}

fn init_tracing(level: &str) {
    let _ = tracing_subscriber::fmt().with_env_filter(level).try_init();
}

/// Parse `--tun-addr` (`10.7.0.1/24`) into an address plus netmask.
///
/// A bare address without a prefix is accepted and defaults to /24.
fn parse_tun_cidr(cidr: &str) -> Result<(Ipv4Addr, Ipv4Addr)> {
    let (addr_part, prefix_part) = match cidr.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (cidr, None),
    };
    let address: Ipv4Addr = addr_part
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid --tun-addr '{}': {}", cidr, e))?;
    let prefix: u32 = match prefix_part {
        Some(p) => p
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid --tun-addr prefix in '{}': {}", cidr, e))?,
        None => 24,
    };
    if prefix > 32 {
        return Err(anyhow::anyhow!(
            "Invalid --tun-addr prefix /{}: must be 0-32",
            prefix
        ));
    }
    let mask = if prefix == 0 {
        0u32
    } else {
        u32::MAX << (32 - prefix)
    };
    Ok((address, Ipv4Addr::from(mask)))
}

#[cfg(target_os = "linux")]
fn apply_gateway_options(
    opts: &mut TunRuntimeOptions,
    gateway: bool,
    lan_interfaces: Vec<String>,
    bypass_cidrs: Vec<String>,
    table: Option<u32>,
    lan_dns_hijack: bool,
) -> Result<()> {
    if !gateway {
        return Ok(());
    }
    let mut config = phantom_client::gateway::GatewayConfig {
        tun_name: opts.tun.name.clone(),
        tun_addr: opts.tun.address,
        lan_dns_hijack,
        ..Default::default()
    };
    if !lan_interfaces.is_empty() {
        config.lan_interfaces = lan_interfaces;
    }
    if !bypass_cidrs.is_empty() {
        config.bypass_cidrs = bypass_cidrs;
    }
    if let Some(table) = table {
        config.table_id = table;
    }
    opts.gateway = Some(config);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn apply_gateway_options(
    _opts: &mut TunRuntimeOptions,
    gateway: bool,
    _lan_interfaces: Vec<String>,
    _bypass_cidrs: Vec<String>,
    _table: Option<u32>,
    _lan_dns_hijack: bool,
) -> Result<()> {
    if gateway {
        return Err(anyhow::anyhow!(
            "--gateway is only supported on Linux (policy routing via iproute2/iptables)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tun_cidr_parses_prefix_into_netmask() {
        assert_eq!(
            parse_tun_cidr("10.7.0.1/24").unwrap(),
            (
                Ipv4Addr::new(10, 7, 0, 1),
                Ipv4Addr::new(255, 255, 255, 0)
            )
        );
        assert_eq!(
            parse_tun_cidr("172.19.0.1/16").unwrap(),
            (
                Ipv4Addr::new(172, 19, 0, 1),
                Ipv4Addr::new(255, 255, 0, 0)
            )
        );
        assert_eq!(
            parse_tun_cidr("10.0.0.1/32").unwrap().1,
            Ipv4Addr::new(255, 255, 255, 255)
        );
    }

    #[test]
    fn tun_cidr_without_prefix_defaults_to_24() {
        assert_eq!(
            parse_tun_cidr("10.7.0.1").unwrap().1,
            Ipv4Addr::new(255, 255, 255, 0)
        );
    }

    #[test]
    fn tun_cidr_rejects_bad_input() {
        assert!(parse_tun_cidr("not-an-ip/24").is_err());
        assert!(parse_tun_cidr("10.7.0.1/33").is_err());
        assert!(parse_tun_cidr("10.7.0.1/abc").is_err());
    }

    #[test]
    fn cli_rejects_gateway_flags_without_tun() {
        use clap::Parser;
        // `--gateway` depends on `--tun`, and the LAN flags depend on
        // `--gateway`: clap must reject the orphaned combinations.
        assert!(Cli::try_parse_from(["phantom", "client", "--gateway"]).is_err());
        assert!(
            Cli::try_parse_from(["phantom", "client", "--tun", "--lan-interface", "br0"]).is_err()
        );
        assert!(Cli::try_parse_from(["phantom", "client", "--tun-name", "phantom0"]).is_err());
    }

    #[test]
    fn cli_accepts_full_gateway_invocation() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "phantom",
            "client",
            "--server",
            "phantom://dGVzdA==@example.com:443",
            "--tun",
            "--tun-name",
            "phantom0",
            "--tun-addr",
            "10.7.0.1/24",
            "--gateway",
            "--lan-interface",
            "br0",
            "--lan-interface",
            "br1",
            "--table",
            "201",
        ])
        .expect("valid gateway invocation");
        match cli.command {
            Commands::Client {
                tun,
                gateway,
                lan_interfaces,
                table,
                ..
            } => {
                assert!(tun && gateway);
                assert_eq!(lan_interfaces, vec!["br0", "br1"]);
                assert_eq!(table, Some(201));
            }
            _ => panic!("expected the client subcommand"),
        }
    }

    #[test]
    fn client_defaults_to_socks5_only() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["phantom", "client"]).unwrap();
        match cli.command {
            Commands::Client { tun, gateway, .. } => assert!(!tun && !gateway),
            _ => panic!("expected the client subcommand"),
        }
    }
}
