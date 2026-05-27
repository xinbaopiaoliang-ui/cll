use crate::{
    config::{
        persist_remote_network_config, BandwidthQuality, ControlPlaneConfig, NetworkConfig,
        OperatorIps,
    },
    state::RuntimeState,
};
use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    task::JoinHandle,
    time::{interval, MissedTickBehavior},
};
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

const REPORT_PATH: &str = "/api/node/v1/report";
const HANDSHAKE_PATH: &str = "/api/node/v1/handshake";
const CONFIG_PATH: &str = "/api/node/v1/config";
static NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn spawn_control_plane(state: RuntimeState, config_path: PathBuf) -> Vec<JoinHandle<()>> {
    let Some(control) = state.config().control.clone() else {
        info!("control plane disabled: missing [control] config");
        return Vec::new();
    };

    if !control.enabled {
        info!("control plane disabled by config");
        return Vec::new();
    }

    let Some((node_id, panel_url, node_secret)) = state.identity().control_plane_credentials()
    else {
        warn!("control plane disabled: node_id, panel_url, or node_secret is missing");
        return Vec::new();
    };

    if is_placeholder_panel_url(panel_url) {
        warn!(%panel_url, "control plane disabled: panel_url is still the example placeholder");
        return Vec::new();
    }

    let report_interval_sec = state
        .config()
        .report
        .as_ref()
        .map(|report| report.interval_sec)
        .unwrap_or(30)
        .max(5);
    let client = Client::builder()
        .timeout(Duration::from_secs(control.request_timeout_sec))
        .user_agent(format!("xaccel-node/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client builds");

    let task_state = state.clone();
    let config_state = state.clone();
    let panel_url = panel_url.trim_end_matches('/').to_string();
    let config_panel_url = panel_url.clone();
    let node_secret = node_secret.to_string();
    let config_node_secret = node_secret.clone();
    let config_poll_interval_sec = control.config_poll_interval_sec.max(5);
    let report_client = client.clone();
    let config_client = client;

    let task = tokio::spawn(async move {
        if let Err(error) = send_handshake(
            &report_client,
            &task_state,
            node_id,
            &panel_url,
            &node_secret,
        )
        .await
        {
            warn!(?error, "control plane handshake failed");
        }
        run_report_loop(
            report_client,
            task_state,
            control,
            node_id,
            panel_url,
            node_secret,
            report_interval_sec,
        )
        .await;
    });

    let config_task = tokio::spawn(async move {
        run_config_loop(
            config_client,
            config_state,
            node_id,
            config_panel_url,
            config_node_secret,
            config_poll_interval_sec,
            config_path,
        )
        .await;
    });

    vec![task, config_task]
}

#[derive(Serialize)]
struct NodeHandshake {
    node_id: u64,
    node_version: &'static str,
    os: &'static str,
    arch: &'static str,
    boot_id: String,
    timestamp: u64,
    nonce: String,
    config_revision: u64,
    listen_addr: Option<String>,
}

#[derive(Deserialize)]
struct NodeHandshakeResponse {
    status: String,
    node_id: u64,
    server_time: u64,
    config_revision: u64,
    min_node_version: String,
}

async fn send_handshake(
    client: &Client,
    state: &RuntimeState,
    node_id: u64,
    panel_url: &str,
    node_secret: &str,
) -> anyhow::Result<()> {
    let timestamp = now_unix();
    let nonce = next_nonce(timestamp);
    let handshake = NodeHandshake {
        node_id,
        node_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        boot_id: read_boot_id(),
        timestamp,
        nonce: nonce.clone(),
        config_revision: state.config_revision(),
        listen_addr: state
            .effective_network()
            .map(|network| network.listen_endpoint()),
    };
    let body = serde_json::to_vec(&handshake).context("failed to encode node handshake")?;
    let signed = sign_request(
        "POST",
        HANDSHAKE_PATH,
        timestamp,
        &nonce,
        &body,
        node_secret,
    )?;
    let url = format!("{panel_url}{HANDSHAKE_PATH}");

    let response = match client
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-Node-Id", node_id.to_string())
        .header("X-Node-Timestamp", timestamp.to_string())
        .header("X-Node-Nonce", &nonce)
        .header("X-Node-Body-Sha256", signed.body_sha256)
        .header("X-Node-Signature", signed.signature)
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            state
                .stats()
                .record_handshake_failure(None, error.to_string());
            return Err(error.into());
        }
    };

    let status = response.status();
    if !status.is_success() {
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "failed to read response body".to_string());
        let message = trim_for_log(&message, 200);
        state
            .stats()
            .record_handshake_failure(Some(status.as_u16()), message.clone());
        bail!("handshake rejected: http {} {}", status.as_u16(), message);
    }

    let payload = response
        .json::<NodeHandshakeResponse>()
        .await
        .context("failed to decode handshake response")?;
    state.stats().record_handshake_success(status.as_u16());
    info!(
        status = %payload.status,
        node_id = payload.node_id,
        server_time = payload.server_time,
        remote_config_revision = payload.config_revision,
        local_config_revision = state.config_revision(),
        min_node_version = %payload.min_node_version,
        "control plane handshake accepted"
    );
    Ok(())
}

