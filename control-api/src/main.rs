use anyhow::{bail, Context};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use base64::{
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
    Engine,
};
use clap::Parser;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{mysql::MySqlPoolOptions, FromRow, MySql, MySqlPool, QueryBuilder};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tracing::{error, info};

type HmacSha256 = Hmac<Sha256>;

const TOKEN_PREFIX: &str = "xat";
const TOKEN_VERSION: &str = "v1";
const NODE_REPORT_PATH: &str = "/api/node/v1/report";
const NODE_HANDSHAKE_PATH: &str = "/api/node/v1/handshake";
const NODE_CONFIG_PATH: &str = "/api/node/v1/config";
const NODE_REPORT_MAX_SKEW_SEC: u64 = 300;
const NODE_BOOTSTRAP_PATH: &str = "/api/node/v1/bootstrap";
const DEFAULT_BOOTSTRAP_TTL_SEC: u64 = 3600;
const MAX_BOOTSTRAP_TTL_SEC: u64 = 86_400;
const DEFAULT_INSTALL_URL: &str =
    "https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MIN_NODE_VERSION: &str = "0.1.0";
const ADMIN_DASHBOARD_HTML: &str = include_str!("../static/admin-dashboard.html");

#[derive(Debug, Parser)]
#[command(name = "xaccel-control-api")]
#[command(about = "XAccel Rust control-plane API backed by MySQL")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:18080")]
    listen: SocketAddr,

    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    #[arg(long, default_value_t = 120)]
    token_ttl_sec: u64,

    #[arg(long, default_value_t = 8)]
    max_db_connections: u32,

    #[arg(long, env = "XACCEL_ADMIN_TOKEN")]
    admin_token: Option<String>,

    #[arg(long, env = "XACCEL_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,
}

