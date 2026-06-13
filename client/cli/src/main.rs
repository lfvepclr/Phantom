use anyhow::Result;
use clap::{Parser, Subcommand};
use phantom_core::{parse_phantom_uri, ClientConfig, CipherPreference, TransportProtocol};
use phantom_server::bootstrap::{run_auto, run_interactive, AutoOptions};

#[derive(Parser)]
#[command(name = "phantom", version, about = "Phantom proxy tool (幽灵)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as client (SOCKS5 proxy). Use `--server <URI>` for the quick link
    /// flow (no TOML required); use `-c <file>` to load full client.toml.
    Client {
        /// Optional client.toml path. If omitted, the URI from `--server` is
        /// used standalone (default settings).
        #[arg(short, long)]
        config: Option<String>,
        /// Server URI (phantom://<base64_key>@host:port?cipher=auto&proto=quic#name)
        #[arg(short, long)]
        server: Option<String>,
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
        Commands::Client { config, server } => {
            let mut config = match config.as_deref() {
                Some(path) => ClientConfig::load(path)?,
                None => ClientConfig::default(),
            };
            if let Some(uri) = server {
                let entry = parse_phantom_uri(&uri)
                    .map_err(|e| anyhow::anyhow!("Failed to parse server URI: {}", e))?;
                config.servers = vec![entry];
            }
            if config.servers.is_empty() {
                return Err(anyhow::anyhow!("No servers configured. Use --server or provide a client.toml with [[servers]]"));
            }
            init_tracing("info");
            let client = phantom_client::PhantomClient::new(config)?;
            client.run().await?;
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
    let _ = tracing_subscriber::fmt()
        .with_env_filter(level)
        .try_init();
}