async fn run_report_loop(
    client: Client,
    state: RuntimeState,
    control: ControlPlaneConfig,
    node_id: u64,
    panel_url: String,
    node_secret: String,
    report_interval_sec: u64,
) {
    info!(
        %panel_url,
        report_interval_sec,
        config_revision = control.config_revision,
        "control plane report loop started"
    );

    let mut ticker = interval(Duration::from_secs(report_interval_sec));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        if let Err(error) = send_report(&client, &state, node_id, &panel_url, &node_secret).await {
            warn!(?error, "control plane report failed");
        }
    }
}

async fn run_config_loop(
    client: Client,
    state: RuntimeState,
    node_id: u64,
    panel_url: String,
    node_secret: String,
    config_poll_interval_sec: u64,
    config_path: PathBuf,
) {
    info!(
        %panel_url,
        config_poll_interval_sec,
        "control plane config sync loop started"
    );

    let mut ticker = interval(Duration::from_secs(config_poll_interval_sec));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        if let Err(error) = fetch_and_apply_config(
            &client,
            &state,
            node_id,
            &panel_url,
            &node_secret,
            &config_path,
        )
        .await
        {
            state.stats().record_config_failure(None, error.to_string());
            warn!(?error, "control plane config sync failed");
        }
    }
}

#[derive(Serialize)]
struct NodeReport {
    node_id: u64,
    config_revision: u64,
    node_version: &'static str,
    status: &'static str,
    timestamp: u64,
    health: crate::state::HealthSnapshot,
}

async fn send_report(
    client: &Client,
    state: &RuntimeState,
    node_id: u64,
    panel_url: &str,
    node_secret: &str,
) -> anyhow::Result<()> {
    let report = NodeReport {
        node_id,
        config_revision: state.config_revision(),
        node_version: env!("CARGO_PKG_VERSION"),
        status: state.status(),
        timestamp: now_unix(),
        health: state.health_snapshot(),
    };
    let body = serde_json::to_vec(&report).context("failed to encode node report")?;
    let timestamp = now_unix();
    let nonce = next_nonce(timestamp);
    let signed = sign_request("POST", REPORT_PATH, timestamp, &nonce, &body, node_secret)?;
    let url = format!("{panel_url}{REPORT_PATH}");

    let response = match client
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-Node-Id", node_id.to_string())
        .header("X-Node-Timestamp", timestamp.to_string())
        .header("X-Node-Nonce", &nonce)
        .header("X-Node-Body-Sha256", signed.body_sha256)
        .header("X-Node-Signature", signed.signature)
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            state
                .stats()
                .record_control_failure(None, error.to_string());
            return Err(error.into());
        }
    };

    let status = response.status();
    if status.is_success() {
        state.stats().record_control_success(status.as_u16());
        return Ok(());
    }

    let message = response
        .text()
        .await
        .unwrap_or_else(|_| "failed to read response body".to_string());
    let message = trim_for_log(&message, 200);
    state
        .stats()
        .record_control_failure(Some(status.as_u16()), message.clone());
    bail!("report rejected: http {} {}", status.as_u16(), message)
}

#[derive(Deserialize)]
struct NodeConfigResponse {
    config_revision: u64,
    network: RemoteNetworkConfig,
}

#[derive(Deserialize)]
struct RemoteNetworkConfig {
    server_ip: String,
    listen_ip: Option<String>,
    server_port: u16,
    relay_server_ip: Option<String>,
    relay_server_port: Option<u16>,
    is_support_ipv6: bool,
    disable_quic: bool,
    area: String,
    bandwidth_quality: BandwidthQuality,
    tag: Option<String>,
    operator_ips: Option<OperatorIps>,
}