#[derive(Clone)]
struct AppState {
    pool: MySqlPool,
    token_ttl_sec: u64,
    admin_token: Option<String>,
    public_base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConnectIntentRequest {
    user_id: u64,
    device_id: String,
    game_id: u64,
    platform: Option<String>,
    client_isp: Option<String>,
    client_ip: Option<String>,
    bandwidth_quality: Option<String>,
}

#[derive(Debug, Serialize)]
struct ConnectIntentResponse {
    intent_id: String,
    ttl_sec: u64,
    client: ClientContext,
    candidates: Vec<NodeCandidate>,
}

#[derive(Debug, Serialize)]
struct ClientContext {
    platform: Option<String>,
    client_isp: Option<String>,
    client_ip: Option<String>,
    bandwidth_quality: String,
}

#[derive(Debug, Serialize)]
struct NodeCandidate {
    node_id: u64,
    area: String,
    tag: String,
    host: String,
    port: u16,
    transports: Vec<&'static str>,
    bandwidth_quality: String,
    probe: ProbeInfo,
    route: ClientRouteClaims,
    credential: CredentialInfo,
}

#[derive(Debug, Serialize)]
struct ProbeInfo {
    udp: bool,
    tcp: bool,
    protocol: &'static str,
}

#[derive(Debug, Serialize)]
struct CredentialInfo {
    token: String,
    expires_at: u64,
    intent_id: String,
    route: ClientRouteClaims,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientTokenClaims {
    node_id: u64,
    user_id: u64,
    device_id: String,
    game_id: u64,
    intent_id: Option<String>,
    route: Option<ClientRouteClaims>,
    expires_at: u64,
    issued_at: Option<u64>,
    nonce: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientRouteClaims {
    target_addr: String,
    protocol: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    database: &'static str,
}

#[derive(Debug, Deserialize, Serialize)]
struct NodeReportRequest {
    node_id: u64,
    config_revision: u64,
    node_version: String,
    status: String,
    timestamp: u64,
    health: NodeHealthSnapshot,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct NodeHealthSnapshot {
    #[serde(default)]
    listeners: NodeListenerSnapshot,
    #[serde(default)]
    traffic: NodeTrafficSnapshot,
    #[serde(default)]
    sessions: NodeSessionSnapshot,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct NodeListenerSnapshot {
    #[serde(default)]
    udp_listening: bool,
    #[serde(default)]
    tcp_listening: bool,
    listen_addr: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct NodeTrafficSnapshot {
    #[serde(default)]
    udp_rx_packets: u64,
    #[serde(default)]
    udp_rx_bytes: u64,
    #[serde(default)]
    udp_tx_packets: u64,
    #[serde(default)]
    udp_tx_bytes: u64,
    #[serde(default)]
    tcp_accepted: u64,
    #[serde(default)]
    tcp_rx_bytes: u64,
    #[serde(default)]
    tcp_tx_bytes: u64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct NodeSessionSnapshot {
    #[serde(default)]
    active_tcp_connections: u64,
    #[serde(default)]
    active_udp_sessions: u64,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Serialize)]
struct NodeReportResponse {
    status: &'static str,
    node_id: u64,
    stored: bool,
    server_time: u64,
}

#[derive(Debug, Deserialize)]
struct NodeHandshakeRequest {
    node_id: u64,
    node_version: String,
    os: String,
    arch: String,
    boot_id: String,
    timestamp: u64,
    nonce: String,
    #[serde(default)]
    config_revision: u64,
    listen_addr: Option<String>,
}

#[derive(Debug, Serialize)]
struct NodeHandshakeResponse {
    status: &'static str,
    node_id: u64,
    server_time: u64,
    config_revision: u64,
    min_node_version: &'static str,
    websocket: NodeHandshakeWebsocket,
}

#[derive(Debug, Serialize)]
struct NodeHandshakeWebsocket {
    enabled: bool,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BootstrapRequest {
    bootstrap_token: String,
    hostname: String,
    os: String,
    arch: String,
    kernel: String,
    ips: Vec<String>,
    installer_version: String,
}

#[derive(Debug, Serialize)]
struct BootstrapResponse {
    node_id: u64,
    node_secret: String,
    panel_url: String,
    server_ip: String,
    server_port: u32,
    config_revision: u64,
    release: BootstrapReleaseInfo,
}

#[derive(Debug, Serialize)]
struct BootstrapReleaseInfo {
    version: &'static str,
    manifest_url: String,
}

#[derive(Debug, Deserialize)]
struct AdminListNodesQuery {
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AdminNodeDetailQuery {
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AdminCreateNodeRequest {
    name: String,
    server_ip: String,
    server_port: u32,
    relay_server_ip: Option<String>,
    relay_server_port: Option<u32>,
    is_support_ipv6: Option<bool>,
    bandwidth_quality: Option<String>,
    disable_quic: Option<bool>,
    area: Option<String>,
    telecom_ip: Option<String>,
    mobile_ip: Option<String>,
    unicom_ip: Option<String>,
    tag: Option<String>,
}

type AdminUpdateNodeRequest = AdminCreateNodeRequest;

#[derive(Debug, Deserialize)]
struct AdminUpdateNodeStatusRequest {
    status: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdminCreateBootstrapTokenRequest {
    expires_in_sec: Option<u64>,
    created_by: Option<u64>,
    public_base_url: Option<String>,
    install_url: Option<String>,
    enable_control_plane: Option<bool>,
    channel: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdminListRouteRulesQuery {
    game_id: Option<u64>,
    node_id: Option<u64>,
    status: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AdminRouteRuleRequest {
    game_id: u64,
    game_name: String,
    node_id: u64,
    target_addr: String,
    protocol: Option<String>,
    area: Option<String>,
    tag: Option<String>,
    priority: Option<u32>,
    status: Option<String>,
}

type AdminUpdateRouteRuleRequest = AdminRouteRuleRequest;

#[derive(Debug, Serialize)]
struct AdminListNodesResponse {
    nodes: Vec<AdminNodeSummary>,
    total: usize,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminNodeDetailResponse {
    node: AdminNodeSummary,
    recent_reports: Vec<AdminReportDetail>,
    recent_audit_logs: Vec<AdminAuditLogDetail>,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminCreateNodeResponse {
    status: &'static str,
    node: AdminNodeSummary,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminUpdateNodeStatusResponse {
    status: &'static str,
    node_id: u64,
    previous_status: String,
    current_status: String,
    reason: Option<String>,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminUpdateNodeResponse {
    status: &'static str,
    node: AdminNodeSummary,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminCreateBootstrapTokenResponse {
    status: &'static str,
    node_id: u64,
    bootstrap_token: String,
    bootstrap_url: String,
    expires_at: u64,
    install_command: String,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminListRouteRulesResponse {
    rules: Vec<AdminRouteRuleSummary>,
    total: usize,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminRouteRuleResponse {
    status: &'static str,
    rule: AdminRouteRuleSummary,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminDeleteRouteRuleResponse {
    status: &'static str,
    rule_id: u64,
    deleted: bool,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminNodeSummary {
    id: u64,
    name: String,
    endpoint: String,
    server_ip: String,
    server_port: u32,
    relay_server_ip: Option<String>,
    relay_server_port: Option<u32>,
    is_support_ipv6: bool,
    area: String,
    tag: Option<String>,
    bandwidth_quality: String,
    disable_quic: bool,
    telecom_ip: Option<String>,
    mobile_ip: Option<String>,
    unicom_ip: Option<String>,
    status: String,
    kernel_version: Option<String>,
    config_revision: u64,
    last_seen_at: Option<u64>,
    last_report_at: Option<u64>,
    latest_report: Option<AdminReportSummary>,
}

#[derive(Debug, Serialize)]
struct AdminRouteRuleSummary {
    id: u64,
    game_id: u64,
    game_name: String,
    node_id: u64,
    node_name: String,
    node_endpoint: String,
    node_status: String,
    target_addr: String,
    protocol: String,
    area: Option<String>,
    tag: Option<String>,
    priority: u32,
    status: String,
    created_at: Option<u64>,
    updated_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdminReportSummary {
    id: u64,
    status: String,
    active_sessions: u32,
    udp_sessions: u32,
    tcp_sessions: u32,
    reported_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdminReportDetail {
    id: u64,
    node_id: u64,
    config_revision: u64,
    status: String,
    active_sessions: u32,
    udp_sessions: u32,
    tcp_sessions: u32,
    reported_at: Option<u64>,
    raw: Option<Value>,
}

#[derive(Debug, Serialize)]
struct AdminAuditLogDetail {
    id: u64,
    node_id: u64,
    actor_type: String,
    actor_id: Option<u64>,
    action: String,
    created_at: Option<u64>,
    detail: Option<Value>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorMessage,
}

#[derive(Debug, Serialize)]
struct ErrorMessage {
    code: &'static str,
    message: String,
}

#[derive(Debug, FromRow)]
struct CandidateRow {
    node_id: u64,
    server_ip: String,
    server_port: u32,
    area: String,
    tag: Option<String>,
    bandwidth_quality: String,
    node_secret: String,
    target_addr: String,
    protocol: String,
}

#[derive(Debug, FromRow)]
struct AdminNodeRow {
    id: u64,
    name: String,
    server_ip: String,
    server_port: u32,
    relay_server_ip: Option<String>,
    relay_server_port: Option<u32>,
    is_support_ipv6: i8,
    area: String,
    tag: Option<String>,
    bandwidth_quality: String,
    disable_quic: i8,
    telecom_ip: Option<String>,
    mobile_ip: Option<String>,
    unicom_ip: Option<String>,
    status: String,
    kernel_version: Option<String>,
    config_revision: u64,
    last_seen_at: Option<u64>,
    last_report_at: Option<u64>,
    latest_report_id: Option<u64>,
    latest_report_status: Option<String>,
    latest_active_sessions: Option<u32>,
    latest_udp_sessions: Option<u32>,
    latest_tcp_sessions: Option<u32>,
    latest_reported_at: Option<u64>,
}

#[derive(Debug, FromRow)]
struct AdminReportRow {
    id: u64,
    node_id: u64,
    config_revision: u64,
    status: String,
    active_sessions: u32,
    udp_sessions: u32,
    tcp_sessions: u32,
    reported_at: Option<u64>,
    raw_json: Option<String>,
}

#[derive(Debug, FromRow)]
struct AdminAuditLogRow {
    id: u64,
    node_id: u64,
    actor_type: String,
    actor_id: Option<u64>,
    action: String,
    created_at: Option<u64>,
    detail_json: Option<String>,
}

#[derive(Debug, FromRow)]
struct AdminRouteRuleRow {
    id: u64,
    game_id: u64,
    game_name: String,
    node_id: u64,
    node_name: String,
    node_server_ip: String,
    node_server_port: u32,
    node_status: String,
    target_addr: String,
    protocol: String,
    area: Option<String>,
    tag: Option<String>,
    priority: u32,
    status: String,
    created_at: Option<u64>,
    updated_at: Option<u64>,
}

#[derive(Debug)]
struct NormalizedCreateNode {
    name: String,
    server_ip: String,
    server_port: u32,
    relay_server_ip: Option<String>,
    relay_server_port: Option<u32>,
    is_support_ipv6: i8,
    bandwidth_quality: String,
    disable_quic: i8,
    area: String,
    telecom_ip: Option<String>,
    mobile_ip: Option<String>,
    unicom_ip: Option<String>,
    tag: Option<String>,
}

#[derive(Debug)]
struct NormalizedRouteRule {
    game_id: u64,
    game_name: String,
    node_id: u64,
    target_addr: String,
    protocol: String,
    area: Option<String>,
    tag: Option<String>,
    priority: u32,
    status: String,
}

#[derive(Debug, FromRow)]
struct NodeConfigRow {
    id: u64,
    server_ip: String,
    server_port: u32,
    relay_server_ip: Option<String>,
    relay_server_port: Option<u32>,
    is_support_ipv6: i8,
    bandwidth_quality: String,
    disable_quic: i8,
    area: String,
    telecom_ip: Option<String>,
    mobile_ip: Option<String>,
    unicom_ip: Option<String>,
    tag: Option<String>,
    config_revision: u64,
}

#[derive(Debug, Serialize)]
struct NodeConfigResponse {
    status: &'static str,
    node_id: u64,
    config_revision: u64,
    server_time: u64,
    network: NodeConfigNetworkResponse,
}

#[derive(Debug, Serialize)]
struct NodeConfigNetworkResponse {
    server_ip: String,
    listen_ip: String,
    server_port: u32,
    relay_server_ip: Option<String>,
    relay_server_port: Option<u32>,
    is_support_ipv6: bool,
    disable_quic: bool,
    area: String,
    bandwidth_quality: String,
    tag: Option<String>,
    operator_ips: NodeConfigOperatorIpsResponse,
}

#[derive(Debug, Serialize)]
struct NodeConfigOperatorIpsResponse {
    telecom_ip: Option<String>,
    mobile_ip: Option<String>,
    unicom_ip: Option<String>,
}

#[derive(Debug, FromRow)]
struct BootstrapExchangeRow {
    token_id: u64,
    node_id: u64,
    expires_at: u64,
    used_at: Option<u64>,
    node_secret: Option<String>,
    server_ip: String,
    server_port: u32,
    config_revision: u64,
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "xaccel_control_api=info".to_string()),
        )
        .init();

    let cli = Cli::parse();
    validate_cli(&cli)?;

    let pool = MySqlPoolOptions::new()
        .max_connections(cli.max_db_connections)
        .connect(&cli.database_url)
        .await
        .context("failed to connect MySQL")?;
    run_schema_migrations(&pool)
        .await
        .context("failed to run schema migrations")?;

    let state = AppState {
        pool,
        token_ttl_sec: cli.token_ttl_sec.max(1),
        admin_token: cli
            .admin_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(ToOwned::to_owned),
        public_base_url: cli
            .public_base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(trim_trailing_slash),
    };
    let app = Router::new()
        .route("/admin", get(admin_dashboard))
        .route("/health", get(health))
        .route("/api/client/v1/connect-intent", post(connect_intent))
        .route(NODE_BOOTSTRAP_PATH, post(node_bootstrap))
        .route(NODE_HANDSHAKE_PATH, post(node_handshake))
        .route(NODE_CONFIG_PATH, get(node_config))
        .route(NODE_REPORT_PATH, post(node_report))
        .route(
            "/api/admin/v1/nodes",
            get(admin_list_nodes).post(admin_create_node),
        )
        .route(
            "/api/admin/v1/nodes/:node_id",
            get(admin_get_node).patch(admin_update_node),
        )
        .route(
            "/api/admin/v1/nodes/:node_id/status",
            patch(admin_update_node_status),
        )
        .route(
            "/api/admin/v1/nodes/:node_id/bootstrap-token",
            post(admin_create_bootstrap_token),
        )
        .route(
            "/api/admin/v1/game-route-rules",
            get(admin_list_route_rules).post(admin_create_route_rule),
        )
        .route(
            "/api/admin/v1/game-route-rules/:rule_id",
            patch(admin_update_route_rule).delete(admin_delete_route_rule),
        )
        .with_state(Arc::new(state));

    let listener = TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("failed to bind control api {}", cli.listen))?;
    info!(version = VERSION, listen = %cli.listen, "xaccel-control-api listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("control api server stopped")?;
    Ok(())
}

async fn admin_dashboard() -> Html<&'static str> {
    Html(ADMIN_DASHBOARD_HTML)
}

async fn run_schema_migrations(pool: &MySqlPool) -> anyhow::Result<()> {
    ensure_game_route_game_name_column(pool).await?;
    Ok(())
}

async fn ensure_game_route_game_name_column(pool: &MySqlPool) -> anyhow::Result<()> {
    let exists = sqlx::query_scalar::<_, String>(
        r#"
SELECT COLUMN_NAME
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = DATABASE()
  AND TABLE_NAME = 'game_route_rules'
  AND COLUMN_NAME = 'game_name'
LIMIT 1
"#,
    )
    .fetch_optional(pool)
    .await
    .context("failed to inspect game_route_rules.game_name")?;

    if exists.is_none() {
        sqlx::query(
            r#"
ALTER TABLE game_route_rules
ADD COLUMN game_name VARCHAR(128) NOT NULL DEFAULT '' AFTER game_id
"#,
        )
        .execute(pool)
        .await
        .context("failed to add game_route_rules.game_name")?;
    }

    Ok(())
}

fn validate_cli(cli: &Cli) -> anyhow::Result<()> {
    if cli.database_url.trim().is_empty() {
        bail!("--database-url or DATABASE_URL is required");
    }
    if cli.max_db_connections == 0 {
        bail!("--max-db-connections must be positive");
    }
    if cli
        .admin_token
        .as_deref()
        .is_some_and(|token| token.trim().is_empty())
    {
        bail!("--admin-token must not be empty when provided");
    }
    if cli
        .public_base_url
        .as_deref()
        .is_some_and(|url| url.trim().is_empty())
    {
        bail!("--public-base-url must not be empty when provided");
    }
    if let Some(url) = cli.public_base_url.as_deref() {
        let url = url.trim();
        if url.chars().any(char::is_whitespace) {
            bail!("--public-base-url must not contain whitespace");
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("--public-base-url must start with http:// or https://");
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        error!(error = %err, "failed to install ctrl-c handler");
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    let database = match sqlx::query("SELECT 1").execute(&state.pool).await {
        Ok(_) => "ok",
        Err(error) => {
            error!(error = %error, "health database ping failed");
            "error"
        }
    };

    Json(HealthResponse {
        status: if database == "ok" {
            "ready"
        } else {
            "degraded"
        },
        version: VERSION,
        database,
    })
    .into_response()
}

async fn connect_intent(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ConnectIntentRequest>,
) -> Result<Json<ConnectIntentResponse>, AppError> {
    validate_connect_intent_request(&request)?;
    let response = issue_connect_intent(&state.pool, state.token_ttl_sec, request).await?;
    Ok(Json(response))
}

async fn node_bootstrap(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<BootstrapRequest>,
) -> Result<Json<BootstrapResponse>, AppError> {
    validate_bootstrap_request(&request)?;
    let panel_url = resolve_public_base_url(&state, &headers)?;
    let response = exchange_bootstrap_token(&state.pool, request, panel_url).await?;
    Ok(Json(response))
}

async fn node_handshake(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<NodeHandshakeResponse>, AppError> {
    let header_node_id = required_header_u64(&headers, "X-Node-Id")?;
    let timestamp = required_header_u64(&headers, "X-Node-Timestamp")?;
    let nonce = required_header(&headers, "X-Node-Nonce")?;
    let body_sha256 = required_header(&headers, "X-Node-Body-Sha256")?;
    let signature = required_header(&headers, "X-Node-Signature")?;

    validate_node_report_timestamp(timestamp)?;

    let node_secret = select_node_secret(&state.pool, header_node_id)
        .await?
        .ok_or_else(|| AppError::unauthorized("unknown_node", "node is not registered"))?;
    verify_node_signature(
        "POST",
        NODE_HANDSHAKE_PATH,
        &node_secret,
        timestamp,
        nonce,
        body_sha256,
        signature,
        &body,
    )?;

    let request = serde_json::from_slice::<NodeHandshakeRequest>(&body).map_err(|error| {
        AppError::bad_request(
            "invalid_handshake",
            format!("invalid handshake body: {error}"),
        )
    })?;
    validate_node_handshake_request(header_node_id, timestamp, nonce, &request)?;
    let config_revision = persist_node_handshake(&state.pool, &request).await?;

    Ok(Json(NodeHandshakeResponse {
        status: "ok",
        node_id: request.node_id,
        server_time: now_unix(),
        config_revision,
        min_node_version: MIN_NODE_VERSION,
        websocket: NodeHandshakeWebsocket {
            enabled: false,
            url: None,
        },
    }))
}

async fn node_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<NodeReportResponse>, AppError> {
    let header_node_id = required_header_u64(&headers, "X-Node-Id")?;
    let timestamp = required_header_u64(&headers, "X-Node-Timestamp")?;
    let nonce = required_header(&headers, "X-Node-Nonce")?;
    let body_sha256 = required_header(&headers, "X-Node-Body-Sha256")?;
    let signature = required_header(&headers, "X-Node-Signature")?;

    validate_node_report_timestamp(timestamp)?;

    let node_secret = select_node_secret(&state.pool, header_node_id)
        .await?
        .ok_or_else(|| AppError::unauthorized("unknown_node", "node is not registered"))?;
    verify_node_signature(
        "POST",
        NODE_REPORT_PATH,
        &node_secret,
        timestamp,
        nonce,
        body_sha256,
        signature,
        &body,
    )?;

    let raw_json = serde_json::from_slice::<Value>(&body).map_err(|error| {
        AppError::bad_request("invalid_report", format!("invalid report body: {error}"))
    })?;
    let report =
        serde_json::from_value::<NodeReportRequest>(raw_json.clone()).map_err(|error| {
            AppError::bad_request("invalid_report", format!("invalid report body: {error}"))
        })?;
    validate_node_report_request(header_node_id, &report)?;
    persist_node_report(&state.pool, &report, &raw_json).await?;

    Ok(Json(NodeReportResponse {
        status: "ok",
        node_id: report.node_id,
        stored: true,
        server_time: now_unix(),
    }))
}

async fn node_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<NodeConfigResponse>, AppError> {
    let node_id = required_header_u64(&headers, "X-Node-Id")?;
    let timestamp = required_header_u64(&headers, "X-Node-Timestamp")?;
    let nonce = required_header(&headers, "X-Node-Nonce")?;
    let body_sha256 = required_header(&headers, "X-Node-Body-Sha256")?;
    let signature = required_header(&headers, "X-Node-Signature")?;

    validate_node_report_timestamp(timestamp)?;

    let node_secret = select_node_secret(&state.pool, node_id)
        .await?
        .ok_or_else(|| AppError::unauthorized("unknown_node", "node is not registered"))?;
    verify_node_signature(
        "GET",
        NODE_CONFIG_PATH,
        &node_secret,
        timestamp,
        nonce,
        body_sha256,
        signature,
        b"",
    )?;

    let row = select_node_config(&state.pool, node_id)
        .await?
        .ok_or_else(|| AppError::not_found("node_not_found", "node does not exist"))?;
    Ok(Json(NodeConfigResponse::from_row(row)))
}

async fn admin_list_nodes(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AdminListNodesQuery>,
) -> Result<Json<AdminListNodesResponse>, AppError> {
    require_admin(&state, &headers)?;
    let limit = clamp_limit(query.limit, 200, 500);
    let rows = select_admin_nodes(&state.pool, limit).await?;
    let nodes = rows
        .into_iter()
        .map(AdminNodeSummary::from_row)
        .collect::<Vec<_>>();

    Ok(Json(AdminListNodesResponse {
        total: nodes.len(),
        nodes,
        server_time: now_unix(),
    }))
}

async fn admin_get_node(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Query(query): Query<AdminNodeDetailQuery>,
) -> Result<Json<AdminNodeDetailResponse>, AppError> {
    require_admin(&state, &headers)?;
    let node = select_admin_node(&state.pool, node_id)
        .await?
        .ok_or_else(|| AppError::not_found("node_not_found", "node does not exist"))?;
    let reports = select_admin_reports(&state.pool, node_id, clamp_limit(query.limit, 20, 100))
        .await?
        .into_iter()
        .map(AdminReportDetail::from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let audit_logs =
        select_admin_audit_logs(&state.pool, node_id, clamp_limit(query.limit, 20, 100))
            .await?
            .into_iter()
            .map(AdminAuditLogDetail::from_row)
            .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(AdminNodeDetailResponse {
        node: AdminNodeSummary::from_row(node),
        recent_reports: reports,
        recent_audit_logs: audit_logs,
        server_time: now_unix(),
    }))
}

async fn admin_create_node(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdminCreateNodeRequest>,
) -> Result<Json<AdminCreateNodeResponse>, AppError> {
    require_admin(&state, &headers)?;
    let node_id = insert_admin_node(&state.pool, request).await?;
    let node = select_admin_node(&state.pool, node_id)
        .await?
        .ok_or_else(|| AppError::not_found("node_not_found", "created node does not exist"))?;

    Ok(Json(AdminCreateNodeResponse {
        status: "ok",
        node: AdminNodeSummary::from_row(node),
        server_time: now_unix(),
    }))
}

async fn admin_update_node(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Json(request): Json<AdminUpdateNodeRequest>,
) -> Result<Json<AdminUpdateNodeResponse>, AppError> {
    require_admin(&state, &headers)?;
    update_admin_node_config(&state.pool, node_id, request).await?;
    let node = select_admin_node(&state.pool, node_id)
        .await?
        .ok_or_else(|| AppError::not_found("node_not_found", "updated node does not exist"))?;

    Ok(Json(AdminUpdateNodeResponse {
        status: "ok",
        node: AdminNodeSummary::from_row(node),
        server_time: now_unix(),
    }))
}

async fn admin_update_node_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Json(request): Json<AdminUpdateNodeStatusRequest>,
) -> Result<Json<AdminUpdateNodeStatusResponse>, AppError> {
    require_admin(&state, &headers)?;
    let next_status = validate_admin_node_status(&request.status)?;
    let reason = request
        .reason
        .map(|reason| reason.trim().to_string())
        .filter(|reason| !reason.is_empty());
    let previous_status =
        update_admin_node_status(&state.pool, node_id, next_status, reason.as_deref()).await?;

    Ok(Json(AdminUpdateNodeStatusResponse {
        status: "ok",
        node_id,
        previous_status,
        current_status: next_status.to_string(),
        reason,
        server_time: now_unix(),
    }))
}

async fn admin_create_bootstrap_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Json(request): Json<AdminCreateBootstrapTokenRequest>,
) -> Result<Json<AdminCreateBootstrapTokenResponse>, AppError> {
    require_admin(&state, &headers)?;
    let public_base_url = request
        .public_base_url
        .as_deref()
        .map(normalize_url_arg)
        .transpose()?
        .unwrap_or(resolve_public_base_url(&state, &headers)?);
    let install_url = request
        .install_url
        .as_deref()
        .map(normalize_url_arg)
        .transpose()?
        .unwrap_or_else(|| DEFAULT_INSTALL_URL.to_string());
    let channel = request
        .channel
        .as_deref()
        .map(normalize_command_arg)
        .transpose()?;
    let expires_in_sec = clamp_bootstrap_ttl(request.expires_in_sec)?;
    let expires_at = now_unix() + expires_in_sec;
    let bootstrap_token =
        create_bootstrap_token(&state.pool, node_id, request.created_by, expires_at).await?;
    let bootstrap_url = format!("{public_base_url}{NODE_BOOTSTRAP_PATH}");
    let install_command = build_bootstrap_install_command(
        &install_url,
        &bootstrap_url,
        &bootstrap_token,
        request.enable_control_plane.unwrap_or(true),
        channel.as_deref(),
    );

    Ok(Json(AdminCreateBootstrapTokenResponse {
        status: "ok",
        node_id,
        bootstrap_token,
        bootstrap_url,
        expires_at,
        install_command,
        server_time: now_unix(),
    }))
}

async fn admin_list_route_rules(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AdminListRouteRulesQuery>,
) -> Result<Json<AdminListRouteRulesResponse>, AppError> {
    require_admin(&state, &headers)?;
    validate_route_rule_query(&query)?;
    let limit = clamp_limit(query.limit, 200, 500);
    let rows = select_admin_route_rules(&state.pool, &query, limit).await?;
    let rules = rows
        .into_iter()
        .map(AdminRouteRuleSummary::from_row)
        .collect::<Vec<_>>();

    Ok(Json(AdminListRouteRulesResponse {
        total: rules.len(),
        rules,
        server_time: now_unix(),
    }))
}

async fn admin_create_route_rule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdminRouteRuleRequest>,
) -> Result<Json<AdminRouteRuleResponse>, AppError> {
    require_admin(&state, &headers)?;
    let rule_id = insert_admin_route_rule(&state.pool, request).await?;
    let rule = select_admin_route_rule(&state.pool, rule_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found("route_rule_not_found", "created rule does not exist")
        })?;

    Ok(Json(AdminRouteRuleResponse {
        status: "ok",
        rule: AdminRouteRuleSummary::from_row(rule),
        server_time: now_unix(),
    }))
}

async fn admin_update_route_rule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(rule_id): Path<u64>,
    Json(request): Json<AdminUpdateRouteRuleRequest>,
) -> Result<Json<AdminRouteRuleResponse>, AppError> {
    require_admin(&state, &headers)?;
    update_admin_route_rule(&state.pool, rule_id, request).await?;
    let rule = select_admin_route_rule(&state.pool, rule_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found("route_rule_not_found", "updated rule does not exist")
        })?;

    Ok(Json(AdminRouteRuleResponse {
        status: "ok",
        rule: AdminRouteRuleSummary::from_row(rule),
        server_time: now_unix(),
    }))
}

async fn admin_delete_route_rule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(rule_id): Path<u64>,
) -> Result<Json<AdminDeleteRouteRuleResponse>, AppError> {
    require_admin(&state, &headers)?;
    if rule_id == 0 {
        return Err(AppError::bad_request(
            "invalid_route_rule",
            "rule_id must be positive",
        ));
    }
    let deleted = delete_admin_route_rule(&state.pool, rule_id).await?;

    Ok(Json(AdminDeleteRouteRuleResponse {
        status: "ok",
        rule_id,
        deleted,
        server_time: now_unix(),
    }))
}

async fn issue_connect_intent(
    pool: &MySqlPool,
    ttl_sec: u64,
    request: ConnectIntentRequest,
) -> Result<ConnectIntentResponse, AppError> {
    let requested_quality = request
        .bandwidth_quality
        .clone()
        .unwrap_or_else(|| "normal".to_string());
    let row = select_candidate(pool, &request, &requested_quality)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "no_candidate", "no available node"))?;

    let issued_at = now_unix();
    let expires_at = issued_at + ttl_sec;
    let intent_id = format!(
        "intent-{}-{}-{}-{}",
        request.user_id, request.game_id, issued_at, row.node_id
    );
    let route = ClientRouteClaims {
        target_addr: row.target_addr.clone(),
        protocol: row.protocol.clone(),
    };
    let claims = ClientTokenClaims {
        node_id: row.node_id,
        user_id: request.user_id,
        device_id: request.device_id.clone(),
        game_id: request.game_id,
        intent_id: Some(intent_id.clone()),
        route: Some(route.clone()),
        expires_at,
        issued_at: Some(issued_at),
        nonce: Some(format!(
            "{}-{}-{}",
            issued_at, row.node_id, request.device_id
        )),
    };
    let token = sign_client_token(&claims, &row.node_secret).map_err(AppError::internal)?;

    insert_connect_intent(pool, &intent_id, &request, &row, expires_at).await?;

    let client = ClientContext {
        platform: request.platform,
        client_isp: request.client_isp,
        client_ip: request.client_ip,
        bandwidth_quality: requested_quality,
    };

    Ok(ConnectIntentResponse {
        intent_id: intent_id.clone(),
        ttl_sec,
        client,
        candidates: vec![NodeCandidate {
            node_id: row.node_id,
            area: row.area,
            tag: row.tag.unwrap_or_else(|| "default".to_string()),
            host: row.server_ip,
            port: u16::try_from(row.server_port).map_err(|_| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "invalid_node_port",
                    "selected node port is outside 1-65535",
                )
            })?,
            transports: vec!["udp"],
            bandwidth_quality: row.bandwidth_quality,
            probe: ProbeInfo {
                udp: true,
                tcp: true,
                protocol: "xaccel/1",
            },
            route: route.clone(),
            credential: CredentialInfo {
                token,
                expires_at,
                intent_id,
                route,
            },
        }],
    })
}

async fn select_candidate(
    pool: &MySqlPool,
    request: &ConnectIntentRequest,
    requested_quality: &str,
) -> Result<Option<CandidateRow>, AppError> {
    sqlx::query_as::<_, CandidateRow>(
        r#"
SELECT
  n.id AS node_id,
  n.server_ip,
  n.server_port,
  n.area,
  n.tag,
  n.bandwidth_quality,
  n.node_secret,
  r.target_addr,
  r.protocol
FROM game_route_rules r
JOIN accel_nodes n ON n.id = r.node_id
WHERE r.game_id = ?
  AND r.status = 'enabled'
  AND r.protocol = 'udp'
  AND n.status = 'online'
  AND n.disable_quic = 0
  AND n.node_secret IS NOT NULL
  AND n.node_secret <> ''
ORDER BY
  CASE WHEN n.bandwidth_quality = ? THEN 0 ELSE 1 END,
  r.priority ASC,
  n.last_seen_at DESC,
  n.id ASC
LIMIT 1
"#,
    )
    .bind(request.game_id)
    .bind(requested_quality)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)
}

async fn insert_connect_intent(
    pool: &MySqlPool,
    intent_id: &str,
    request: &ConnectIntentRequest,
    row: &CandidateRow,
    expires_at: u64,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
INSERT INTO connect_intents (
  intent_id,
  user_id,
  device_id,
  game_id,
  node_id,
  target_addr,
  protocol,
  client_ip,
  client_isp,
  platform,
  bandwidth_quality,
  expires_at,
  created_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, FROM_UNIXTIME(?), CURRENT_TIMESTAMP)
"#,
    )
    .bind(intent_id)
    .bind(request.user_id)
    .bind(&request.device_id)
    .bind(request.game_id)
    .bind(row.node_id)
    .bind(&row.target_addr)
    .bind(&row.protocol)
    .bind(&request.client_ip)
    .bind(&request.client_isp)
    .bind(&request.platform)
    .bind(
        request
            .bandwidth_quality
            .as_deref()
            .unwrap_or(row.bandwidth_quality.as_str()),
    )
    .bind(expires_at)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    Ok(())
}

async fn create_bootstrap_token(
    pool: &MySqlPool,
    node_id: u64,
    created_by: Option<u64>,
    expires_at: u64,
) -> Result<String, AppError> {
    if node_id == 0 {
        return Err(AppError::bad_request(
            "invalid_node",
            "node_id must be positive",
        ));
    }
    if created_by == Some(0) {
        return Err(AppError::bad_request(
            "invalid_actor",
            "created_by must be positive when provided",
        ));
    }

    let node_exists = sqlx::query_scalar::<_, u64>(
        r#"
SELECT id
FROM accel_nodes
WHERE id = ?
LIMIT 1
"#,
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)?
    .is_some();

    if !node_exists {
        return Err(AppError::not_found("node_not_found", "node does not exist"));
    }

    let token = generate_bootstrap_token();
    let token_hash = hash_bootstrap_token(&token);
    sqlx::query(
        r#"
INSERT INTO node_bootstrap_tokens (
  node_id,
  token_hash,
  expires_at,
  created_by,
  created_at
) VALUES (?, ?, FROM_UNIXTIME(?), ?, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(token_hash)
    .bind(expires_at)
    .bind(created_by)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    Ok(token)
}

async fn exchange_bootstrap_token(
    pool: &MySqlPool,
    request: BootstrapRequest,
    panel_url: String,
) -> Result<BootstrapResponse, AppError> {
    let token_hash = hash_bootstrap_token(&request.bootstrap_token);
    let mut tx = pool.begin().await.map_err(AppError::database)?;
    let row = sqlx::query_as::<_, BootstrapExchangeRow>(
        r#"
SELECT
  bt.id AS token_id,
  bt.node_id,
  CAST(UNIX_TIMESTAMP(bt.expires_at) AS UNSIGNED) AS expires_at,
  CAST(UNIX_TIMESTAMP(bt.used_at) AS UNSIGNED) AS used_at,
  n.node_secret,
  n.server_ip,
  n.server_port,
  n.config_revision
FROM node_bootstrap_tokens bt
JOIN accel_nodes n ON n.id = bt.node_id
WHERE bt.token_hash = ?
LIMIT 1
FOR UPDATE
"#,
    )
    .bind(token_hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::database)?
    .ok_or_else(|| {
        AppError::bad_request("invalid_bootstrap_token", "bootstrap token is invalid")
    })?;

    if row.used_at.is_some() {
        return Err(AppError::bad_request(
            "bootstrap_token_used",
            "bootstrap token was already used",
        ));
    }
    if row.expires_at <= now_unix() {
        return Err(AppError::bad_request(
            "bootstrap_token_expired",
            "bootstrap token is expired",
        ));
    }

    let node_secret = row
        .node_secret
        .filter(|secret| !secret.trim().is_empty())
        .unwrap_or_else(generate_node_secret);
    let config_revision = row.config_revision.max(1);
    let used_by_ip = request.ips.first().map(String::as_str);

    sqlx::query(
        r#"
UPDATE node_bootstrap_tokens
SET used_at = CURRENT_TIMESTAMP,
    used_by_ip = ?
WHERE id = ?
"#,
    )
    .bind(used_by_ip)
    .bind(row.token_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    sqlx::query(
        r#"
UPDATE accel_nodes
SET
  status = 'installing',
  node_secret = ?,
  config_revision = ?,
  installed_at = CURRENT_TIMESTAMP,
  install_error_code = NULL,
  install_error_message = NULL
WHERE id = ?
"#,
    )
    .bind(&node_secret)
    .bind(config_revision)
    .bind(row.node_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    tx.commit().await.map_err(AppError::database)?;

    Ok(BootstrapResponse {
        node_id: row.node_id,
        node_secret,
        panel_url,
        server_ip: row.server_ip,
        server_port: row.server_port,
        config_revision,
        release: BootstrapReleaseInfo {
            version: VERSION,
            manifest_url: String::new(),
        },
    })
}

async fn select_admin_nodes(pool: &MySqlPool, limit: u32) -> Result<Vec<AdminNodeRow>, AppError> {
    sqlx::query_as::<_, AdminNodeRow>(
        r#"
SELECT
  n.id,
  n.name,
  n.server_ip,
  n.server_port,
  n.relay_server_ip,
  n.relay_server_port,
  n.is_support_ipv6,
  n.area,
  n.tag,
  n.bandwidth_quality,
  n.disable_quic,
  n.telecom_ip,
  n.mobile_ip,
  n.unicom_ip,
  n.status,
  n.kernel_version,
  n.config_revision,
  CAST(UNIX_TIMESTAMP(n.last_seen_at) AS UNSIGNED) AS last_seen_at,
  CAST(UNIX_TIMESTAMP(n.last_report_at) AS UNSIGNED) AS last_report_at,
  lr.id AS latest_report_id,
  lr.status AS latest_report_status,
  lr.active_sessions AS latest_active_sessions,
  lr.udp_sessions AS latest_udp_sessions,
  lr.tcp_sessions AS latest_tcp_sessions,
  CAST(UNIX_TIMESTAMP(lr.reported_at) AS UNSIGNED) AS latest_reported_at
FROM accel_nodes n
LEFT JOIN node_runtime_reports lr
  ON lr.id = (
    SELECT r.id
    FROM node_runtime_reports r
    WHERE r.node_id = n.id
    ORDER BY r.id DESC
    LIMIT 1
  )
ORDER BY
  n.last_report_at IS NULL ASC,
  n.last_report_at DESC,
  n.id ASC
LIMIT ?
"#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::database)
}

async fn select_admin_node(
    pool: &MySqlPool,
    node_id: u64,
) -> Result<Option<AdminNodeRow>, AppError> {
    sqlx::query_as::<_, AdminNodeRow>(
        r#"
SELECT
  n.id,
  n.name,
  n.server_ip,
  n.server_port,
  n.relay_server_ip,
  n.relay_server_port,
  n.is_support_ipv6,
  n.area,
  n.tag,
  n.bandwidth_quality,
  n.disable_quic,
  n.telecom_ip,
  n.mobile_ip,
  n.unicom_ip,
  n.status,
  n.kernel_version,
  n.config_revision,
  CAST(UNIX_TIMESTAMP(n.last_seen_at) AS UNSIGNED) AS last_seen_at,
  CAST(UNIX_TIMESTAMP(n.last_report_at) AS UNSIGNED) AS last_report_at,
  lr.id AS latest_report_id,
  lr.status AS latest_report_status,
  lr.active_sessions AS latest_active_sessions,
  lr.udp_sessions AS latest_udp_sessions,
  lr.tcp_sessions AS latest_tcp_sessions,
  CAST(UNIX_TIMESTAMP(lr.reported_at) AS UNSIGNED) AS latest_reported_at
FROM accel_nodes n
LEFT JOIN node_runtime_reports lr
  ON lr.id = (
    SELECT r.id
    FROM node_runtime_reports r
    WHERE r.node_id = n.id
    ORDER BY r.id DESC
    LIMIT 1
  )
WHERE n.id = ?
LIMIT 1
"#,
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)
}

async fn select_admin_reports(
    pool: &MySqlPool,
    node_id: u64,
    limit: u32,
) -> Result<Vec<AdminReportRow>, AppError> {
    sqlx::query_as::<_, AdminReportRow>(
        r#"
SELECT
  id,
  node_id,
  config_revision,
  status,
  active_sessions,
  udp_sessions,
  tcp_sessions,
  CAST(UNIX_TIMESTAMP(reported_at) AS UNSIGNED) AS reported_at,
  CAST(raw_json AS CHAR) AS raw_json
FROM node_runtime_reports
WHERE node_id = ?
ORDER BY id DESC
LIMIT ?
"#,
    )
    .bind(node_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::database)
}

async fn select_admin_audit_logs(
    pool: &MySqlPool,
    node_id: u64,
    limit: u32,
) -> Result<Vec<AdminAuditLogRow>, AppError> {
    sqlx::query_as::<_, AdminAuditLogRow>(
        r#"
SELECT
  id,
  node_id,
  actor_type,
  actor_id,
  action,
  CAST(UNIX_TIMESTAMP(created_at) AS UNSIGNED) AS created_at,
  CAST(detail_json AS CHAR) AS detail_json
FROM node_audit_logs
WHERE node_id = ?
ORDER BY id DESC
LIMIT ?
"#,
    )
    .bind(node_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::database)
}

async fn select_admin_route_rules(
    pool: &MySqlPool,
    query: &AdminListRouteRulesQuery,
    limit: u32,
) -> Result<Vec<AdminRouteRuleRow>, AppError> {
    let mut builder = QueryBuilder::<MySql>::new(
        r#"
SELECT
  r.id,
  r.game_id,
  COALESCE(NULLIF(r.game_name, ''), CONCAT('游戏 ', r.game_id)) AS game_name,
  r.node_id,
  n.name AS node_name,
  n.server_ip AS node_server_ip,
  n.server_port AS node_server_port,
  n.status AS node_status,
  r.target_addr,
  r.protocol,
  r.area,
  r.tag,
  r.priority,
  r.status,
  CAST(UNIX_TIMESTAMP(r.created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(r.updated_at) AS UNSIGNED) AS updated_at
FROM game_route_rules r
JOIN accel_nodes n ON n.id = r.node_id
WHERE 1 = 1
"#,
    );

    if let Some(game_id) = query.game_id {
        builder.push(" AND r.game_id = ");
        builder.push_bind(game_id);
    }
    if let Some(node_id) = query.node_id {
        builder.push(" AND r.node_id = ");
        builder.push_bind(node_id);
    }
    if let Some(status) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|status| !status.is_empty())
    {
        builder.push(" AND r.status = ");
        builder.push_bind(status);
    }

    builder.push(
        r#"
ORDER BY r.game_id ASC, r.priority ASC, r.id ASC
LIMIT
"#,
    );
    builder.push_bind(limit);

    builder
        .build_query_as::<AdminRouteRuleRow>()
        .fetch_all(pool)
        .await
        .map_err(AppError::database)
}

async fn select_admin_route_rule(
    pool: &MySqlPool,
    rule_id: u64,
) -> Result<Option<AdminRouteRuleRow>, AppError> {
    sqlx::query_as::<_, AdminRouteRuleRow>(
        r#"
SELECT
  r.id,
  r.game_id,
  COALESCE(NULLIF(r.game_name, ''), CONCAT('游戏 ', r.game_id)) AS game_name,
  r.node_id,
  n.name AS node_name,
  n.server_ip AS node_server_ip,
  n.server_port AS node_server_port,
  n.status AS node_status,
  r.target_addr,
  r.protocol,
  r.area,
  r.tag,
  r.priority,
  r.status,
  CAST(UNIX_TIMESTAMP(r.created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(r.updated_at) AS UNSIGNED) AS updated_at
FROM game_route_rules r
JOIN accel_nodes n ON n.id = r.node_id
WHERE r.id = ?
LIMIT 1
"#,
    )
    .bind(rule_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)
}

async fn insert_admin_route_rule(
    pool: &MySqlPool,
    request: AdminRouteRuleRequest,
) -> Result<u64, AppError> {
    let rule = normalize_route_rule_request(&request)?;
    ensure_admin_node_exists(pool, rule.node_id).await?;
    let result = sqlx::query(
        r#"
INSERT INTO game_route_rules (
  game_id,
  game_name,
  node_id,
  target_addr,
  protocol,
  area,
  tag,
  priority,
  status,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
"#,
    )
    .bind(rule.game_id)
    .bind(&rule.game_name)
    .bind(rule.node_id)
    .bind(&rule.target_addr)
    .bind(&rule.protocol)
    .bind(&rule.area)
    .bind(&rule.tag)
    .bind(rule.priority)
    .bind(&rule.status)
    .execute(pool)
    .await
    .map_err(map_route_rule_write_error)?;

    Ok(result.last_insert_id())
}

async fn update_admin_route_rule(
    pool: &MySqlPool,
    rule_id: u64,
    request: AdminUpdateRouteRuleRequest,
) -> Result<(), AppError> {
    if rule_id == 0 {
        return Err(AppError::bad_request(
            "invalid_route_rule",
            "rule_id must be positive",
        ));
    }
    let rule = normalize_route_rule_request(&request)?;
    ensure_admin_node_exists(pool, rule.node_id).await?;
    let result = sqlx::query(
        r#"
UPDATE game_route_rules
SET
  game_id = ?,
  game_name = ?,
  node_id = ?,
  target_addr = ?,
  protocol = ?,
  area = ?,
  tag = ?,
  priority = ?,
  status = ?,
  updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
    )
    .bind(rule.game_id)
    .bind(&rule.game_name)
    .bind(rule.node_id)
    .bind(&rule.target_addr)
    .bind(&rule.protocol)
    .bind(&rule.area)
    .bind(&rule.tag)
    .bind(rule.priority)
    .bind(&rule.status)
    .bind(rule_id)
    .execute(pool)
    .await
    .map_err(map_route_rule_write_error)?;

    if result.rows_affected() == 0 {
        return Err(AppError::not_found(
            "route_rule_not_found",
            "route rule does not exist",
        ));
    }
    Ok(())
}

async fn delete_admin_route_rule(pool: &MySqlPool, rule_id: u64) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
DELETE FROM game_route_rules
WHERE id = ?
"#,
    )
    .bind(rule_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    Ok(result.rows_affected() > 0)
}

async fn ensure_admin_node_exists(pool: &MySqlPool, node_id: u64) -> Result<(), AppError> {
    if node_id == 0 {
        return Err(AppError::bad_request(
            "invalid_node",
            "node_id must be positive",
        ));
    }
    let exists = sqlx::query_scalar::<_, u64>(
        r#"
SELECT id
FROM accel_nodes
WHERE id = ?
LIMIT 1
"#,
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)?
    .is_some();

    if exists {
        Ok(())
    } else {
        Err(AppError::not_found("node_not_found", "node does not exist"))
    }
}

async fn insert_admin_node(
    pool: &MySqlPool,
    request: AdminCreateNodeRequest,
) -> Result<u64, AppError> {
    let node = normalize_create_node_request(&request)?;
    let result = sqlx::query(
        r#"
INSERT INTO accel_nodes (
  name,
  server_ip,
  server_port,
  relay_server_ip,
  relay_server_port,
  is_support_ipv6,
  bandwidth_quality,
  disable_quic,
  area,
  telecom_ip,
  mobile_ip,
  unicom_ip,
  tag,
  status,
  config_revision,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending_install', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
"#,
    )
    .bind(&node.name)
    .bind(&node.server_ip)
    .bind(node.server_port)
    .bind(&node.relay_server_ip)
    .bind(node.relay_server_port)
    .bind(node.is_support_ipv6)
    .bind(&node.bandwidth_quality)
    .bind(node.disable_quic)
    .bind(&node.area)
    .bind(&node.telecom_ip)
    .bind(&node.mobile_ip)
    .bind(&node.unicom_ip)
    .bind(&node.tag)
    .execute(pool)
    .await;

    match result {
        Ok(result) => Ok(result.last_insert_id()),
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("1062") => {
            Err(AppError::conflict(
                "node_endpoint_exists",
                "a node with the same server_ip and server_port already exists",
            ))
        }
        Err(error) => Err(AppError::database(error)),
    }
}

async fn update_admin_node_config(
    pool: &MySqlPool,
    node_id: u64,
    request: AdminUpdateNodeRequest,
) -> Result<(), AppError> {
    let node = normalize_create_node_request(&request)?;
    let mut tx = pool.begin().await.map_err(AppError::database)?;
    let result = sqlx::query(
        r#"
UPDATE accel_nodes
SET
  name = ?,
  server_ip = ?,
  server_port = ?,
  relay_server_ip = ?,
  relay_server_port = ?,
  is_support_ipv6 = ?,
  bandwidth_quality = ?,
  disable_quic = ?,
  area = ?,
  telecom_ip = ?,
  mobile_ip = ?,
  unicom_ip = ?,
  tag = ?,
  config_revision = config_revision + 1,
  updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
    )
    .bind(&node.name)
    .bind(&node.server_ip)
    .bind(node.server_port)
    .bind(&node.relay_server_ip)
    .bind(node.relay_server_port)
    .bind(node.is_support_ipv6)
    .bind(&node.bandwidth_quality)
    .bind(node.disable_quic)
    .bind(&node.area)
    .bind(&node.telecom_ip)
    .bind(&node.mobile_ip)
    .bind(&node.unicom_ip)
    .bind(&node.tag)
    .bind(node_id)
    .execute(&mut *tx)
    .await;

    let result = match result {
        Ok(result) => result,
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("1062") => {
            return Err(AppError::conflict(
                "node_endpoint_exists",
                "a node with the same server_ip and server_port already exists",
            ));
        }
        Err(error) => return Err(AppError::database(error)),
    };

    if result.rows_affected() == 0 {
        return Err(AppError::not_found("node_not_found", "node does not exist"));
    }

    let revision = sqlx::query_scalar::<_, u64>(
        r#"
SELECT config_revision
FROM accel_nodes
WHERE id = ?
LIMIT 1
"#,
    )
    .bind(node_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::database)?;
    let config_json = serde_json::json!({
        "network": {
            "server_ip": node.server_ip,
            "listen_ip": "0.0.0.0",
            "server_port": node.server_port,
            "relay_server_ip": node.relay_server_ip,
            "relay_server_port": node.relay_server_port,
            "is_support_ipv6": node.is_support_ipv6 != 0,
            "disable_quic": node.disable_quic != 0,
            "area": node.area,
            "bandwidth_quality": node.bandwidth_quality,
            "tag": node.tag,
            "operator_ips": {
                "telecom_ip": node.telecom_ip,
                "mobile_ip": node.mobile_ip,
                "unicom_ip": node.unicom_ip
            }
        }
    });
    sqlx::query(
        r#"
INSERT INTO node_config_revisions (
  node_id,
  revision,
  config_json,
  created_at
) VALUES (?, ?, ?, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(revision)
    .bind(config_json.to_string())
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    tx.commit().await.map_err(AppError::database)?;
    Ok(())
}

async fn update_admin_node_status(
    pool: &MySqlPool,
    node_id: u64,
    next_status: &str,
    reason: Option<&str>,
) -> Result<String, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::database)?;
    let previous_status = sqlx::query_scalar::<_, String>(
        r#"
SELECT status
FROM accel_nodes
WHERE id = ?
LIMIT 1
"#,
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::database)?
    .ok_or_else(|| AppError::not_found("node_not_found", "node does not exist"))?;

    sqlx::query(
        r#"
UPDATE accel_nodes
SET status = ?
WHERE id = ?
"#,
    )
    .bind(next_status)
    .bind(node_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    let response_previous_status = previous_status.clone();
    let detail_json = serde_json::json!({
        "previous_status": previous_status,
        "current_status": next_status,
        "reason": reason,
    });
    sqlx::query(
        r#"
INSERT INTO node_audit_logs (
  node_id,
  actor_type,
  actor_id,
  action,
  detail_json,
  created_at
) VALUES (?, 'admin_api', NULL, 'node.status.update', ?, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(detail_json.to_string())
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    tx.commit().await.map_err(AppError::database)?;
    Ok(response_previous_status)
}

async fn select_node_secret(pool: &MySqlPool, node_id: u64) -> Result<Option<String>, AppError> {
    sqlx::query_scalar::<_, String>(
        r#"
SELECT node_secret
FROM accel_nodes
WHERE id = ?
  AND node_secret IS NOT NULL
  AND node_secret <> ''
LIMIT 1
"#,
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)
}

async fn select_node_config(
    pool: &MySqlPool,
    node_id: u64,
) -> Result<Option<NodeConfigRow>, AppError> {
    sqlx::query_as::<_, NodeConfigRow>(
        r#"
SELECT
  id,
  server_ip,
  server_port,
  relay_server_ip,
  relay_server_port,
  is_support_ipv6,
  bandwidth_quality,
  disable_quic,
  area,
  telecom_ip,
  mobile_ip,
  unicom_ip,
  tag,
  config_revision
FROM accel_nodes
WHERE id = ?
LIMIT 1
"#,
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)
}

async fn persist_node_report(
    pool: &MySqlPool,
    report: &NodeReportRequest,
    raw_json: &Value,
) -> Result<(), AppError> {
    let raw_json = serde_json::to_string(raw_json).map_err(|error| {
        AppError::internal(anyhow::anyhow!(
            "failed to encode node report json: {error}"
        ))
    })?;
    let udp_sessions = clamp_u32(report.health.sessions.active_udp_sessions);
    let tcp_sessions = clamp_u32(report.health.sessions.active_tcp_connections);
    let active_sessions = udp_sessions.saturating_add(tcp_sessions);
    let db_status = report_database_status(report);
    let reported_at = report.timestamp.max(1);

    let mut tx = pool.begin().await.map_err(AppError::database)?;

    sqlx::query(
        r#"
INSERT INTO node_runtime_reports (
  node_id,
  config_revision,
  status,
  active_sessions,
  udp_sessions,
  tcp_sessions,
  raw_json,
  reported_at
) VALUES (?, ?, ?, ?, ?, ?, ?, FROM_UNIXTIME(?))
"#,
    )
    .bind(report.node_id)
    .bind(report.config_revision)
    .bind(&report.status)
    .bind(active_sessions)
    .bind(udp_sessions)
    .bind(tcp_sessions)
    .bind(&raw_json)
    .bind(reported_at)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    sqlx::query(
        r#"
UPDATE accel_nodes
SET
  status = ?,
  kernel_version = ?,
  config_revision = GREATEST(config_revision, ?),
  last_seen_at = FROM_UNIXTIME(?),
  last_report_at = CURRENT_TIMESTAMP
WHERE id = ?
  AND status NOT IN ('disabled', 'draining')
"#,
    )
    .bind(db_status)
    .bind(&report.node_version)
    .bind(report.config_revision)
    .bind(reported_at)
    .bind(report.node_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    tx.commit().await.map_err(AppError::database)?;
    Ok(())
}

async fn persist_node_handshake(
    pool: &MySqlPool,
    handshake: &NodeHandshakeRequest,
) -> Result<u64, AppError> {
    let seen_at = handshake.timestamp.max(1);
    sqlx::query(
        r#"
UPDATE accel_nodes
SET
  status = CASE
    WHEN status IN ('disabled', 'draining') THEN status
    ELSE 'online'
  END,
  kernel_version = ?,
  config_revision = GREATEST(config_revision, ?),
  last_seen_at = FROM_UNIXTIME(?),
  updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
    )
    .bind(&handshake.node_version)
    .bind(handshake.config_revision.max(1))
    .bind(seen_at)
    .bind(handshake.node_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    sqlx::query_scalar::<_, u64>(
        r#"
SELECT config_revision
FROM accel_nodes
WHERE id = ?
LIMIT 1
"#,
    )
    .bind(handshake.node_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)?
    .ok_or_else(|| AppError::not_found("node_not_found", "node does not exist"))
}

fn validate_connect_intent_request(request: &ConnectIntentRequest) -> Result<(), AppError> {
    if request.user_id == 0 {
        return Err(AppError::bad_request(
            "invalid_user",
            "user_id must be positive",
        ));
    }
    if request.device_id.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_device",
            "device_id is required",
        ));
    }
    if request.game_id == 0 {
        return Err(AppError::bad_request(
            "invalid_game",
            "game_id must be positive",
        ));
    }
    if let Some(quality) = request.bandwidth_quality.as_deref() {
        if !matches!(quality, "fast" | "normal" | "slow") {
            return Err(AppError::bad_request(
                "invalid_quality",
                "bandwidth_quality must be fast, normal, or slow",
            ));
        }
    }
    Ok(())
}

fn validate_bootstrap_request(request: &BootstrapRequest) -> Result<(), AppError> {
    if request.bootstrap_token.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_bootstrap_token",
            "bootstrap_token is required",
        ));
    }
    if request.hostname.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_hostname",
            "hostname is required",
        ));
    }
    if request.os != "linux" {
        return Err(AppError::bad_request(
            "invalid_os",
            "only linux bootstrap is supported",
        ));
    }
    if !matches!(request.arch.as_str(), "x86_64" | "aarch64") {
        return Err(AppError::bad_request(
            "invalid_arch",
            "arch must be x86_64 or aarch64",
        ));
    }
    if request.kernel.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_kernel",
            "kernel is required",
        ));
    }
    if request.installer_version.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_installer_version",
            "installer_version is required",
        ));
    }
    if request.ips.is_empty() {
        return Err(AppError::bad_request(
            "invalid_ips",
            "at least one host IP is required",
        ));
    }
    Ok(())
}

fn normalize_create_node_request(
    request: &AdminCreateNodeRequest,
) -> Result<NormalizedCreateNode, AppError> {
    let name = normalize_required_text(&request.name, "name", 128)?;
    let server_ip = normalize_ip_text(&request.server_ip, "server_ip")?;
    let server_port = validate_port(request.server_port, "server_port")?;
    let relay_server_ip =
        normalize_optional_ip_text(request.relay_server_ip.as_deref(), "relay_server_ip")?;
    let relay_server_port = match request.relay_server_port {
        Some(port) => Some(validate_port(port, "relay_server_port")?),
        None => None,
    };
    if relay_server_ip.is_none() && relay_server_port.is_some() {
        return Err(AppError::bad_request(
            "invalid_relay",
            "relay_server_ip is required when relay_server_port is provided",
        ));
    }

    let bandwidth_quality = request
        .bandwidth_quality
        .as_deref()
        .map(str::trim)
        .filter(|quality| !quality.is_empty())
        .unwrap_or("normal");
    if !matches!(bandwidth_quality, "fast" | "normal" | "slow") {
        return Err(AppError::bad_request(
            "invalid_quality",
            "bandwidth_quality must be fast, normal, or slow",
        ));
    }

    Ok(NormalizedCreateNode {
        name,
        server_ip,
        server_port,
        relay_server_ip,
        relay_server_port,
        is_support_ipv6: bool_i8(request.is_support_ipv6.unwrap_or(false)),
        bandwidth_quality: bandwidth_quality.to_string(),
        disable_quic: bool_i8(request.disable_quic.unwrap_or(false)),
        area: normalize_optional_text(request.area.as_deref(), 32)?
            .unwrap_or_else(|| "UNKNOWN".to_string()),
        telecom_ip: normalize_optional_ip_text(request.telecom_ip.as_deref(), "telecom_ip")?,
        mobile_ip: normalize_optional_ip_text(request.mobile_ip.as_deref(), "mobile_ip")?,
        unicom_ip: normalize_optional_ip_text(request.unicom_ip.as_deref(), "unicom_ip")?,
        tag: normalize_optional_text(request.tag.as_deref(), 64)?,
    })
}

fn validate_route_rule_query(query: &AdminListRouteRulesQuery) -> Result<(), AppError> {
    if query.game_id == Some(0) {
        return Err(AppError::bad_request(
            "invalid_game",
            "game_id must be positive",
        ));
    }
    if query.node_id == Some(0) {
        return Err(AppError::bad_request(
            "invalid_node",
            "node_id must be positive",
        ));
    }
    if let Some(status) = query.status.as_deref() {
        validate_route_rule_status(status)?;
    }
    Ok(())
}

fn normalize_route_rule_request(
    request: &AdminRouteRuleRequest,
) -> Result<NormalizedRouteRule, AppError> {
    if request.game_id == 0 {
        return Err(AppError::bad_request(
            "invalid_game",
            "game_id must be positive",
        ));
    }
    if request.node_id == 0 {
        return Err(AppError::bad_request(
            "invalid_node",
            "node_id must be positive",
        ));
    }

    let protocol = request
        .protocol
        .as_deref()
        .map(str::trim)
        .filter(|protocol| !protocol.is_empty())
        .unwrap_or("udp");
    if protocol != "udp" {
        return Err(AppError::bad_request(
            "invalid_route_protocol",
            "protocol must be udp",
        ));
    }

    Ok(NormalizedRouteRule {
        game_id: request.game_id,
        game_name: normalize_required_text(&request.game_name, "game_name", 128)?,
        node_id: request.node_id,
        target_addr: validate_target_addr(&request.target_addr)?,
        protocol: protocol.to_string(),
        area: normalize_optional_text(request.area.as_deref(), 32)?,
        tag: normalize_optional_text(request.tag.as_deref(), 64)?,
        priority: request.priority.unwrap_or(100),
        status: validate_route_rule_status(request.status.as_deref().unwrap_or("enabled"))?
            .to_string(),
    })
}

fn validate_target_addr(value: &str) -> Result<String, AppError> {
    let value = normalize_required_text(value, "target_addr", 255)?;
    let (host, port) = value.rsplit_once(':').ok_or_else(|| {
        AppError::bad_request(
            "invalid_target_addr",
            "target_addr must use host:port format",
        )
    })?;
    if host.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_target_addr",
            "target host is required",
        ));
    }
    let port = port
        .parse::<u32>()
        .map_err(|_| AppError::bad_request("invalid_target_addr", "target port must be numeric"))?;
    validate_port(port, "target_port")?;
    Ok(value)
}

fn validate_route_rule_status(status: &str) -> Result<&'static str, AppError> {
    match status.trim() {
        "enabled" => Ok("enabled"),
        "disabled" => Ok("disabled"),
        _ => Err(AppError::bad_request(
            "invalid_route_status",
            "status must be enabled or disabled",
        )),
    }
}

fn validate_admin_node_status(status: &str) -> Result<&'static str, AppError> {
    match status.trim() {
        "pending_install" => Ok("pending_install"),
        "draining" => Ok("draining"),
        "offline" => Ok("offline"),
        "install_failed" => Ok("install_failed"),
        "disabled" => Ok("disabled"),
        _ => Err(AppError::bad_request(
            "invalid_node_status",
            "status must be pending_install, draining, offline, install_failed, or disabled; online/degraded are set by signed node reports",
        )),
    }
}

fn validate_node_handshake_request(
    header_node_id: u64,
    header_timestamp: u64,
    header_nonce: &str,
    request: &NodeHandshakeRequest,
) -> Result<(), AppError> {
    if request.node_id == 0 {
        return Err(AppError::bad_request(
            "invalid_node",
            "handshake node_id must be positive",
        ));
    }
    if request.node_id != header_node_id {
        return Err(AppError::bad_request(
            "node_id_mismatch",
            "header node id does not match handshake body",
        ));
    }
    if request.timestamp != header_timestamp {
        return Err(AppError::bad_request(
            "timestamp_mismatch",
            "header timestamp does not match handshake body",
        ));
    }
    if request.nonce != header_nonce {
        return Err(AppError::bad_request(
            "nonce_mismatch",
            "header nonce does not match handshake body",
        ));
    }
    if request.node_version.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_node_version",
            "node_version is required",
        ));
    }
    if request.os != "linux" {
        return Err(AppError::bad_request(
            "invalid_os",
            "only linux node handshakes are supported",
        ));
    }
    if request.arch.trim().is_empty() {
        return Err(AppError::bad_request("invalid_arch", "arch is required"));
    }
    if request.boot_id.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_boot_id",
            "boot_id is required",
        ));
    }
    if request
        .listen_addr
        .as_deref()
        .is_some_and(|listen_addr| listen_addr.chars().count() > 128)
    {
        return Err(AppError::bad_request(
            "invalid_listen_addr",
            "listen_addr must be at most 128 characters",
        ));
    }
    Ok(())
}

