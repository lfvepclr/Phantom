use anyhow::Result;
use clap::{Parser, Subcommand};
use phantom_core::{ClientConfig, parse_phantom_uri};
use phantom_crypto::KeyPair;

#[derive(Parser)]
#[command(name = "phantom", version, about = "Phantom proxy tool (幽灵)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as client (SOCKS5 proxy)
    Client {
        #[arg(short, long, default_value = "client.toml")]
        config: String,
        /// Server URI (phantom://<base64_key>@host:port?cipher=auto&proto=quic#name)
        #[arg(short, long)]
        server: Option<String>,
    },
    /// Run as server
    Server {
        #[arg(short, long, default_value = "server.toml")]
        config: String,
    },
    /// Generate X25519 key pair
    Keygen {
        /// Output directory for key files
        #[arg(short, long, default_value = ".")]
        output: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Client { config, server } => {
            let mut config = ClientConfig::load(&config)?;
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
        Commands::Server { config } => {
            phantom_server::run(&config).await?;
        }
        Commands::Keygen { output } => {
            let kp = KeyPair::generate()?;
            let pub_path = format!("{}/phantom.pub", output);
            let sec_path = format!("{}/server_private", output);
            kp.save_secret_to_file(&sec_path)?;
            kp.save_public_to_file(&pub_path)?;

            println!("Generated X25519 key pair:");
            println!("  Public key (add to client.toml [[servers]].public_key):");
            println!("  {}", kp.public_key_base64());
            println!("  Public key file: {}", pub_path);
            println!("  Secret key file: {} (chmod 600)", sec_path);
            println!();
            println!("To allow this client, append the public key to the server's clients file.");
        }
    }

    Ok(())
}

fn init_tracing(level: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(level)
        .init();
}
