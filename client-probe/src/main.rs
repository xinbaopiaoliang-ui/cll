use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    net::SocketAddr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{net::UdpSocket, time::timeout};

const PROTOCOL_VERSION: &str = "xaccel/1";
const UDP_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Parser)]
#[command(name = "xaccel-client-probe")]
#[command(about = "Run an XAccel connect-intent, UDP probe, and session.data check")]
struct Cli {
    #[arg(
        long,
        env = "XACCEL_CONTROL_URL",
        default_value = "http://127.0.0.1:18080"
    )]
    control_url: String,

    #[arg(long, default_value_t = 1001)]
    user_id: u64,

    #[arg(long, default_value = "pc-001")]
    device_id: String,

    #[arg(long, default_value_t = 8888)]
    game_id: u64,

    #[arg(long)]
    region_id: Option<u64>,

    #[arg(long, default_value = "pc")]
    platform: String,

    #[arg(long)]
    client_isp: Option<String>,

    #[arg(long)]
    client_ip: Option<String>,

    #[arg(long, default_value = "normal")]
    bandwidth_quality: String,

    #[arg(long, default_value_t = 0)]
    candidate_index: usize,

    #[arg(long, default_value = "hello")]
    payload: String,

    #[arg(long, default_value_t = 500)]
    response_timeout_ms: u64,

    #[arg(long, default_value_t = 3)]
    timeout_sec: u64,

    #[arg(long)]
    skip_session_data: bool,

    #[arg(long)]
    compact: bool,
}

#[derive(Debug, Serialize)]
struct ConnectIntentRequest {
    user_id: u64,
    device_id: String,
    game_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    region_id: Option<u64>,
    platform: String,
    client_isp: Option<String>,
    client_ip: Option<String>,
    bandwidth_quality: String,
}

#[derive(Debug, Deserialize)]
struct ConnectIntentResponse {
    intent_id: String,
    ttl_sec: u64,
    candidates: Vec<NodeCandidate>,
}