fn validate_node_report_request(
    header_node_id: u64,
    report: &NodeReportRequest,
) -> Result<(), AppError> {
    if report.node_id == 0 {
        return Err(AppError::bad_request(
            "invalid_node",
            "report node_id must be positive",
        ));
    }
    if report.node_id != header_node_id {
        return Err(AppError::bad_request(
            "node_id_mismatch",
            "header node id does not match report body",
        ));
    }
    if report.node_version.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_node_version",
            "node_version is required",
        ));
    }
    if report.status.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_status",
            "status is required",
        ));
    }
    if report.timestamp == 0 {
        return Err(AppError::bad_request(
            "invalid_report_timestamp",
            "report timestamp must be positive",
        ));
    }
    Ok(())
}

fn validate_node_report_timestamp(timestamp: u64) -> Result<(), AppError> {
    if timestamp == 0 {
        return Err(AppError::bad_request(
            "invalid_timestamp",
            "X-Node-Timestamp must be positive",
        ));
    }

    let now = now_unix();
    if timestamp.abs_diff(now) > NODE_REPORT_MAX_SKEW_SEC {
        return Err(AppError::unauthorized(
            "stale_report",
            "node report timestamp is outside the allowed window",
        ));
    }

    Ok(())
}

fn verify_node_signature(
    method: &str,
    path: &str,
    secret: &str,
    timestamp: u64,
    nonce: &str,
    body_sha256: &str,
    signature: &str,
    body: &[u8],
) -> Result<(), AppError> {
    if nonce.trim().is_empty() {
        return Err(AppError::bad_request(
            "invalid_nonce",
            "X-Node-Nonce is required",
        ));
    }

    let expected_body_sha256 = BASE64.encode(Sha256::digest(body));
    if body_sha256 != expected_body_sha256 {
        return Err(AppError::bad_request(
            "body_hash_mismatch",
            "X-Node-Body-Sha256 does not match request body",
        ));
    }

    let signature = BASE64.decode(signature).map_err(|_| {
        AppError::unauthorized("invalid_signature", "node signature is not valid base64")
    })?;
    let canonical = format!("{method}\n{path}\n{timestamp}\n{nonce}\n{body_sha256}");
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes()).map_err(|error| {
        AppError::internal(anyhow::anyhow!(
            "failed to initialize node report verifier: {error}"
        ))
    })?;
    mac.update(canonical.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| AppError::unauthorized("invalid_signature", "node signature mismatch"))
}

