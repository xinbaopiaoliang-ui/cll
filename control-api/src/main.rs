use anyhow::{bail, Context};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use base64::{
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
    Engine,
};
use clap::Parser;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{mysql::MySqlPoolOptions, FromRow, MySqlPool};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tracing::{error, info};

type HmacSha256 = Hmac<Sha256>;

const TOKEN_PREFIX: &str = "xat";
const TOKEN_VERSION: &str = "v1";
const NODE_REPORT_PATH: &str = "/api/node/v1/report";
const NODE_REPORT_MAX_SKEW_SEC: u64 = 300;
const VERSION: &str = env!("CARGO_PKG_VERSION");

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
}

#[derive(Clone)]
struct AppState {
    pool: MySqlPool,
    token_ttl_sec: u64,
    admin_token: Option<String>,
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
struct AdminListNodesQuery {
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AdminNodeDetailQuery {
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AdminUpdateNodeStatusRequest {
    status: String,
    reason: Option<String>,
}

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
struct AdminNodeSummary {
    id: u64,
    name: String,
    endpoint: String,
    server_ip: String,
    server_port: u32,
    area: String,
    tag: Option<String>,
    bandwidth_quality: String,
    disable_quic: bool,
    status: String,
    kernel_version: Option<String>,
    config_revision: u64,
    last_seen_at: Option<u64>,
    last_report_at: Option<u64>,
    latest_report: Option<AdminReportSummary>,
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
    area: String,
    tag: Option<String>,
    bandwidth_quality: String,
    disable_quic: i8,
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

    let state = AppState {
        pool,
        token_ttl_sec: cli.token_ttl_sec.max(1),
        admin_token: cli
            .admin_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(ToOwned::to_owned),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/client/v1/connect-intent", post(connect_intent))
        .route(NODE_REPORT_PATH, post(node_report))
        .route("/api/admin/v1/nodes", get(admin_list_nodes))
        .route("/api/admin/v1/nodes/:node_id", get(admin_get_node))
        .route(
            "/api/admin/v1/nodes/:node_id/status",
            patch(admin_update_node_status),
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
    verify_node_report_signature(
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

    Ok(Json(AdminNodeDetailResponse {
        node: AdminNodeSummary::from_row(node),
        recent_reports: reports,
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

async fn select_admin_nodes(pool: &MySqlPool, limit: u32) -> Result<Vec<AdminNodeRow>, AppError> {
    sqlx::query_as::<_, AdminNodeRow>(
        r#"
SELECT
  n.id,
  n.name,
  n.server_ip,
  n.server_port,
  n.area,
  n.tag,
  n.bandwidth_quality,
  n.disable_quic,
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
  n.area,
  n.tag,
  n.bandwidth_quality,
  n.disable_quic,
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
  config_revision = ?,
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

fn verify_node_report_signature(
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
    let canonical = format!("POST\n{NODE_REPORT_PATH}\n{timestamp}\n{nonce}\n{body_sha256}");
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
            area: row.area,
            tag: row.tag,
            bandwidth_quality: row.bandwidth_quality,
            disable_quic: row.disable_quic != 0,
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
        let body = br#"{"node_id":1,"config_revision":1,"node_version":"0.14.0","status":"ready","timestamp":1779250000,"health":{"listeners":{"udp_listening":true,"tcp_listening":true},"traffic":{},"sessions":{}}}"#;
        let timestamp = now_unix();
        let nonce = "test-nonce";
        let body_sha256 = BASE64.encode(Sha256::digest(body));
        let canonical = format!("POST\n{NODE_REPORT_PATH}\n{timestamp}\n{nonce}\n{body_sha256}");
        let mut mac = <HmacSha256 as Mac>::new_from_slice(b"secret").expect("hmac");
        mac.update(canonical.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        verify_node_report_signature("secret", timestamp, nonce, &body_sha256, &signature, body)
            .expect("signature verifies");
    }

    #[test]
    fn rejects_node_report_body_hash_mismatch() {
        let timestamp = now_unix();
        let error = verify_node_report_signature(
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
            node_version: "0.14.0".to_string(),
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
    fn reads_bearer_admin_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret".parse().unwrap());
        assert_eq!(admin_token_from_headers(&headers), Some("secret"));
    }

    #[test]
    fn compares_tokens_in_constant_time_shape() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"other"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
    }
}
