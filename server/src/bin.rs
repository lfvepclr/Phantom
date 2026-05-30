use anyhow::Result;
use phantom_core::ServerConfig;

#[cfg(all(feature = "io-uring", target_os = "linux"))]
fn main() -> Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "server.toml".to_string());

    let config = ServerConfig::load(&config_path)?;
    if config.performance.workers != 0 {
        tracing::warn!(
            "performance.workers = {} is ignored in io-uring mode",
            config.performance.workers
        );
    }

    tokio_uring::start(async {
        phantom_server::run(&config_path).await
    })
}

#[cfg(not(all(feature = "io-uring", target_os = "linux")))]
fn main() -> Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "server.toml".to_string());

    let config = ServerConfig::load(&config_path)?;
    let workers = if config.performance.workers == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        config.performance.workers as usize
    };

    tracing::info!("Starting server with {} worker threads", workers);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;

    rt.block_on(phantom_server::run(&config_path))
}