fn required_header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, AppError> {
    headers
        .get(name)
        .ok_or_else(|| AppError::bad_request("missing_header", format!("{name} is required")))?
        .to_str()
        .map_err(|_| AppError::bad_request("invalid_header", format!("{name} is not valid UTF-8")))
}

fn required_header_u64(headers: &HeaderMap, name: &'static str) -> Result<u64, AppError> {
    required_header(headers, name)?
        .parse::<u64>()
        .map_err(|_| AppError::bad_request("invalid_header", format!("{name} must be numeric")))
}

fn report_database_status(report: &NodeReportRequest) -> &'static str {
    if report.status == "ready"
        && report.health.listeners.udp_listening
        && report.health.listeners.tcp_listening
    {
        "online"
    } else {
        "degraded"
    }
}

fn clamp_u32(value: u64) -> u32 {
    value.min(u64::from(u32::MAX)) as u32
}

fn clamp_limit(limit: Option<u32>, default: u32, max: u32) -> u32 {
    limit.unwrap_or(default).max(1).min(max)
}

fn clamp_bootstrap_ttl(ttl: Option<u64>) -> Result<u64, AppError> {
    let ttl = ttl.unwrap_or(DEFAULT_BOOTSTRAP_TTL_SEC);
    if ttl == 0 {
        return Err(AppError::bad_request(
            "invalid_bootstrap_ttl",
            "expires_in_sec must be positive",
        ));
    }
    Ok(ttl.min(MAX_BOOTSTRAP_TTL_SEC))
}

