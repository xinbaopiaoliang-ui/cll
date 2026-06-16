use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{net::UdpSocket, time::timeout};

const PROTOCOL_VERSION: &str = "xaccel/1";
const UDP_BUFFER_BYTES: usize = 64 * 1024;
const RAW_UDP_TUNNEL_MAGIC: &[u8; 4] = b"XAU1";
const RAW_UDP_TUNNEL_VERSION: u8 = 1;
const RAW_UDP_KIND_PACKET: u8 = 1;
const RAW_UDP_KIND_RESPONSE: u8 = 2;
const RAW_UDP_HEADER_BYTES: usize = 20;
const RAW_UDP_RELAY_TIMEOUT_MS: u64 = 200;

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

    #[arg(long, env = "XACCEL_CLIENT_API_TOKEN")]
    client_api_token: Option<String>,

    #[arg(long, env = "XACCEL_ACCEL_TICKET_FILE")]
    accel_ticket_file: Option<PathBuf>,

    #[arg(long, env = "XACCEL_ACCEL_TICKET_JSON")]
    accel_ticket_json: Option<String>,

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

    #[arg(long, default_value = "json")]
    session_data_mode: String,

    #[arg(long)]
    compact: bool,

    #[arg(long)]
    target_host: Option<String>,

    #[arg(long)]
    target_port: Option<u16>,

    #[arg(long, default_value = "udp")]
    target_protocol: String,

    #[arg(long)]
    target_id: Option<String>,

    #[arg(long)]
    original_domain: Option<String>,
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
    #[serde(default, skip)]
    ticket_client: Option<AccelTicketClient>,
    #[serde(default, skip)]
    ticket_route_policy: Option<RoutePolicy>,
}

#[derive(Debug, Deserialize)]
struct AccelTicket {
    ticket_id: String,
    ttl_sec: u64,
    client: AccelTicketClient,
    node: AccelTicketNode,
    route: CandidateRoute,
    #[serde(default)]
    route_policy: Option<RoutePolicy>,
    credential: CandidateCredential,
}

