use std::path::PathBuf;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;
use truongphoto_core::{PowerState, ServerConfig, StoragePaths};
use truongphoto_db::DbHandle;
use truongphoto_server::start_server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("truongphoto_server=info,tower_http=info")),
        )
        .init();

    let data_dir = std::env::var_os("TP_DATA_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(StoragePaths::default_data_dir);

    let mut config = ServerConfig::from_data_dir(data_dir);
    if let Ok(host) = std::env::var("TP_BIND_HOST") {
        let trimmed = host.trim();
        if !trimmed.is_empty() {
            config.host = trimmed.to_string();
        }
    }
    if let Ok(port_str) = std::env::var("TP_PORT") {
        if let Ok(port) = port_str.trim().parse::<u16>() {
            config.port = port;
        }
    }

    let db = DbHandle::open(&config.db_path)?;
    let (_power_tx, power_rx) = watch::channel(PowerState::default());

    let server = start_server(config, db.clone(), power_rx).await?;
    tracing::info!(port = server.port(), "headless truongphoto-server ready");

    wait_for_shutdown_signal().await;
    tracing::info!("shutdown signal received, draining server...");

    server.shutdown().await;
    db.shutdown().await?;
    tracing::info!("truongphoto-server stopped cleanly");

    Ok(())
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