fn normalize_required_text(
    value: &str,
    field: &'static str,
    max_len: usize,
) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::bad_request(
            "invalid_field",
            format!("{field} is required"),
        ));
    }
    if value.chars().count() > max_len {
        return Err(AppError::bad_request(
            "invalid_field",
            format!("{field} must be at most {max_len} characters"),
        ));
    }
    Ok(value.to_string())
}

fn normalize_optional_text(
    value: Option<&str>,
    max_len: usize,
) -> Result<Option<String>, AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > max_len {
        return Err(AppError::bad_request(
            "invalid_field",
            format!("field must be at most {max_len} characters"),
        ));
    }
    Ok(Some(value.to_string()))
}

fn normalize_ip_text(value: &str, field: &'static str) -> Result<String, AppError> {
    let value = normalize_required_text(value, field, 64)?;
    value.parse::<IpAddr>().map_err(|_| {
        AppError::bad_request("invalid_ip", format!("{field} must be a valid IP address"))
    })?;
    Ok(value)
}

fn normalize_optional_ip_text(
    value: Option<&str>,
    field: &'static str,
) -> Result<Option<String>, AppError> {
    let Some(value) = normalize_optional_text(value, 64)? else {
        return Ok(None);
    };
    value.parse::<IpAddr>().map_err(|_| {
        AppError::bad_request("invalid_ip", format!("{field} must be a valid IP address"))
    })?;
    Ok(Some(value))
}