#[derive(Debug, Clone, Deserialize)]
struct AccelTicketClient {
    user_id: u64,
    device_id: String,
    game_id: u64,
    region_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct AccelTicketNode {
    node_id: u64,
    host: String,
    port: u16,
    #[serde(default = "default_area")]
    area: String,
    #[serde(default = "default_tag")]
    tag: String,
    #[serde(default = "default_transports")]
    transports: Vec<String>,
    #[serde(default = "default_bandwidth_quality")]
    bandwidth_quality: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RoutePolicy {
    policy_id: String,
    policy_version: u32,
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dns_strategy: Option<String>,
    #[serde(default)]
    targets: Vec<RouteTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RouteTarget {
    target_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    purpose: Option<String>,
    host_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default)]
    resolved_ips: Vec<String>,
    #[serde(default)]
    observed_ips: Vec<String>,
    #[serde(default)]
    cidrs: Vec<String>,
    #[serde(default)]
    ports: Vec<PortRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_client_observed_ip: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolve_ttl_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PortRange {
    protocol: String,
    from: u16,
    to: u16,
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
    #[serde(default)]
    route_policy: Option<RoutePolicy>,
    credential: CandidateCredential,
    scheduler: Option<CandidateScheduler>,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CandidateScheduler {
    route_priority: u32,
    latest_active_sessions: u32,
    latest_udp_sessions: u32,
    latest_tcp_sessions: u32,
    latest_reported_at: Option<u64>,
    latest_report_age_sec: Option<u64>,
    report_fresh: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    route_policy: Option<RoutePolicy>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<SessionDataTarget>,
    payload: String,
    response_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
struct SessionDataTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    target_id: Option<String>,
    protocol: String,
    host: String,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_domain: Option<String>,
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
    target_id: Option<String>,
    protocol: Option<String>,
    address: String,
    matched_policy: Option<String>,
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
    scheduler: Option<CandidateScheduler>,
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
    mode: String,
    status: String,
    request_payload_bytes: u64,
    response_payload_bytes: u64,
    response_payload_base64: String,
    response_payload_text: Option<String>,
    target_id: Option<String>,
    target_protocol: Option<String>,
    target_addr: Option<String>,
    matched_policy: Option<String>,
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
    let connect_intent = match load_accel_ticket(&cli)? {
        Some(ticket) => connect_intent_from_ticket(ticket),
        None => request_connect_intent(&cli, deadline).await?,
    };
    let ticket_client = connect_intent.ticket_client.clone();
    let probe_user_id = ticket_client
        .as_ref()
        .map(|client| client.user_id)
        .unwrap_or(cli.user_id);
    let probe_device_id = ticket_client
        .as_ref()
        .map(|client| client.device_id.clone())
        .unwrap_or_else(|| cli.device_id.clone());
    let probe_game_id = ticket_client
        .as_ref()
        .map(|client| client.game_id)
        .unwrap_or(cli.game_id);
    let probe_region_id = ticket_client
        .as_ref()
        .map(|client| client.region_id)
        .unwrap_or(cli.region_id);
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

    let probe_nonce = format!("probe-{}-{}", probe_user_id, now_unix());
    let route_policy = candidate
        .route_policy
        .clone()
        .or_else(|| connect_intent.ticket_route_policy.clone());
    let probe_request = ProbeRequest {
        message_type: "probe",
        protocol: PROTOCOL_VERSION,
        client_nonce: probe_nonce,
        user_id: probe_user_id,
        device_id: probe_device_id,
        game_id: probe_game_id,
        region_id: probe_region_id,
        transport: "udp",
        token: candidate.credential.token.clone(),
        route_policy: route_policy.clone(),
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
        Some(
            run_session_data(
                &cli,
                &socket,
                &probe_response.session.session_id,
                deadline,
                route_policy.as_ref(),
            )
            .await?,
        )
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
            scheduler: candidate.scheduler,
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
    if cli.accel_ticket_file.is_some() && cli.accel_ticket_json.is_some() {
        bail!("use only one of --accel-ticket-file or --accel-ticket-json");
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
    if !matches!(
        cli.session_data_mode.to_ascii_lowercase().as_str(),
        "json" | "raw-udp"
    ) {
        bail!("--session-data-mode must be json or raw-udp");
    }
    if cli.target_host.is_some() ^ cli.target_port.is_some() {
        bail!("--target-host and --target-port must be provided together");
    }
    if !matches!(
        cli.target_protocol.to_ascii_lowercase().as_str(),
        "udp" | "tcp"
    ) {
        bail!("--target-protocol must be udp or tcp");
    }
    Ok(())
}

fn load_accel_ticket(cli: &Cli) -> anyhow::Result<Option<AccelTicket>> {
    let raw = if let Some(json) = cli.accel_ticket_json.as_ref() {
        Some(json.clone())
    } else if let Some(path) = cli.accel_ticket_file.as_ref() {
        Some(
            fs::read_to_string(path)
                .with_context(|| format!("failed to read accel ticket file {}", path.display()))?,
        )
    } else {
        None
    };

    let Some(raw) = raw else {
        return Ok(None);
    };

    let value: Value = serde_json::from_str(&raw).context("failed to decode accel ticket JSON")?;
    Ok(Some(decode_accel_ticket(value)?))
}

fn decode_accel_ticket(value: Value) -> anyhow::Result<AccelTicket> {
    if let Some(ticket) = value.get("accel_ticket") {
        return serde_json::from_value(ticket.clone()).context("failed to decode accel_ticket");
    }
    if let Some(ticket) = value.pointer("/result/accel_ticket") {
        return serde_json::from_value(ticket.clone())
            .context("failed to decode result.accel_ticket");
    }
    serde_json::from_value(value).context("failed to decode raw accel ticket")
}

fn connect_intent_from_ticket(ticket: AccelTicket) -> ConnectIntentResponse {
    let route_policy = ticket.route_policy.clone();
    ConnectIntentResponse {
        intent_id: ticket.ticket_id,
        ttl_sec: ticket.ttl_sec,
        ticket_client: Some(ticket.client),
        ticket_route_policy: route_policy.clone(),
        candidates: vec![NodeCandidate {
            node_id: ticket.node.node_id,
            area: ticket.node.area,
            tag: ticket.node.tag,
            host: ticket.node.host,
            port: ticket.node.port,
            transports: ticket.node.transports,
            bandwidth_quality: ticket.node.bandwidth_quality,
            route: ticket.route.clone(),
            route_policy,
            credential: ticket.credential,
            scheduler: None,
        }],
    }
}

fn default_area() -> String {
    "UNKNOWN".to_string()
}

fn default_tag() -> String {
    "default".to_string()
}

fn default_transports() -> Vec<String> {
    vec!["udp".to_string()]
}

fn default_bandwidth_quality() -> String {
    "normal".to_string()
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
    let mut request_builder = client
        .post(connect_intent_url(&cli.control_url))
        .json(&request);
    if let Some(token) = cli
        .client_api_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        request_builder = request_builder.bearer_auth(token);
    }
    let response = request_builder
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
    route_policy: Option<&RoutePolicy>,
) -> anyhow::Result<SessionDataStepSummary> {
    if cli.session_data_mode.eq_ignore_ascii_case("raw-udp") {
        return run_raw_udp_tunnel(cli, socket, session_id, deadline, route_policy).await;
    }

    let target = select_session_target(cli, route_policy)?;
    let request = SessionDataRequest {
        message_type: "session.data",
        protocol: PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        client_nonce: format!("data-{}-{}", cli.user_id, now_unix()),
        target,
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

    let target = response.target;

    Ok(SessionDataStepSummary {
        latency_ms: timer.elapsed().as_millis(),
        mode: "json".to_string(),
        status: response.status,
        request_payload_bytes: response.request_payload_bytes,
        response_payload_bytes: response.payload_bytes,
        response_payload_base64: response.payload,
        response_payload_text,
        target_id: target.as_ref().and_then(|target| target.target_id.clone()),
        target_protocol: target.as_ref().and_then(|target| target.protocol.clone()),
        target_addr: target.as_ref().map(|target| target.address.clone()),
        matched_policy: target.and_then(|target| target.matched_policy),
        relay: response.relay.map(|relay| RelaySummary {
            mode: relay.mode,
            timeout_ms: relay.timeout_ms,
            timed_out: relay.timed_out,
            upstream_tx_bytes: relay.upstream_tx_bytes,
            upstream_rx_bytes: relay.upstream_rx_bytes,
        }),
    })
}

async fn run_raw_udp_tunnel(
    cli: &Cli,
    socket: &UdpSocket,
    session_id: &str,
    deadline: Duration,
    route_policy: Option<&RoutePolicy>,
) -> anyhow::Result<SessionDataStepSummary> {
    let Some(target) = select_session_target(cli, route_policy)? else {
        bail!("raw UDP tunnel requires a concrete target");
    };
    if !target.protocol.eq_ignore_ascii_case("udp") {
        bail!("raw UDP tunnel requires --target-protocol udp");
    }

    let payload = cli.payload.as_bytes();
    let frame = encode_raw_udp_tunnel_frame(
        session_id,
        target.target_id.as_deref().unwrap_or_default(),
        &target.host,
        target.port,
        payload,
    )?;
    let timer = Instant::now();
    let response = send_raw_udp_tunnel(socket, &frame, deadline).await?;
    if response.session_id != session_id {
        bail!(
            "raw UDP response session_id does not match probe session: expected {}, got {}",
            session_id,
            response.session_id
        );
    }

    let response_payload_base64 = BASE64.encode(&response.payload);
    let response_payload_text = String::from_utf8(response.payload.clone()).ok();
    let response_payload_bytes = response.payload.len() as u64;
    let timed_out = response.status_code == 1 || response.status == "upstream_timeout";
    Ok(SessionDataStepSummary {
        latency_ms: timer.elapsed().as_millis(),
        mode: "raw_udp".to_string(),
        status: response.status,
        request_payload_bytes: payload.len() as u64,
        response_payload_bytes,
        response_payload_base64,
        response_payload_text,
        target_id: target.target_id,
        target_protocol: Some(target.protocol),
        target_addr: Some(format!("{}:{}", target.host, target.port)),
        matched_policy: None,
        relay: Some(RelaySummary {
            mode: "raw_udp_tunnel".to_string(),
            timeout_ms: RAW_UDP_RELAY_TIMEOUT_MS,
            timed_out,
            upstream_tx_bytes: payload.len() as u64,
            upstream_rx_bytes: response_payload_bytes,
        }),
    })
}

fn select_session_target(
    cli: &Cli,
    route_policy: Option<&RoutePolicy>,
) -> anyhow::Result<Option<SessionDataTarget>> {
    if let (Some(host), Some(port)) = (cli.target_host.as_ref(), cli.target_port) {
        return Ok(Some(SessionDataTarget {
            target_id: cli.target_id.clone(),
            protocol: cli.target_protocol.to_ascii_lowercase(),
            host: host.trim().to_string(),
            port,
            original_domain: cli.original_domain.clone(),
        }));
    }

    let Some(route_policy) = route_policy else {
        return Ok(None);
    };

    let requested_protocol = cli.target_protocol.to_ascii_lowercase();
    for target in &route_policy.targets {
        let Some(port) = target
            .ports
            .iter()
            .find(|port| port.protocol.eq_ignore_ascii_case(&requested_protocol))
        else {
            continue;
        };
        if let Some(host) = target
            .host
            .as_deref()
            .or_else(|| target.resolved_ips.first().map(String::as_str))
            .or_else(|| target.observed_ips.first().map(String::as_str))
        {
            return Ok(Some(SessionDataTarget {
                target_id: Some(target.target_id.clone()),
                protocol: requested_protocol,
                host: host.to_string(),
                port: port.from,
                original_domain: target
                    .host
                    .as_ref()
                    .filter(|_| target.host_type == "domain")
                    .cloned(),
            }));
        }
    }

    bail!(
        "route_policy does not contain a concrete target for --target-protocol; pass --target-host and --target-port"
    );
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

async fn send_raw_udp_tunnel(
    socket: &UdpSocket,
    frame: &[u8],
    deadline: Duration,
) -> anyhow::Result<RawUdpTunnelResponse> {
    socket
        .send(frame)
        .await
        .context("failed to send raw UDP tunnel frame")?;

    let mut buf = vec![0u8; UDP_BUFFER_BYTES];
    let size = timeout(deadline, socket.recv(&mut buf))
        .await
        .context("timed out waiting for raw UDP tunnel response")?
        .context("failed to receive raw UDP tunnel response")?;
    decode_raw_udp_tunnel_response(&buf[..size])
}

#[derive(Debug)]
struct RawUdpTunnelResponse {
    status_code: u8,
    session_id: String,
    status: String,
    payload: Vec<u8>,
}

fn encode_raw_udp_tunnel_frame(
    session_id: &str,
    target_id: &str,
    host: &str,
    port: u16,
    payload: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let session_bytes = session_id.as_bytes();
    let target_bytes = target_id.as_bytes();
    let host_bytes = host.as_bytes();
    let session_len = u16::try_from(session_bytes.len()).context("session_id is too long")?;
    let target_len = u16::try_from(target_bytes.len()).context("target_id is too long")?;
    let host_len = u16::try_from(host_bytes.len()).context("target host is too long")?;
    let payload_len = u32::try_from(payload.len()).context("payload is too large")?;

    let mut frame = Vec::with_capacity(
        RAW_UDP_HEADER_BYTES
            + session_bytes.len()
            + target_bytes.len()
            + host_bytes.len()
            + payload.len(),
    );
    frame.extend_from_slice(RAW_UDP_TUNNEL_MAGIC);
    frame.push(RAW_UDP_TUNNEL_VERSION);
    frame.push(RAW_UDP_KIND_PACKET);
    frame.push(0);
    frame.push(0);
    frame.extend_from_slice(&session_len.to_be_bytes());
    frame.extend_from_slice(&target_len.to_be_bytes());
    frame.extend_from_slice(&host_len.to_be_bytes());
    frame.extend_from_slice(&port.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(session_bytes);
    frame.extend_from_slice(target_bytes);
    frame.extend_from_slice(host_bytes);
    frame.extend_from_slice(payload);
    Ok(frame)
}

fn decode_raw_udp_tunnel_response(bytes: &[u8]) -> anyhow::Result<RawUdpTunnelResponse> {
    if bytes.len() < RAW_UDP_HEADER_BYTES {
        bail!("raw UDP tunnel response is shorter than header");
    }
    if &bytes[..4] != RAW_UDP_TUNNEL_MAGIC {
        bail!("raw UDP tunnel response magic mismatch");
    }
    if bytes[4] != RAW_UDP_TUNNEL_VERSION {
        bail!("raw UDP tunnel response version mismatch");
    }
    if bytes[5] != RAW_UDP_KIND_RESPONSE {
        bail!("raw UDP tunnel response kind mismatch");
    }

    let session_len = read_u16(bytes, 8)? as usize;
    let status_len = read_u16(bytes, 10)? as usize;
    let payload_len = read_u32(bytes, 16)? as usize;
    let total_len = RAW_UDP_HEADER_BYTES
        .checked_add(session_len)
        .and_then(|value| value.checked_add(status_len))
        .and_then(|value| value.checked_add(payload_len))
        .context("raw UDP tunnel response length overflow")?;
    if bytes.len() != total_len {
        bail!(
            "raw UDP tunnel response length mismatch: expected {}, got {}",
            total_len,
            bytes.len()
        );
    }

    let mut offset = RAW_UDP_HEADER_BYTES;
    let session_id = read_utf8_field(bytes, &mut offset, session_len, "session_id")?;
    let status = read_utf8_field(bytes, &mut offset, status_len, "status")?;
    let payload = bytes[offset..offset + payload_len].to_vec();

    Ok(RawUdpTunnelResponse {
        status_code: bytes[6],
        session_id,
        status,
        payload,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> anyhow::Result<u16> {
    let field = bytes
        .get(offset..offset + 2)
        .context("raw UDP tunnel response is truncated")?;
    Ok(u16::from_be_bytes([field[0], field[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> anyhow::Result<u32> {
    let field = bytes
        .get(offset..offset + 4)
        .context("raw UDP tunnel response is truncated")?;
    Ok(u32::from_be_bytes([field[0], field[1], field[2], field[3]]))
}

fn read_utf8_field(
    bytes: &[u8],
    offset: &mut usize,
    len: usize,
    name: &str,
) -> anyhow::Result<String> {
    let end = offset
        .checked_add(len)
        .context("raw UDP tunnel response field length overflow")?;
    let field = bytes
        .get(*offset..end)
        .with_context(|| format!("raw UDP tunnel response {name} is truncated"))?;
    *offset = end;
    String::from_utf8(field.to_vec())
        .with_context(|| format!("raw UDP tunnel response {name} is not UTF-8"))
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

    #[test]
    fn decodes_wrapped_accel_ticket() {
        let value = serde_json::json!({
            "status": "ok",
            "accel_ticket": {
                "ticket_id": "intent-1001-8888-1-2",
                "ttl_sec": 120,
                "client": {
                    "user_id": 1001,
                    "device_id": "pc-001",
                    "game_id": 8888,
                    "region_id": 1
                },
                "node": {
                    "node_id": 2,
                    "host": "47.83.160.126",
                    "port": 666
                },
                "route": {
                    "target_addr": "127.0.0.1:7777",
                    "protocol": "udp",
                    "region_id": 1,
                    "region_name": "Default"
                },
                "credential": {
                    "token": "xat.v1.payload.signature",
                    "expires_at": 9999999999_u64,
                    "intent_id": "intent-1001-8888-1-2"
                }
            }
        });

        let ticket = decode_accel_ticket(value).expect("ticket decodes");
        let intent = connect_intent_from_ticket(ticket);
        assert_eq!(intent.intent_id, "intent-1001-8888-1-2");
        assert_eq!(intent.candidates[0].node_id, 2);
        assert_eq!(intent.candidates[0].area, "UNKNOWN");
        assert_eq!(intent.candidates[0].route.target_addr, "127.0.0.1:7777");
        assert_eq!(
            intent.ticket_client.as_ref().map(|client| client.region_id),
            Some(Some(1))
        );
    }

    #[test]
    fn preserves_missing_ticket_region() {
        let value = serde_json::json!({
            "ticket_id": "intent-1001-8888-1-2",
            "ttl_sec": 120,
            "client": {
                "user_id": 1001,
                "device_id": "pc-001",
                "game_id": 8888
            },
            "node": {
                "node_id": 2,
                "host": "47.83.160.126",
                "port": 666
            },
            "route": {
                "target_addr": "127.0.0.1:7777",
                "protocol": "udp"
            },
            "credential": {
                "token": "xat.v1.payload.signature",
                "expires_at": 9999999999_u64,
                "intent_id": "intent-1001-8888-1-2"
            }
        });

        let ticket = decode_accel_ticket(value).expect("ticket decodes");
        let intent = connect_intent_from_ticket(ticket);
        assert_eq!(
            intent.ticket_client.as_ref().map(|client| client.region_id),
            Some(None)
        );
    }

    #[test]
    fn validates_tcp_target_protocol() {
        let cli = Cli::parse_from([
            "xaccel-client-probe",
            "--target-host",
            "127.0.0.1",
            "--target-port",
            "7788",
            "--target-protocol",
            "tcp",
        ]);

        validate_cli(&cli).expect("tcp target protocol is valid");
    }

    #[test]
    fn encodes_raw_udp_tunnel_frame() {
        let frame =
            encode_raw_udp_tunnel_frame("ps-udp-test", "udp-echo", "127.0.0.1", 7777, b"hello")
                .expect("frame encodes");

        assert_eq!(&frame[..4], b"XAU1");
        assert_eq!(frame[4], RAW_UDP_TUNNEL_VERSION);
        assert_eq!(frame[5], RAW_UDP_KIND_PACKET);
        assert_eq!(u16::from_be_bytes([frame[8], frame[9]]), 11);
        assert_eq!(u16::from_be_bytes([frame[10], frame[11]]), 8);
        assert_eq!(u16::from_be_bytes([frame[12], frame[13]]), 9);
        assert_eq!(u16::from_be_bytes([frame[14], frame[15]]), 7777);
        assert_eq!(
            u32::from_be_bytes([frame[16], frame[17], frame[18], frame[19]]),
            5
        );
        assert_eq!(&frame[20..31], b"ps-udp-test");
        assert_eq!(&frame[31..39], b"udp-echo");
        assert_eq!(&frame[39..48], b"127.0.0.1");
        assert_eq!(&frame[48..], b"hello");
    }

    #[test]
    fn decodes_raw_udp_tunnel_response() {
        let mut frame = Vec::new();
        let session = b"ps-udp-test";
        let status = b"forwarded";
        let payload = b"udp:hello";
        frame.extend_from_slice(b"XAU1");
        frame.push(RAW_UDP_TUNNEL_VERSION);
        frame.push(RAW_UDP_KIND_RESPONSE);
        frame.push(0);
        frame.push(0);
        frame.extend_from_slice(&(session.len() as u16).to_be_bytes());
        frame.extend_from_slice(&(status.len() as u16).to_be_bytes());
        frame.extend_from_slice(&0_u16.to_be_bytes());
        frame.extend_from_slice(&0_u16.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(session);
        frame.extend_from_slice(status);
        frame.extend_from_slice(payload);

        let response = decode_raw_udp_tunnel_response(&frame).expect("response decodes");
        assert_eq!(response.status_code, 0);
        assert_eq!(response.session_id, "ps-udp-test");
        assert_eq!(response.status, "forwarded");
        assert_eq!(response.payload, b"udp:hello");
    }
}
