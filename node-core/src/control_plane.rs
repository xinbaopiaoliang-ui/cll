use crate::{config::ControlPlaneConfig, state::RuntimeState};
use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
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
static NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn spawn_control_plane(state: RuntimeState) -> Vec<JoinHandle<()>> {
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
    let panel_url = panel_url.trim_end_matches('/').to_string();
    let node_secret = node_secret.to_string();

    let task = tokio::spawn(async move {
        run_report_loop(
            client,
            task_state,
            control,
            node_id,
            panel_url,
            node_secret,
            report_interval_sec,
        )
        .await;
    });

    vec![task]
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
        if let Err(error) = send_report(
            &client,
            &state,
            node_id,
            &panel_url,
            &node_secret,
            control.config_revision,
        )
        .await
        {
            warn!(?error, "control plane report failed");
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
    config_revision: u64,
) -> anyhow::Result<()> {
    let report = NodeReport {
        node_id,
        config_revision,
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