fn validate_port(port: u32, field: &'static str) -> Result<u32, AppError> {
    if (1..=65_535).contains(&port) {
        Ok(port)
    } else {
        Err(AppError::bad_request(
            "invalid_port",
            format!("{field} must be 1-65535"),
        ))
    }
}

fn bool_i8(value: bool) -> i8 {
    if value {
        1
    } else {
        0
    }
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let configured = state.admin_token.as_deref().ok_or_else(|| {
        AppError::service_unavailable(
            "admin_disabled",
            "admin API is disabled because XACCEL_ADMIN_TOKEN is not configured",
        )
    })?;
    let provided = admin_token_from_headers(headers)
        .ok_or_else(|| AppError::unauthorized("admin_auth_required", "admin token is required"))?;

    if constant_time_eq(configured.as_bytes(), provided.as_bytes()) {
        Ok(())
    } else {
        Err(AppError::unauthorized(
            "admin_auth_failed",
            "admin token is invalid",
        ))
    }
}

fn admin_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    if let Some(value) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
    {
        if let Some(token) = value.strip_prefix("Bearer ") {
            return Some(token.trim());
        }
    }

    headers
        .get("X-Admin-Token")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }
    diff == 0
}

fn resolve_public_base_url(state: &AppState, headers: &HeaderMap) -> Result<String, AppError> {
    if let Some(url) = state.public_base_url.as_deref() {
        return Ok(url.to_string());
    }

    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .ok_or_else(|| {
            AppError::bad_request(
                "missing_host",
                "Host or X-Forwarded-Host header is required to derive public base URL",
            )
        })?;
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|proto| !proto.is_empty())
        .unwrap_or("http");

    normalize_url_arg(&format!("{proto}://{host}"))
}