#[derive(Debug, Clone, Deserialize)]
struct NodeCandidate {
    node_id: u64,
    area: String,
    tag: String,
    host: String,
    port: u16,
    transports: Vec<String>,
    bandwidth_quality: String,
    route: CandidateRoute,
    credential: CandidateCredential,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CandidateRoute {
    target_addr: String,
    protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    region_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    region_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CandidateCredential {
    token: String,
    expires_at: u64,
    intent_id: String,
}

#[derive(Debug, Serialize)]
struct ProbeRequest {
    #[serde(rename = "type")]
    message_type: &'static str,
    protocol: &'static str,
    client_nonce: String,
    user_id: u64,
    device_id: String,
    game_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    region_id: Option<u64>,
    transport: &'static str,
    token: String,
}

#[derive(Debug, Deserialize)]
struct ProbeResponse {
    #[serde(rename = "type")]
    message_type: String,
    node_id: Option<u64>,
    node_version: String,
    transport: String,
    session: ProbeSession,
}

#[derive(Debug, Deserialize)]
struct ProbeSession {
    session_id: String,
    ttl_sec: u64,
    intent_id: Option<String>,
    route_target_addr: Option<String>,
    credential_valid: bool,
    credential_expires_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SessionDataRequest {
    #[serde(rename = "type")]
    message_type: &'static str,
    protocol: &'static str,
    session_id: String,
    client_nonce: String,
    payload: String,
    response_timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
struct SessionDataResponse {
    #[serde(rename = "type")]
    message_type: String,
    status: String,
    payload: String,
    payload_bytes: u64,
    request_payload_bytes: u64,
    target: Option<TargetInfo>,
    relay: Option<RelayInfo>,
}

#[derive(Debug, Deserialize)]
struct TargetInfo {
    address: String,
}

#[derive(Debug, Deserialize)]
struct RelayInfo {
    mode: String,
    timeout_ms: u64,
    timed_out: bool,
    upstream_tx_bytes: u64,
    upstream_rx_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct ProbeSummary {
    status: &'static str,
    version: &'static str,
    control: ControlSummary,
    node: NodeSummary,
    probe: ProbeStepSummary,
    session_data: Option<SessionDataStepSummary>,
}

#[derive(Debug, Serialize)]
struct ControlSummary {
    url: String,
    intent_id: String,
    ttl_sec: u64,
    credential_intent_id: String,
    credential_expires_at: u64,
}

#[derive(Debug, Serialize)]
struct NodeSummary {
    node_id: u64,
    node_version: String,
    address: String,
    area: String,
    tag: String,
    transports: Vec<String>,
    bandwidth_quality: String,
    route: CandidateRoute,
}

#[derive(Debug, Serialize)]
struct ProbeStepSummary {
    latency_ms: u128,
    transport: String,
    session_id: String,
    ttl_sec: u64,
    intent_id: Option<String>,
    route_target_addr: Option<String>,
    credential_valid: bool,
    credential_expires_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SessionDataStepSummary {
    latency_ms: u128,
    status: String,
    request_payload_bytes: u64,
    response_payload_bytes: u64,
    response_payload_base64: String,
    response_payload_text: Option<String>,
    target_addr: Option<String>,
    relay: Option<RelaySummary>,
}

#[derive(Debug, Serialize)]
struct RelaySummary {
    mode: String,
    timeout_ms: u64,
    timed_out: bool,
    upstream_tx_bytes: u64,
    upstream_rx_bytes: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    validate_cli(&cli)?;

    let deadline = Duration::from_secs(cli.timeout_sec.max(1));
    let connect_intent = request_connect_intent(&cli, deadline).await?;
    let candidate = connect_intent
        .candidates
        .get(cli.candidate_index)
        .cloned()
        .with_context(|| {
            format!(
                "candidate index {} is out of range; control-api returned {} candidates",
                cli.candidate_index,
                connect_intent.candidates.len()
            )
        })?;

    if !candidate
        .transports
        .iter()
        .any(|transport| transport.eq_ignore_ascii_case("udp"))
    {
        bail!("selected candidate does not advertise udp transport");
    }

    let node_addr = format!("{}:{}", candidate.host, candidate.port)
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid node address {}:{}", candidate.host, candidate.port))?;
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("failed to bind local UDP socket")?;
    socket
        .connect(node_addr)
        .await
        .with_context(|| format!("failed to connect UDP socket to {node_addr}"))?;

    let probe_nonce = format!("probe-{}-{}", cli.user_id, now_unix());
    let probe_request = ProbeRequest {
        message_type: "probe",
        protocol: PROTOCOL_VERSION,
        client_nonce: probe_nonce,
        user_id: cli.user_id,
        device_id: cli.device_id.clone(),
        game_id: cli.game_id,
        region_id: cli.region_id,
        transport: "udp",
        token: candidate.credential.token.clone(),
    };
    let probe_timer = Instant::now();
    let probe_value = send_json_udp(&socket, &probe_request, deadline).await?;
    ensure_node_success(&probe_value, "probe")?;
    let probe_response: ProbeResponse =
        serde_json::from_value(probe_value).context("failed to decode probe response")?;
    if probe_response.message_type != "probe.ok" {
        bail!(
            "unexpected probe response type: {}",
            probe_response.message_type
        );
    }
    if probe_response
        .node_id
        .is_some_and(|node_id| node_id != candidate.node_id)
    {
        bail!(
            "probe response node_id does not match selected candidate: expected {}, got {:?}",
            candidate.node_id,
            probe_response.node_id
        );
    }
    let probe_latency_ms = probe_timer.elapsed().as_millis();

    let session_data = if cli.skip_session_data {
        None
    } else {
        Some(run_session_data(&cli, &socket, &probe_response.session.session_id, deadline).await?)
    };

    let summary = ProbeSummary {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        control: ControlSummary {
            url: connect_intent_url(&cli.control_url),
            intent_id: connect_intent.intent_id,
            ttl_sec: connect_intent.ttl_sec,
            credential_intent_id: candidate.credential.intent_id,
            credential_expires_at: candidate.credential.expires_at,
        },
        node: NodeSummary {
            node_id: candidate.node_id,
            node_version: probe_response.node_version,
            address: node_addr.to_string(),
            area: candidate.area,
            tag: candidate.tag,
            transports: candidate.transports,
            bandwidth_quality: candidate.bandwidth_quality,
            route: candidate.route,
        },
        probe: ProbeStepSummary {
            latency_ms: probe_latency_ms,
            transport: probe_response.transport,
            session_id: probe_response.session.session_id,
            ttl_sec: probe_response.session.ttl_sec,
            intent_id: probe_response.session.intent_id,
            route_target_addr: probe_response.session.route_target_addr,
            credential_valid: probe_response.session.credential_valid,
            credential_expires_at: probe_response.session.credential_expires_at,
        },
        session_data,
    };

    if cli.compact {
        println!("{}", serde_json::to_string(&summary)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }
    Ok(())
}

fn validate_cli(cli: &Cli) -> anyhow::Result<()> {
    if cli.control_url.trim().is_empty() {
        bail!("--control-url must not be empty");
    }
    if cli.device_id.trim().is_empty() {
        bail!("--device-id must not be empty");
    }
    if !matches!(cli.bandwidth_quality.as_str(), "fast" | "normal" | "slow") {
        bail!("--bandwidth-quality must be fast, normal, or slow");
    }
    if cli.response_timeout_ms == 0 {
        bail!("--response-timeout-ms must be positive");
    }
    Ok(())
}

async fn request_connect_intent(
    cli: &Cli,
    deadline: Duration,
) -> anyhow::Result<ConnectIntentResponse> {
    let request = ConnectIntentRequest {
        user_id: cli.user_id,
        device_id: cli.device_id.clone(),
        game_id: cli.game_id,
        region_id: cli.region_id,
        platform: cli.platform.clone(),
        client_isp: cli.client_isp.clone(),
        client_ip: cli.client_ip.clone(),
        bandwidth_quality: cli.bandwidth_quality.clone(),
    };
    let client = reqwest::Client::builder()
        .timeout(deadline)
        .build()
        .context("failed to build HTTP client")?;
    let response = client
        .post(connect_intent_url(&cli.control_url))
        .json(&request)
        .send()
        .await
        .context("failed to request connect-intent")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read connect-intent response body")?;
    if !status.is_success() {
        bail!("connect-intent failed with HTTP {status}: {body}");
    }
    serde_json::from_str(&body).context("failed to decode connect-intent response")
}

async fn run_session_data(
    cli: &Cli,
    socket: &UdpSocket,
    session_id: &str,
    deadline: Duration,
) -> anyhow::Result<SessionDataStepSummary> {
    let request = SessionDataRequest {
        message_type: "session.data",
        protocol: PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        client_nonce: format!("data-{}-{}", cli.user_id, now_unix()),
        payload: BASE64.encode(cli.payload.as_bytes()),
        response_timeout_ms: cli.response_timeout_ms,
    };
    let timer = Instant::now();
    let response_value = send_json_udp(socket, &request, deadline).await?;
    ensure_node_success(&response_value, "session.data")?;
    let response: SessionDataResponse =
        serde_json::from_value(response_value).context("failed to decode session.data response")?;
    if response.message_type != "session.data.ok" {
        bail!(
            "unexpected session.data response type: {}",
            response.message_type
        );
    }

    let response_payload_text = decode_payload_text(&response.payload);

    Ok(SessionDataStepSummary {
        latency_ms: timer.elapsed().as_millis(),
        status: response.status,
        request_payload_bytes: response.request_payload_bytes,
        response_payload_bytes: response.payload_bytes,
        response_payload_base64: response.payload,
        response_payload_text,
        target_addr: response.target.map(|target| target.address),
        relay: response.relay.map(|relay| RelaySummary {
            mode: relay.mode,
            timeout_ms: relay.timeout_ms,
            timed_out: relay.timed_out,
            upstream_tx_bytes: relay.upstream_tx_bytes,
            upstream_rx_bytes: relay.upstream_rx_bytes,
        }),
    })
}

async fn send_json_udp<T: Serialize>(
    socket: &UdpSocket,
    message: &T,
    deadline: Duration,
) -> anyhow::Result<Value> {
    let mut encoded = serde_json::to_vec(message).context("failed to encode UDP JSON message")?;
    encoded.push(b'\n');
    socket
        .send(&encoded)
        .await
        .context("failed to send UDP message")?;

    let mut buf = vec![0u8; UDP_BUFFER_BYTES];
    let size = timeout(deadline, socket.recv(&mut buf))
        .await
        .context("timed out waiting for UDP response")?
        .context("failed to receive UDP response")?;
    serde_json::from_slice(&buf[..size]).context("failed to decode UDP JSON response")
}

fn ensure_node_success(value: &Value, step: &str) -> anyhow::Result<()> {
    let Some(message_type) = value.get("type").and_then(Value::as_str) else {
        bail!("{step} response is missing type");
    };
    if message_type.ends_with(".error") {
        let error = serde_json::from_value::<ErrorResponse>(value.clone())
            .context("failed to decode node error response")?;
        bail!(
            "{step} failed: {}: {}",
            error.error.code,
            error.error.message
        );
    }
    Ok(())
}

fn connect_intent_url(control_url: &str) -> String {
    format!(
        "{}/api/client/v1/connect-intent",
        control_url.trim().trim_end_matches('/')
    )
}

fn decode_payload_text(payload: &str) -> Option<String> {
    let bytes = BASE64.decode(payload.as_bytes()).ok()?;
    String::from_utf8(bytes).ok()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_connect_intent_url_without_double_slash() {
        assert_eq!(
            connect_intent_url("http://127.0.0.1:18080/"),
            "http://127.0.0.1:18080/api/client/v1/connect-intent"
        );
    }

    #[test]
    fn decodes_text_payload() {
        assert_eq!(decode_payload_text("aGVsbG8=").as_deref(), Some("hello"));
    }

    #[test]
    fn ignores_non_utf8_payload_text() {
        assert!(decode_payload_text("//4=").is_none());
    }
}
