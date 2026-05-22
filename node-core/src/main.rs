mod auth;
mod cli;
mod config;
mod control_plane;
mod health;
mod identity;
mod listener;
mod session;
mod state;

use anyhow::Context;
use auth::{sign_client_token, ClientTokenClaims};
use clap::Parser;
use cli::Cli;
use config::NodeConfig;
use control_plane::spawn_control_plane;
use health::run_health_server;
use identity::IdentityState;
use listener::spawn_network_listeners;
use state::RuntimeState;
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
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

    if cli.make_client_token {
        let token = make_client_token(&cli, &identity)?;
        println!("{token}");
        return Ok(());
    }

    let state = RuntimeState::new(config, identity);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        status = %state.status(),
        "xaccel-node starting"
    );

    if state.identity().is_bootstrap_placeholder() {
        warn!("node identity is still bootstrap placeholder; control-plane exchange is pending");
    }

    let listener_tasks = spawn_network_listeners(state.clone()).await?;
    let control_plane_tasks = spawn_control_plane(state.clone());

    let health_state = state.clone();
    let health_addr = state.config().runtime.health_addr;
    let health_task =
        tokio::spawn(async move { run_health_server(health_addr, health_state).await });

    wait_for_shutdown().await;
    info!("shutdown requested");

    health_task.abort();
    for task in listener_tasks {
        task.abort();
    }
    for task in control_plane_tasks {
        task.abort();
    }
    Ok(())
}

fn make_client_token(cli: &Cli, identity: &IdentityState) -> anyhow::Result<String> {
    let node_id = identity
        .node_id
        .context("node_id is required to create a client token")?;
    let secret = identity
        .node_secret()
        .context("node_secret is required to create a client token")?;
    let user_id = cli
        .token_user_id
        .context("--token-user-id is required with --make-client-token")?;
    let device_id = cli
        .token_device_id
        .clone()
        .context("--token-device-id is required with --make-client-token")?;
    let game_id = cli
        .token_game_id
        .context("--token-game-id is required with --make-client-token")?;
    let issued_at = now_unix();

    let claims = ClientTokenClaims {
        node_id,
        user_id,
        device_id,
        game_id,
        expires_at: issued_at + cli.token_ttl_sec.max(1),
        issued_at: Some(issued_at),
        nonce: cli.token_nonce.clone(),
    };

    sign_client_token(&claims, secret)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
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