fn normalize_url_arg(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::bad_request(
            "invalid_url",
            "url must not be empty",
        ));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(AppError::bad_request(
            "invalid_url",
            "url must not contain whitespace",
        ));
    }
    if !(value.starts_with("http://") || value.starts_with("https://")) {
        return Err(AppError::bad_request(
            "invalid_url",
            "url must start with http:// or https://",
        ));
    }
    Ok(trim_trailing_slash(value))
}

fn normalize_command_arg(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::bad_request(
            "invalid_argument",
            "argument must not be empty",
        ));
    }
    if value
        .chars()
        .any(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')))
    {
        return Err(AppError::bad_request(
            "invalid_argument",
            "argument may only contain letters, numbers, dot, underscore, or dash",
        ));
    }
    Ok(value.to_string())
}

fn trim_trailing_slash(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn generate_bootstrap_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("xbt.{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn generate_node_secret() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    BASE64.encode(bytes)
}

fn hash_bootstrap_token(token: &str) -> String {
    format!(
        "sha256:{}",
        URL_SAFE_NO_PAD.encode(Sha256::digest(token.trim()))
    )
}

fn build_bootstrap_install_command(
    install_url: &str,
    bootstrap_url: &str,
    bootstrap_token: &str,
    enable_control_plane: bool,
    channel: Option<&str>,
) -> String {
    let mut command = format!(
        "curl -fsSL {install_url} | sudo bash -s -- \\\n  --bootstrap-url {bootstrap_url} \\\n  --bootstrap-token {bootstrap_token}"
    );
    if let Some(channel) = channel {
        command.push_str(" \\\n  --channel ");
        command.push_str(channel);
    }
    if enable_control_plane {
        command.push_str(" \\\n  --enable-control-plane");
    }
    command
}

fn sign_client_token(claims: &ClientTokenClaims, secret: &str) -> anyhow::Result<String> {
    let payload = serde_json::to_vec(claims).context("failed to encode claims")?;
    let payload = URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{TOKEN_PREFIX}.{TOKEN_VERSION}.{payload}");
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
        .context("failed to initialize token signer")?;
    mac.update(signing_input.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

    Ok(format!("{signing_input}.{signature}"))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn map_route_rule_write_error(error: sqlx::Error) -> AppError {
    match error {
        sqlx::Error::Database(error) if error.code().as_deref() == Some("1062") => {
            AppError::conflict(
                "route_rule_exists",
                "a route rule with the same game, node, target, and protocol already exists",
            )
        }
        sqlx::Error::Database(error) if error.code().as_deref() == Some("1452") => {
            AppError::not_found("node_not_found", "node does not exist")
        }
        error => AppError::database(error),
    }
}

impl AppError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn unauthorized(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    fn service_unavailable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message)
    }

    fn database(error: sqlx::Error) -> Self {
        error!(error = %error, "database operation failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "database operation failed",
        )
    }

    fn internal(error: anyhow::Error) -> Self {
        error!(error = %error, "internal operation failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal operation failed",
        )
    }
}

impl AdminNodeSummary {
    fn from_row(row: AdminNodeRow) -> Self {
        Self {
            endpoint: format!("{}:{}", row.server_ip, row.server_port),
            id: row.id,
            name: row.name,
            server_ip: row.server_ip,
            server_port: row.server_port,
            relay_server_ip: row.relay_server_ip,
            relay_server_port: row.relay_server_port,
            is_support_ipv6: row.is_support_ipv6 != 0,
            area: row.area,
            tag: row.tag,
            bandwidth_quality: row.bandwidth_quality,
            disable_quic: row.disable_quic != 0,
            telecom_ip: row.telecom_ip,
            mobile_ip: row.mobile_ip,
            unicom_ip: row.unicom_ip,
            status: row.status,
            kernel_version: row.kernel_version,
            config_revision: row.config_revision,
            last_seen_at: row.last_seen_at,
            last_report_at: row.last_report_at,
            latest_report: row.latest_report_id.map(|id| AdminReportSummary {
                id,
                status: row.latest_report_status.unwrap_or_default(),
                active_sessions: row.latest_active_sessions.unwrap_or_default(),
                udp_sessions: row.latest_udp_sessions.unwrap_or_default(),
                tcp_sessions: row.latest_tcp_sessions.unwrap_or_default(),
                reported_at: row.latest_reported_at,
            }),
        }
    }
}

impl AdminRouteRuleSummary {
    fn from_row(row: AdminRouteRuleRow) -> Self {
        Self {
            id: row.id,
            game_id: row.game_id,
            game_name: row.game_name,
            node_id: row.node_id,
            node_name: row.node_name,
            node_endpoint: format!("{}:{}", row.node_server_ip, row.node_server_port),
            node_status: row.node_status,
            target_addr: row.target_addr,
            protocol: row.protocol,
            area: row.area,
            tag: row.tag,
            priority: row.priority,
            status: row.status,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl NodeConfigResponse {
    fn from_row(row: NodeConfigRow) -> Self {
        Self {
            status: "ok",
            node_id: row.id,
            config_revision: row.config_revision.max(1),
            server_time: now_unix(),
            network: NodeConfigNetworkResponse {
                server_ip: row.server_ip,
                listen_ip: "0.0.0.0".to_string(),
                server_port: row.server_port,
                relay_server_ip: row.relay_server_ip,
                relay_server_port: row.relay_server_port,
                is_support_ipv6: row.is_support_ipv6 != 0,
                disable_quic: row.disable_quic != 0,
                area: row.area,
                bandwidth_quality: row.bandwidth_quality,
                tag: row.tag,
                operator_ips: NodeConfigOperatorIpsResponse {
                    telecom_ip: row.telecom_ip,
                    mobile_ip: row.mobile_ip,
                    unicom_ip: row.unicom_ip,
                },
            },
        }
    }
}

impl AdminReportDetail {
    fn from_row(row: AdminReportRow) -> Result<Self, AppError> {
        let raw = row
            .raw_json
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|error| {
                AppError::internal(anyhow::anyhow!(
                    "failed to decode node report raw_json: {error}"
                ))
            })?;

        Ok(Self {
            id: row.id,
            node_id: row.node_id,
            config_revision: row.config_revision,
            status: row.status,
            active_sessions: row.active_sessions,
            udp_sessions: row.udp_sessions,
            tcp_sessions: row.tcp_sessions,
            reported_at: row.reported_at,
            raw,
        })
    }
}

impl AdminAuditLogDetail {
    fn from_row(row: AdminAuditLogRow) -> Result<Self, AppError> {
        let detail = row
            .detail_json
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|error| {
                AppError::internal(anyhow::anyhow!(
                    "failed to decode node audit detail_json: {error}"
                ))
            })?;

        Ok(Self {
            id: row.id,
            node_id: row.node_id,
            actor_type: row.actor_type,
            actor_id: row.actor_id,
            action: row.action,
            created_at: row.created_at,
            detail,
        })
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorMessage {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_request() -> ConnectIntentRequest {
        ConnectIntentRequest {
            user_id: 1001,
            device_id: "pc-001".to_string(),
            game_id: 8888,
            platform: Some("pc".to_string()),
            client_isp: Some("telecom".to_string()),
            client_ip: Some("127.0.0.1".to_string()),
            bandwidth_quality: Some("fast".to_string()),
        }
    }

    fn valid_create_node_request() -> AdminCreateNodeRequest {
        AdminCreateNodeRequest {
            name: "node-1".to_string(),
            server_ip: "103.201.131.99".to_string(),
            server_port: 666,
            relay_server_ip: None,
            relay_server_port: None,
            is_support_ipv6: Some(false),
            bandwidth_quality: Some("fast".to_string()),
            disable_quic: Some(false),
            area: Some("UNKNOWN".to_string()),
            telecom_ip: None,
            mobile_ip: None,
            unicom_ip: None,
            tag: Some("test".to_string()),
        }
    }

    fn valid_route_rule_request() -> AdminRouteRuleRequest {
        AdminRouteRuleRequest {
            game_id: 8888,
            game_name: "Local Echo Test".to_string(),
            node_id: 1,
            target_addr: "127.0.0.1:7777".to_string(),
            protocol: Some("udp".to_string()),
            area: Some("UNKNOWN".to_string()),
            tag: Some("test".to_string()),
            priority: Some(90),
            status: Some("enabled".to_string()),
        }
    }

    #[test]
    fn validates_connect_intent_request() {
        validate_connect_intent_request(&valid_request()).expect("request is valid");
    }

    #[test]
    fn rejects_unknown_quality() {
        let mut request = valid_request();
        request.bandwidth_quality = Some("turbo".to_string());
        let error = validate_connect_intent_request(&request).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_quality");
    }

    #[test]
    fn signs_xat_v1_token() {
        let claims = ClientTokenClaims {
            node_id: 1,
            user_id: 1001,
            device_id: "pc-001".to_string(),
            game_id: 8888,
            intent_id: Some("intent-test".to_string()),
            route: Some(ClientRouteClaims {
                target_addr: "127.0.0.1:7777".to_string(),
                protocol: "udp".to_string(),
            }),
            expires_at: now_unix() + 120,
            issued_at: Some(now_unix()),
            nonce: Some("n1".to_string()),
        };

        let token = sign_client_token(&claims, "secret").expect("token signs");
        let parts = token.split('.').collect::<Vec<_>>();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "xat");
        assert_eq!(parts[1], "v1");
    }

    #[test]
    fn verifies_node_report_signature() {
        let body = br#"{"node_id":1,"config_revision":1,"node_version":"0.15.0","status":"ready","timestamp":1779250000,"health":{"listeners":{"udp_listening":true,"tcp_listening":true},"traffic":{},"sessions":{}}}"#;
        let timestamp = now_unix();
        let nonce = "test-nonce";
        let body_sha256 = BASE64.encode(Sha256::digest(body));
        let canonical = format!("POST\n{NODE_REPORT_PATH}\n{timestamp}\n{nonce}\n{body_sha256}");
        let mut mac = <HmacSha256 as Mac>::new_from_slice(b"secret").expect("hmac");
        mac.update(canonical.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        verify_node_signature(
            "POST",
            NODE_REPORT_PATH,
            "secret",
            timestamp,
            nonce,
            &body_sha256,
            &signature,
            body,
        )
        .expect("signature verifies");
    }

    #[test]
    fn verifies_node_config_signature() {
        let body = b"";
        let timestamp = now_unix();
        let nonce = "config-nonce";
        let body_sha256 = BASE64.encode(Sha256::digest(body));
        let canonical = format!("GET\n{NODE_CONFIG_PATH}\n{timestamp}\n{nonce}\n{body_sha256}");
        let mut mac = <HmacSha256 as Mac>::new_from_slice(b"secret").expect("hmac");
        mac.update(canonical.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        verify_node_signature(
            "GET",
            NODE_CONFIG_PATH,
            "secret",
            timestamp,
            nonce,
            &body_sha256,
            &signature,
            body,
        )
        .expect("signature verifies");
    }

    #[test]
    fn verifies_node_handshake_signature() {
        let body = br#"{"node_id":1,"node_version":"0.26.2","os":"linux","arch":"x86_64","boot_id":"boot-1","timestamp":1779250000,"nonce":"handshake-nonce","config_revision":1,"listen_addr":"0.0.0.0:666"}"#;
        let timestamp = now_unix();
        let nonce = "handshake-nonce";
        let body_sha256 = BASE64.encode(Sha256::digest(body));
        let canonical = format!("POST\n{NODE_HANDSHAKE_PATH}\n{timestamp}\n{nonce}\n{body_sha256}");
        let mut mac = <HmacSha256 as Mac>::new_from_slice(b"secret").expect("hmac");
        mac.update(canonical.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        verify_node_signature(
            "POST",
            NODE_HANDSHAKE_PATH,
            "secret",
            timestamp,
            nonce,
            &body_sha256,
            &signature,
            body,
        )
        .expect("signature verifies");
    }

    #[test]
    fn rejects_node_report_body_hash_mismatch() {
        let timestamp = now_unix();
        let error = verify_node_signature(
            "POST",
            NODE_REPORT_PATH,
            "secret",
            timestamp,
            "test-nonce",
            "wrong-hash",
            "wrong-signature",
            br#"{}"#,
        )
        .unwrap_err();

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "body_hash_mismatch");
    }

    #[test]
    fn maps_ready_report_to_online() {
        let report = NodeReportRequest {
            node_id: 1,
            config_revision: 1,
            node_version: "0.15.0".to_string(),
            status: "ready".to_string(),
            timestamp: now_unix(),
            health: NodeHealthSnapshot {
                listeners: NodeListenerSnapshot {
                    udp_listening: true,
                    tcp_listening: true,
                    listen_addr: Some("103.201.131.99:666".to_string()),
                },
                ..NodeHealthSnapshot::default()
            },
        };

        assert_eq!(report_database_status(&report), "online");
    }

    #[test]
    fn validates_node_handshake_request() {
        let timestamp = now_unix();
        let request = NodeHandshakeRequest {
            node_id: 1,
            node_version: "0.26.2".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            boot_id: "boot-1".to_string(),
            timestamp,
            nonce: "nonce-1".to_string(),
            config_revision: 1,
            listen_addr: Some("0.0.0.0:666".to_string()),
        };

        validate_node_handshake_request(1, timestamp, "nonce-1", &request)
            .expect("handshake is valid");
    }

    #[test]
    fn validates_admin_node_status() {
        assert_eq!(
            validate_admin_node_status("draining").expect("status"),
            "draining"
        );
        let error = validate_admin_node_status("bad").unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_node_status");
    }

    #[test]
    fn parses_admin_audit_log_detail() {
        let row = AdminAuditLogRow {
            id: 9,
            node_id: 2,
            actor_type: "admin_api".to_string(),
            actor_id: None,
            action: "node.status.update".to_string(),
            created_at: Some(1779500000),
            detail_json: Some(
                r#"{"previous_status":"online","current_status":"disabled","reason":"维护"}"#
                    .to_string(),
            ),
        };

        let detail = AdminAuditLogDetail::from_row(row).expect("audit detail parses");
        assert_eq!(detail.id, 9);
        assert_eq!(detail.node_id, 2);
        assert_eq!(detail.action, "node.status.update");
        assert_eq!(
            detail
                .detail
                .as_ref()
                .and_then(|value| value.get("current_status"))
                .and_then(Value::as_str),
            Some("disabled")
        );
    }

    #[test]
    fn reads_bearer_admin_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret".parse().unwrap());
        assert_eq!(admin_token_from_headers(&headers), Some("secret"));
    }

    #[test]
    fn embeds_admin_dashboard_html() {
        assert!(ADMIN_DASHBOARD_HTML.contains("XAccel 控制台"));
        assert!(ADMIN_DASHBOARD_HTML.contains("登录节点后台"));
        assert!(ADMIN_DASHBOARD_HTML.contains("新增节点"));
        assert!(ADMIN_DASHBOARD_HTML.contains("编辑配置"));
        assert!(ADMIN_DASHBOARD_HTML.contains("控制总览"));
        assert!(ADMIN_DASHBOARD_HTML.contains("游戏路由"));
        assert!(ADMIN_DASHBOARD_HTML.contains("操作日志"));
        assert!(ADMIN_DASHBOARD_HTML.contains("恢复调度"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-resume-node"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/nodes"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/game-route-rules"));
        assert!(ADMIN_DASHBOARD_HTML.contains("method: \"PATCH\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("bootstrap-token"));
    }

    #[test]
    fn compares_tokens_in_constant_time_shape() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"other"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
    }

    #[test]
    fn validates_bootstrap_request() {
        let request = BootstrapRequest {
            bootstrap_token: "xbt.test".to_string(),
            hostname: "node-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            kernel: "6.8.0".to_string(),
            ips: vec!["103.201.131.99".to_string()],
            installer_version: "0.15.0".to_string(),
        };

        validate_bootstrap_request(&request).expect("request is valid");
    }

    #[test]
    fn normalizes_admin_create_node_request() {
        let node =
            normalize_create_node_request(&valid_create_node_request()).expect("node is valid");

        assert_eq!(node.name, "node-1");
        assert_eq!(node.server_ip, "103.201.131.99");
        assert_eq!(node.server_port, 666);
        assert_eq!(node.bandwidth_quality, "fast");
        assert_eq!(node.disable_quic, 0);
        assert_eq!(node.area, "UNKNOWN");
    }

    #[test]
    fn rejects_invalid_admin_create_node_ip() {
        let mut request = valid_create_node_request();
        request.server_ip = "not-an-ip".to_string();

        let error = normalize_create_node_request(&request).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_ip");
    }

    #[test]
    fn normalizes_route_rule_request() {
        let rule = normalize_route_rule_request(&valid_route_rule_request()).expect("rule");

        assert_eq!(rule.game_id, 8888);
        assert_eq!(rule.game_name, "Local Echo Test");
        assert_eq!(rule.node_id, 1);
        assert_eq!(rule.target_addr, "127.0.0.1:7777");
        assert_eq!(rule.protocol, "udp");
        assert_eq!(rule.priority, 90);
        assert_eq!(rule.status, "enabled");
    }

    #[test]
    fn rejects_invalid_route_rule_target() {
        let mut request = valid_route_rule_request();
        request.target_addr = "127.0.0.1".to_string();

        let error = normalize_route_rule_request(&request).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_target_addr");
    }

    #[test]
    fn rejects_empty_route_rule_game_name() {
        let mut request = valid_route_rule_request();
        request.game_name = "  ".to_string();

        let error = normalize_route_rule_request(&request).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_field");
    }

    #[test]
    fn rejects_invalid_route_rule_status() {
        let mut request = valid_route_rule_request();
        request.status = Some("paused".to_string());

        let error = normalize_route_rule_request(&request).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_route_status");
    }

    #[test]
    fn hashes_bootstrap_token_without_plaintext() {
        let hash = hash_bootstrap_token("xbt.secret");

        assert!(hash.starts_with("sha256:"));
        assert!(!hash.contains("secret"));
        assert_eq!(hash, hash_bootstrap_token("xbt.secret"));
    }

    #[test]
    fn builds_bootstrap_install_command() {
        let command = build_bootstrap_install_command(
            DEFAULT_INSTALL_URL,
            "http://127.0.0.1:18080/api/node/v1/bootstrap",
            "xbt.token",
            true,
            Some("stable"),
        );

        assert!(command.contains("--bootstrap-url http://127.0.0.1:18080/api/node/v1/bootstrap"));
        assert!(command.contains("--bootstrap-token xbt.token"));
        assert!(command.contains("--enable-control-plane"));
    }
}