async fn fetch_and_apply_config(
    client: &Client,
    state: &RuntimeState,
    node_id: u64,
    panel_url: &str,
    node_secret: &str,
    config_path: &Path,
) -> anyhow::Result<()> {
    let body: &[u8] = b"";
    let timestamp = now_unix();
    let nonce = next_nonce(timestamp);
    let signed = sign_request("GET", CONFIG_PATH, timestamp, &nonce, body, node_secret)?;
    let url = format!("{panel_url}{CONFIG_PATH}");

    let response = client
        .get(url)
        .header("X-Node-Id", node_id.to_string())
        .header("X-Node-Timestamp", timestamp.to_string())
        .header("X-Node-Nonce", &nonce)
        .header("X-Node-Body-Sha256", signed.body_sha256)
        .header("X-Node-Signature", signed.signature)
        .send()
        .await
        .context("config request failed")?;

    let status = response.status();
    if !status.is_success() {
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "failed to read response body".to_string());
        bail!(
            "config sync rejected: http {} {}",
            status.as_u16(),
            trim_for_log(&message, 200)
        );
    }

    let remote = response
        .json::<NodeConfigResponse>()
        .await
        .context("failed to decode config response")?;
    let network = NetworkConfig {
        server_ip: remote.network.server_ip,
        listen_ip: remote.network.listen_ip,
        server_port: remote.network.server_port,
        relay_server_ip: remote.network.relay_server_ip,
        relay_server_port: remote.network.relay_server_port,
        is_support_ipv6: remote.network.is_support_ipv6,
        disable_quic: remote.network.disable_quic,
        area: remote.network.area,
        bandwidth_quality: remote.network.bandwidth_quality,
        tag: remote.network.tag,
        operator_ips: remote.network.operator_ips,
    };

    let should_apply = remote.config_revision > state.config_revision();
    if should_apply {
        if let Err(error) =
            persist_remote_network_config(config_path, remote.config_revision, &network)
        {
            state.stats().record_config_failure(
                Some(status.as_u16()),
                format!("failed to persist remote node config: {error}"),
            );
            warn!(
                ?error,
                path = %config_path.display(),
                "failed to persist remote node config for next restart"
            );
            bail!("failed to persist remote node config: {error}");
        }
    }

    let result = state.apply_remote_network_config(remote.config_revision, network);
    state.stats().record_config_success(status.as_u16());
    if result.applied {
        info!(
            previous_revision = result.previous_revision,
            current_revision = result.current_revision,
            restart_required = result.restart_required,
            "node config updated"
        );
    }
    Ok(())
}

struct SignedRequest {
    body_sha256: String,
    signature: String,
}

fn sign_request(
    method: &str,
    path: &str,
    timestamp: u64,
    nonce: &str,
    body: &[u8],
    secret: &str,
) -> anyhow::Result<SignedRequest> {
    let body_sha256 = BASE64.encode(Sha256::digest(body));
    let canonical = format!("{method}\n{path}\n{timestamp}\n{nonce}\n{body_sha256}");
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
        .context("failed to initialize HMAC signer")?;
    mac.update(canonical.as_bytes());

    Ok(SignedRequest {
        body_sha256,
        signature: BASE64.encode(mac.finalize().into_bytes()),
    })
}

fn next_nonce(timestamp: u64) -> String {
    let counter = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}-{counter}", std::process::id(), timestamp)
}

fn is_placeholder_panel_url(panel_url: &str) -> bool {
    panel_url
        .trim()
        .to_ascii_lowercase()
        .contains("api.example.com")
}

fn read_boot_id() -> String {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|boot_id| boot_id.trim().to_string())
        .ok()
        .filter(|boot_id| !boot_id.is_empty())
        .unwrap_or_else(|| format!("pid-{}-{}", std::process::id(), now_unix()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn trim_for_log(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_request_deterministically() {
        let signed =
            sign_request("POST", "/path", 123, "nonce", b"{}", "secret").expect("request signs");

        assert_eq!(
            signed.body_sha256,
            "RBNvo1WzZ4oRRq0W9+hknpT7T8If536DEMBg9hyq/4o="
        );
        assert_eq!(
            signed.signature,
            "97OqjLUUsFtykHLO0NQtnUCEtNfi7khtt1wEgmzmFd0="
        );
    }

    #[test]
    fn detects_placeholder_panel_url() {
        assert!(is_placeholder_panel_url("https://api.example.com"));
        assert!(!is_placeholder_panel_url("https://panel.example.net"));
    }
}
