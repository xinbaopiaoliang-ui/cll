mod cli;
mod config;
mod health;
mod identity;
mod state;

use anyhow::Context;
use clap::Parser;
use cli::Cli;
use config::NodeConfig;
use health::run_health_server;
use identity::IdentityState;
use state::RuntimeState;
use std::path::PathBuf;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();

    if cli.version {
        println!("xaccel-node {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from("/etc/xaccel-node/config.toml"));

    let config = NodeConfig::from_file(&config_path)
        .with_context(|| format!("failed to load config: {}", config_path.display()))?;

    if let Some(check_path) = cli.check_config {
        NodeConfig::from_file(&check_path)
            .with_context(|| format!("failed to check config: {}", check_path.display()))?;
        println!("config ok: {}", check_path.display());
        return Ok(());
    }

    let identity = IdentityState::from_config(&config)?;
    let state = RuntimeState::new(config, identity);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        status = %state.status(),
        "xaccel-node starting"
    );

    if state.identity().is_bootstrap_placeholder() {
        warn!("node identity is still bootstrap placeholder; control-plane exchange is pending");
    }

    let health_state = state.clone();
    let health_addr = state.config().runtime.health_addr;
    let health_task = tokio::spawn(async move { run_health_server(health_addr, health_state).await });

    wait_for_shutdown().await;
    info!("shutdown requested");

    health_task.abort();
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

