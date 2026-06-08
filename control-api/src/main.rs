use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{bail, Context};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, patch, post, put},
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
    io::ErrorKind,
    net::{IpAddr, SocketAddr},
    path::Path as FsPath,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, UdpSocket},
    process::Command,
    time::{sleep, timeout},
};
use tracing::{error, info};

type HmacSha256 = Hmac<Sha256>;

const TOKEN_PREFIX: &str = "xat";
const TOKEN_VERSION: &str = "v1";
const NODE_REPORT_PATH: &str = "/api/node/v1/report";
const NODE_HANDSHAKE_PATH: &str = "/api/node/v1/handshake";
const NODE_CONFIG_PATH: &str = "/api/node/v1/config";
const NODE_TASKS_PATH: &str = "/api/node/v1/tasks";
const NODE_REPORT_MAX_SKEW_SEC: u64 = 300;
const NODE_BOOTSTRAP_PATH: &str = "/api/node/v1/bootstrap";
const DEFAULT_BOOTSTRAP_TTL_SEC: u64 = 3600;
const MAX_BOOTSTRAP_TTL_SEC: u64 = 86_400;
const DEFAULT_INSTALL_URL: &str =
    "https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MIN_NODE_VERSION: &str = "0.1.0";
const ADMIN_DASHBOARD_HTML: &str = include_str!("../static/admin-dashboard.html");
const PROTOCOL_VERSION: &str = "xaccel/1";
const UDP_BUFFER_BYTES: usize = 64 * 1024;
const DEFAULT_DIAGNOSTIC_PAYLOAD: &str = "hello";
const DEFAULT_DIAGNOSTIC_TIMEOUT_SEC: u64 = 3;
const DEFAULT_DIAGNOSTIC_RESPONSE_TIMEOUT_MS: u64 = 500;
const SSH_KNOWN_HOSTS_FILE: &str = "/var/lib/xaccel-control-api/known_hosts";
const SSH_ACTION_TIMEOUT_SEC: u64 = 120;
const MAX_STORED_LAST_ERROR_CHARS: usize = 4096;
const SSH_BOOTSTRAP_TTL_SEC: u64 = 3600;
const SSH_UPGRADE_REPORT_WAIT_SEC: u64 = 45;
const SSH_UPGRADE_REPORT_POLL_SEC: u64 = 3;
const ADMIN_SESSION_PREFIX: &str = "xas";
const ADMIN_SESSION_VERSION: &str = "v1";
const ADMIN_SESSION_TTL_SEC: u64 = 8 * 60 * 60;
const ADMIN_PASSWORD_SCHEME: &str = "pbkdf2-sha256";
const ADMIN_PASSWORD_ITERATIONS: u32 = 120_000;
const SYSTEM_DIAGNOSTIC_CORE_TABLES: [&str; 6] = [
    "accel_nodes",
    "accel_games",
    "game_route_rules",
    "node_runtime_reports",
    "node_health_alerts",
    "node_operation_tasks",
];

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

    #[arg(long, env = "XACCEL_BUSINESS_SYNC_TOKEN")]
    business_sync_token: Option<String>,

    #[arg(long, env = "XACCEL_CREDENTIAL_KEY")]
    credential_key: Option<String>,
}

#[derive(Clone)]
struct AppState {
    pool: MySqlPool,
    listen: SocketAddr,
    token_ttl_sec: u64,
    admin_token: Option<String>,
    public_base_url: Option<String>,
    business_sync_token: Option<String>,
    credential_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConnectIntentRequest {
    user_id: u64,
    device_id: String,
    game_id: u64,
    region_id: Option<u64>,
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

#[derive(Debug, Deserialize)]
struct AdminConnectivityDiagnosticRequest {
    user_id: u64,
    device_id: String,
    game_id: u64,
    region_id: Option<u64>,
    platform: Option<String>,
    client_isp: Option<String>,
    client_ip: Option<String>,
    bandwidth_quality: Option<String>,
    payload: Option<String>,
    timeout_sec: Option<u64>,
    response_timeout_ms: Option<u64>,
    candidate_index: Option<usize>,
    skip_session_data: Option<bool>,
}

#[derive(Debug, Serialize)]
struct AdminConnectivityDiagnosticResponse {
    status: &'static str,
    version: &'static str,
    server_time: u64,
    connect_intent: ConnectIntentResponse,
    selected_candidate_index: usize,
    node: DiagnosticNodeSummary,
    probe: Option<DiagnosticProbeSummary>,
    session_data: Option<DiagnosticSessionDataSummary>,
    error: Option<DiagnosticStepError>,
}

#[derive(Debug, Deserialize)]
struct AdminLoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct AdminLoginResponse {
    status: &'static str,
    token: String,
    expires_at: u64,
    admin: AdminCurrentUser,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminMeResponse {
    admin: AdminCurrentUser,
    server_time: u64,
}

#[derive(Debug, Serialize, Clone)]
struct AdminCurrentUser {
    id: Option<u64>,
    username: String,
    display_name: Option<String>,
    role: String,
    auth_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AdminSessionClaims {
    user_id: u64,
    username: String,
    display_name: Option<String>,
    role: String,
    exp: u64,
    nonce: String,
}

#[derive(Debug, Serialize)]
struct DiagnosticNodeSummary {
    node_id: u64,
    node_version: Option<String>,
    address: String,
    area: String,
    tag: String,
    transports: Vec<String>,
    bandwidth_quality: String,
    route: ClientRouteClaims,
    scheduler: CandidateSchedulerInfo,
}

#[derive(Debug, Serialize)]
struct DiagnosticProbeSummary {
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
struct DiagnosticSessionDataSummary {
    latency_ms: u128,
    status: String,
    request_payload_bytes: u64,
    response_payload_bytes: u64,
    response_payload_base64: String,
    response_payload_text: Option<String>,
    target_addr: Option<String>,
    relay: Option<DiagnosticRelaySummary>,
}

#[derive(Debug, Serialize)]
struct DiagnosticRelaySummary {
    mode: String,
    timeout_ms: u64,
    timed_out: bool,
    upstream_tx_bytes: u64,
    upstream_rx_bytes: u64,
}

#[derive(Debug, Serialize)]
struct DiagnosticStepError {
    step: &'static str,
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct NodeProbeRequest {
    #[serde(rename = "type")]
    message_type: &'static str,
    protocol: &'static str,
    client_nonce: String,
    user_id: u64,
    device_id: String,
    game_id: u64,
    transport: &'static str,
    token: String,
}

#[derive(Debug, Deserialize)]
struct NodeProbeResponse {
    #[serde(rename = "type")]
    message_type: String,
    node_id: Option<u64>,
    node_version: String,
    transport: String,
    session: NodeProbeSession,
}

#[derive(Debug, Deserialize)]
struct NodeProbeSession {
    session_id: String,
    ttl_sec: u64,
    intent_id: Option<String>,
    route_target_addr: Option<String>,
    credential_valid: bool,
    credential_expires_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct NodeSessionDataRequest {
    #[serde(rename = "type")]
    message_type: &'static str,
    protocol: &'static str,
    session_id: String,
    client_nonce: String,
    payload: String,
    response_timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
struct NodeSessionDataResponse {
    #[serde(rename = "type")]
    message_type: String,
    status: String,
    payload: String,
    payload_bytes: u64,
    request_payload_bytes: u64,
    target: Option<NodeTargetInfo>,
    relay: Option<NodeRelayInfo>,
}

#[derive(Debug, Deserialize)]
struct NodeTargetInfo {
    address: String,
}

#[derive(Debug, Deserialize)]
struct NodeRelayInfo {
    mode: String,
    timeout_ms: u64,
    timed_out: bool,
    upstream_tx_bytes: u64,
    upstream_rx_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct NodeErrorResponse {
    error: NodeErrorBody,
}

#[derive(Debug, Deserialize)]
struct NodeErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct ClientContext {
    platform: Option<String>,
    client_isp: Option<String>,
    client_ip: Option<String>,
    bandwidth_quality: String,
    region_id: Option<u64>,
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
    scheduler: CandidateSchedulerInfo,
}

#[derive(Debug, Clone, Serialize)]
struct CandidateSchedulerInfo {
    route_priority: u32,
    latest_active_sessions: u32,
    latest_udp_sessions: u32,
    latest_tcp_sessions: u32,
    latest_reported_at: Option<u64>,
    latest_report_age_sec: Option<u64>,
    report_fresh: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    region_id: Option<u64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    region_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    database: &'static str,
}

#[derive(Debug, Serialize)]
struct AdminSystemDiagnosticsResponse {
    status: &'static str,
    version: &'static str,
    listen_addr: String,
    public_base_url: Option<String>,
    generated_at: u64,
    counts: AdminSystemDiagnosticCounts,
    checks: Vec<AdminSystemDiagnosticCheck>,
    server_time: u64,
}

#[derive(Debug, Default, Serialize)]
struct AdminSystemDiagnosticCounts {
    nodes_total: u64,
    nodes_online: u64,
    nodes_reporting: u64,
    games_enabled: u64,
    routes_enabled: u64,
    active_alerts: u64,
}

#[derive(Debug, Serialize)]
struct AdminSystemDiagnosticCheck {
    key: &'static str,
    title: &'static str,
    status: &'static str,
    message: String,
    suggestion: Option<String>,
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

#[derive(Debug, Serialize)]
struct NodeTasksResponse {
    status: &'static str,
    node_id: u64,
    tasks: Vec<NodeTaskItem>,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct NodeTaskItem {
    task_id: u64,
    task_type: String,
    status: String,
    message: Option<String>,
    created_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct NodeTaskResultRequest {
    node_id: u64,
    task_id: u64,
    status: String,
    message: Option<String>,
    output: Option<String>,
    started_at: Option<u64>,
    finished_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct NodeTaskResultResponse {
    status: &'static str,
    node_id: u64,
    task_id: u64,
    stored: bool,
    server_time: u64,
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
struct AdminDeployNodeRequest {
    public_base_url: Option<String>,
    install_url: Option<String>,
    expires_in_sec: Option<u64>,
    enable_control_plane: Option<bool>,
    channel: Option<String>,
    ssh: AdminSshCredentialRequest,
    save_credential: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AdminCreateNodeTaskRequest {
    task_type: String,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdminSshCredentialRequest {
    host: Option<String>,
    port: Option<u16>,
    username: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct AdminSshCredentialResponse {
    status: &'static str,
    node_id: u64,
    credential: AdminSshCredentialSummary,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminDeleteSshCredentialResponse {
    status: &'static str,
    node_id: u64,
    deleted: bool,
    server_time: u64,
}

#[derive(Debug, Deserialize)]
struct AdminSshActionRequest {
    action: String,
}

#[derive(Debug, Serialize)]
struct AdminSshActionResponse {
    status: &'static str,
    node_id: u64,
    action: String,
    command_label: String,
    exit_code: Option<i32>,
    output: String,
    duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_check: Option<AdminSshActionVersionCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<AdminOperationTaskSummary>,
    server_time: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AdminSshActionVersionCheck {
    before_version: Option<String>,
    after_version: Option<String>,
    version_changed: bool,
    report_refreshed: bool,
    before_report_at: Option<u64>,
    after_report_at: Option<u64>,
    waited_ms: u128,
    message: String,
}

#[derive(Debug, Deserialize)]
struct AdminListOperationTasksQuery {
    node_id: Option<u64>,
    status: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct AdminListOperationTasksResponse {
    tasks: Vec<AdminOperationTaskSummary>,
    total: usize,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminOperationTaskResponse {
    task: AdminOperationTaskSummary,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminListUsersResponse {
    users: Vec<AdminUserSummary>,
    total: usize,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminUserResponse {
    status: &'static str,
    user: AdminUserSummary,
    server_time: u64,
}

#[derive(Debug, Deserialize)]
struct AdminCreateUserRequest {
    username: String,
    display_name: Option<String>,
    password: String,
    role: String,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdminUpdateUserRequest {
    display_name: Option<String>,
    password: Option<String>,
    role: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdminListAuditLogsQuery {
    node_id: Option<u64>,
    action: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct AdminListAuditLogsResponse {
    logs: Vec<AdminAuditLogDetail>,
    total: usize,
    server_time: u64,
}

#[derive(Debug, Deserialize)]
struct AdminListHealthAlertsQuery {
    node_id: Option<u64>,
    status: Option<String>,
    severity: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AdminUpdateHealthAlertRequest {
    status: String,
}

#[derive(Debug, Serialize)]
struct AdminListHealthAlertsResponse {
    alerts: Vec<AdminHealthAlertSummary>,
    total: usize,
    summary: AdminHealthAlertCounts,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminHealthAlertResponse {
    status: &'static str,
    alert: AdminHealthAlertSummary,
    server_time: u64,
}

#[derive(Debug, Default, Serialize)]
struct AdminHealthAlertCounts {
    open: usize,
    acknowledged: usize,
    ignored: usize,
    resolved: usize,
    critical: usize,
    warning: usize,
}

#[derive(Debug, Serialize)]
struct AdminHealthAlertSummary {
    id: u64,
    node_id: u64,
    node_name: String,
    node_endpoint: String,
    alert_key: String,
    severity: String,
    title: String,
    message: String,
    status: String,
    first_seen_at: Option<u64>,
    last_seen_at: Option<u64>,
    acknowledged_at: Option<u64>,
    acknowledged_by: Option<u64>,
    resolved_at: Option<u64>,
    updated_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct AdminListGamesQuery {
    status: Option<String>,
    platform: Option<String>,
    keyword: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AdminGameRequest {
    game_id: u64,
    name: String,
    platform: Option<String>,
    category: Option<String>,
    icon_url: Option<String>,
    status: Option<String>,
    remark: Option<String>,
}

type AdminUpdateGameRequest = AdminGameRequest;

#[derive(Debug, Deserialize)]
struct AdminListRouteRulesQuery {
    game_id: Option<u64>,
    region_id: Option<u64>,
    node_id: Option<u64>,
    status: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AdminRouteRuleRequest {
    game_id: u64,
    game_name: String,
    region_id: Option<u64>,
    region_name: Option<String>,
    node_id: u64,
    target_addr: String,
    protocol: Option<String>,
    area: Option<String>,
    tag: Option<String>,
    priority: Option<u32>,
    status: Option<String>,
}

type AdminUpdateRouteRuleRequest = AdminRouteRuleRequest;

#[derive(Debug, Deserialize)]
struct BusinessSyncCatalogRequest {
    source: Option<String>,
    revision: Option<String>,
    #[serde(default)]
    games: Vec<BusinessSyncGame>,
    #[serde(default)]
    regions: Vec<BusinessSyncRegion>,
    #[serde(default, alias = "routes")]
    route_rules: Vec<BusinessSyncRouteRule>,
}

#[derive(Debug, Deserialize)]
struct BusinessSyncGame {
    game_id: u64,
    name: String,
    platform: Option<String>,
    category: Option<String>,
    icon_url: Option<String>,
    status: Option<String>,
    remark: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BusinessSyncRegion {
    game_id: u64,
    region_id: u64,
    name: String,
    area: Option<String>,
    status: Option<String>,
    remark: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BusinessSyncRouteRule {
    external_id: Option<String>,
    game_id: u64,
    game_name: Option<String>,
    region_id: Option<u64>,
    region_name: Option<String>,
    node_id: u64,
    target_addr: String,
    protocol: Option<String>,
    area: Option<String>,
    tag: Option<String>,
    priority: Option<u32>,
    status: Option<String>,
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
    recent_tasks: Vec<AdminNodeTaskSummary>,
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
struct AdminDeleteNodeResponse {
    status: &'static str,
    node_id: u64,
    deleted: bool,
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
struct AdminDeployNodeResponse {
    status: &'static str,
    node_id: u64,
    action: String,
    command_label: String,
    exit_code: Option<i32>,
    output: String,
    duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_check: Option<AdminSshActionVersionCheck>,
    task: AdminOperationTaskSummary,
    credential_saved: bool,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminCreateNodeTaskResponse {
    status: &'static str,
    node_id: u64,
    task: AdminNodeTaskSummary,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminListGamesResponse {
    games: Vec<AdminGameSummary>,
    total: usize,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminGameResponse {
    status: &'static str,
    game: AdminGameSummary,
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct AdminDeleteGameResponse {
    status: &'static str,
    game_id: u64,
    deleted: bool,
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
struct BusinessSyncCatalogResponse {
    status: &'static str,
    source: String,
    revision: Option<String>,
    games_upserted: usize,
    regions_upserted: usize,
    route_rules_upserted: usize,
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
    ssh_credential: AdminSshCredentialSummary,
}

#[derive(Debug, Serialize)]
struct AdminNodeTaskSummary {
    id: u64,
    node_id: u64,
    task_type: String,
    status: String,
    message: Option<String>,
    output: Option<String>,
    error_message: Option<String>,
    created_at: Option<u64>,
    claimed_at: Option<u64>,
    started_at: Option<u64>,
    finished_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdminOperationTaskSummary {
    id: u64,
    node_id: u64,
    node_name: String,
    node_endpoint: String,
    action: String,
    action_label: String,
    executor: String,
    status: String,
    command_label: String,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    output: Option<String>,
    error_message: Option<String>,
    version_check: Option<AdminSshActionVersionCheck>,
    created_at: Option<u64>,
    started_at: Option<u64>,
    finished_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdminUserSummary {
    id: u64,
    username: String,
    display_name: Option<String>,
    role: String,
    status: String,
    last_login_at: Option<u64>,
    created_at: Option<u64>,
    updated_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdminSshCredentialSummary {
    configured: bool,
    host: Option<String>,
    port: Option<u32>,
    username: Option<String>,
    auth_status: Option<String>,
    last_error: Option<String>,
    last_checked_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdminGameSummary {
    id: u64,
    game_id: u64,
    name: String,
    platform: String,
    category: Option<String>,
    icon_url: Option<String>,
    status: String,
    remark: Option<String>,
    route_count: u64,
    created_at: Option<u64>,
    updated_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdminRouteRuleSummary {
    id: u64,
    game_id: u64,
    game_name: String,
    region_id: Option<u64>,
    region_name: Option<String>,
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
    sync_source: Option<String>,
    external_id: Option<String>,
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
    node_name: String,
    node_endpoint: String,
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
    region_id: Option<u64>,
    region_name: Option<String>,
    route_priority: u32,
    latest_active_sessions: Option<u32>,
    latest_udp_sessions: Option<u32>,
    latest_tcp_sessions: Option<u32>,
    latest_reported_at: Option<u64>,
}

#[derive(Debug, FromRow)]
struct AdminGameRow {
    id: u64,
    game_id: u64,
    name: String,
    platform: String,
    category: Option<String>,
    icon_url: Option<String>,
    status: String,
    remark: Option<String>,
    route_count: u64,
    created_at: Option<u64>,
    updated_at: Option<u64>,
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
    latest_report_raw_json: Option<String>,
    ssh_host: Option<String>,
    ssh_port: Option<u32>,
    ssh_username: Option<String>,
    ssh_auth_status: Option<String>,
    ssh_last_error: Option<String>,
    ssh_last_checked_at: Option<u64>,
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
    node_name: String,
    node_server_ip: String,
    node_server_port: u32,
    actor_type: String,
    actor_id: Option<u64>,
    action: String,
    created_at: Option<u64>,
    detail_json: Option<String>,
}

#[derive(Debug, FromRow)]
struct HealthAlertRow {
    id: u64,
    node_id: u64,
    node_name: String,
    node_server_ip: String,
    node_server_port: u32,
    alert_key: String,
    severity: String,
    title: String,
    message: String,
    status: String,
    first_seen_at: Option<u64>,
    last_seen_at: Option<u64>,
    acknowledged_at: Option<u64>,
    acknowledged_by: Option<u64>,
    resolved_at: Option<u64>,
    updated_at: Option<u64>,
}

#[derive(Debug, FromRow)]
struct NodeTaskRow {
    id: u64,
    node_id: u64,
    task_type: String,
    status: String,
    message: Option<String>,
    output: Option<String>,
    error_message: Option<String>,
    created_at: Option<u64>,
    claimed_at: Option<u64>,
    started_at: Option<u64>,
    finished_at: Option<u64>,
}

#[derive(Debug, FromRow)]
struct OperationTaskRow {
    id: u64,
    node_id: u64,
    node_name: String,
    node_server_ip: String,
    node_server_port: u32,
    action: String,
    executor: String,
    status: String,
    command_label: String,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    output: Option<String>,
    error_message: Option<String>,
    version_check_json: Option<String>,
    created_at: Option<u64>,
    started_at: Option<u64>,
    finished_at: Option<u64>,
}

#[derive(Debug, FromRow)]
struct AdminUserRow {
    id: u64,
    username: String,
    display_name: Option<String>,
    password_hash: String,
    role: String,
    status: String,
    last_login_at: Option<u64>,
    created_at: Option<u64>,
    updated_at: Option<u64>,
}

#[derive(Debug, FromRow)]
struct SshCredentialRow {
    host: String,
    port: u32,
    username: String,
    password_ciphertext: String,
    password_nonce: String,
    auth_status: String,
    last_error: Option<String>,
    last_checked_at: Option<u64>,
}

#[derive(Debug)]
struct NormalizedSshCredential {
    host: String,
    port: u16,
    username: String,
    password: String,
}

struct SshActionPlan {
    command_label: String,
    remote_command: String,
    send_password_to_stdin: bool,
}

#[derive(Debug, Clone)]
struct AdminActor {
    id: Option<u64>,
    username: String,
    display_name: Option<String>,
    role: String,
    auth_type: String,
}

struct SshCommandOutput {
    exit_code: Option<i32>,
    combined: String,
}

struct SshCommandError {
    exit_code: Option<i32>,
    message: String,
}

#[derive(Debug, FromRow)]
struct AdminRouteRuleRow {
    id: u64,
    game_id: u64,
    game_name: String,
    region_id: Option<u64>,
    region_name: Option<String>,
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
    sync_source: Option<String>,
    external_id: Option<String>,
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
struct NormalizedGame {
    game_id: u64,
    name: String,
    platform: String,
    category: Option<String>,
    icon_url: Option<String>,
    status: String,
    remark: Option<String>,
}

#[derive(Debug)]
struct NormalizedGameRegion {
    game_id: u64,
    region_id: u64,
    name: String,
    area: Option<String>,
    status: String,
    remark: Option<String>,
}

#[derive(Debug)]
struct NormalizedRouteRule {
    game_id: u64,
    game_name: String,
    region_id: Option<u64>,
    region_name: Option<String>,
    node_id: u64,
    target_addr: String,
    protocol: String,
    area: Option<String>,
    tag: Option<String>,
    priority: u32,
    status: String,
    sync_source: Option<String>,
    external_id: Option<String>,
}

#[derive(Debug)]
struct BusinessSyncCatalog {
    source: String,
    revision: Option<String>,
    games: Vec<NormalizedGame>,
    regions: Vec<NormalizedGameRegion>,
    route_rules: Vec<NormalizedRouteRule>,
}

#[derive(Debug)]
struct HealthAlertSpec {
    key: &'static str,
    severity: &'static str,
    title: String,
    message: String,
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
        listen: cli.listen,
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
        business_sync_token: cli
            .business_sync_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(ToOwned::to_owned),
        credential_key: cli
            .credential_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(ToOwned::to_owned),
    };
    let app = Router::new()
        .route("/admin", get(admin_dashboard))
        .route("/health", get(health))
        .route("/api/client/v1/connect-intent", post(connect_intent))
        .route("/api/business/v1/sync-catalog", post(business_sync_catalog))
        .route(NODE_BOOTSTRAP_PATH, post(node_bootstrap))
        .route(NODE_HANDSHAKE_PATH, post(node_handshake))
        .route(NODE_CONFIG_PATH, get(node_config))
        .route(NODE_REPORT_PATH, post(node_report))
        .route(NODE_TASKS_PATH, get(node_tasks))
        .route("/api/node/v1/tasks/:task_id/result", post(node_task_result))
        .route(
            "/api/admin/v1/nodes",
            get(admin_list_nodes).post(admin_create_node),
        )
        .route("/api/admin/v1/login", post(admin_login))
        .route("/api/admin/v1/me", get(admin_me))
        .route(
            "/api/admin/v1/admin-users",
            get(admin_list_users).post(admin_create_user),
        )
        .route(
            "/api/admin/v1/admin-users/:user_id",
            patch(admin_update_user).delete(admin_delete_user),
        )
        .route(
            "/api/admin/v1/nodes/:node_id",
            get(admin_get_node)
                .patch(admin_update_node)
                .delete(admin_delete_node),
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
            "/api/admin/v1/nodes/:node_id/deploy",
            post(admin_deploy_node),
        )
        .route(
            "/api/admin/v1/nodes/:node_id/tasks",
            post(admin_create_node_task),
        )
        .route(
            "/api/admin/v1/nodes/:node_id/ssh-credential",
            put(admin_upsert_ssh_credential).delete(admin_delete_ssh_credential),
        )
        .route(
            "/api/admin/v1/nodes/:node_id/ssh-actions",
            post(admin_run_ssh_action),
        )
        .route(
            "/api/admin/v1/operation-tasks",
            get(admin_list_operation_tasks),
        )
        .route(
            "/api/admin/v1/operation-tasks/:task_id",
            get(admin_get_operation_task),
        )
        .route("/api/admin/v1/health-alerts", get(admin_list_health_alerts))
        .route(
            "/api/admin/v1/health-alerts/:alert_id",
            patch(admin_update_health_alert),
        )
        .route("/api/admin/v1/audit-logs", get(admin_list_audit_logs))
        .route(
            "/api/admin/v1/connectivity-diagnostic",
            post(admin_connectivity_diagnostic),
        )
        .route(
            "/api/admin/v1/system/diagnostics",
            get(admin_system_diagnostics),
        )
        .route(
            "/api/admin/v1/games",
            get(admin_list_games).post(admin_create_game),
        )
        .route(
            "/api/admin/v1/games/:game_id",
            patch(admin_update_game).delete(admin_delete_game),
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

async fn admin_login(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AdminLoginRequest>,
) -> Result<Json<AdminLoginResponse>, AppError> {
    let signing_secret = state.admin_token.as_deref().ok_or_else(|| {
        AppError::service_unavailable(
            "admin_disabled",
            "admin login is disabled because XACCEL_ADMIN_TOKEN is not configured",
        )
    })?;
    let username = normalize_admin_username(&request.username)?;
    let user = select_admin_user_by_username(&state.pool, &username)
        .await?
        .ok_or_else(|| {
            AppError::unauthorized("admin_auth_failed", "username or password is invalid")
        })?;
    if user.status != "active" {
        return Err(AppError::unauthorized(
            "admin_user_disabled",
            "admin user is disabled",
        ));
    }
    if !verify_admin_password(&request.password, &user.password_hash)? {
        return Err(AppError::unauthorized(
            "admin_auth_failed",
            "username or password is invalid",
        ));
    }
    let (token, expires_at) = create_admin_session_token(signing_secret, &user)?;
    mark_admin_user_login(&state.pool, user.id).await?;

    Ok(Json(AdminLoginResponse {
        status: "ok",
        token,
        expires_at,
        admin: AdminActor {
            id: Some(user.id),
            username: user.username,
            display_name: user.display_name,
            role: user.role,
            auth_type: "password".to_string(),
        }
        .current_user(),
        server_time: now_unix(),
    }))
}

async fn admin_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AdminMeResponse>, AppError> {
    let actor = require_admin(&state, &headers)?;
    Ok(Json(AdminMeResponse {
        admin: actor.current_user(),
        server_time: now_unix(),
    }))
}

async fn admin_list_users(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AdminListUsersResponse>, AppError> {
    require_admin_super(&state, &headers)?;
    let users = select_admin_users(&state.pool)
        .await?
        .into_iter()
        .map(AdminUserSummary::from_row)
        .collect::<Vec<_>>();

    Ok(Json(AdminListUsersResponse {
        total: users.len(),
        users,
        server_time: now_unix(),
    }))
}

async fn admin_create_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdminCreateUserRequest>,
) -> Result<Json<AdminUserResponse>, AppError> {
    require_admin_super(&state, &headers)?;
    let user = insert_admin_user(&state.pool, request).await?;
    Ok(Json(AdminUserResponse {
        status: "ok",
        user: AdminUserSummary::from_row(user),
        server_time: now_unix(),
    }))
}

async fn admin_update_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
    Json(request): Json<AdminUpdateUserRequest>,
) -> Result<Json<AdminUserResponse>, AppError> {
    let actor = require_admin_super(&state, &headers)?;
    if actor.id == Some(user_id) {
        if request.status.as_deref() == Some("disabled") {
            return Err(AppError::bad_request(
                "cannot_disable_self",
                "current admin user cannot disable itself",
            ));
        }
        if request
            .role
            .as_deref()
            .is_some_and(|role| role != "super_admin")
        {
            return Err(AppError::bad_request(
                "cannot_downgrade_self",
                "current admin user cannot downgrade itself",
            ));
        }
    }
    let user = update_admin_user(&state.pool, user_id, request).await?;
    Ok(Json(AdminUserResponse {
        status: "ok",
        user: AdminUserSummary::from_row(user),
        server_time: now_unix(),
    }))
}

async fn admin_delete_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<AdminUserResponse>, AppError> {
    let actor = require_admin_super(&state, &headers)?;
    if actor.id == Some(user_id) {
        return Err(AppError::bad_request(
            "cannot_disable_self",
            "current admin user cannot disable itself",
        ));
    }
    let user = disable_admin_user(&state.pool, user_id).await?;
    Ok(Json(AdminUserResponse {
        status: "ok",
        user: AdminUserSummary::from_row(user),
        server_time: now_unix(),
    }))
}

async fn run_schema_migrations(pool: &MySqlPool) -> anyhow::Result<()> {
    ensure_admin_users_table(pool).await?;
    ensure_game_route_game_name_column(pool).await?;
    ensure_game_catalog_table(pool).await?;
    ensure_game_region_table(pool).await?;
    ensure_game_route_business_columns(pool).await?;
    ensure_game_route_region_indexes(pool).await?;
    ensure_connect_intent_region_column(pool).await?;
    ensure_node_remote_tasks_table(pool).await?;
    ensure_node_ssh_credentials_table(pool).await?;
    ensure_node_operation_tasks_table(pool).await?;
    ensure_node_health_alerts_table(pool).await?;
    seed_game_catalog_from_routes(pool).await?;
    Ok(())
}

async fn ensure_admin_users_table(pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS admin_users (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  username VARCHAR(64) NOT NULL,
  display_name VARCHAR(128) NULL,
  password_hash VARCHAR(255) NOT NULL,
  role ENUM('super_admin', 'operator', 'viewer') NOT NULL DEFAULT 'viewer',
  status ENUM('active', 'disabled') NOT NULL DEFAULT 'active',
  last_login_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY uk_admin_username (username),
  INDEX idx_role_status (role, status)
)
"#,
    )
    .execute(pool)
    .await
    .context("failed to create admin_users")?;
    Ok(())
}

async fn ensure_game_route_game_name_column(pool: &MySqlPool) -> anyhow::Result<()> {
    if !mysql_column_exists(pool, "game_route_rules", "game_name")
        .await
        .context("failed to inspect game_route_rules.game_name")?
    {
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

async fn ensure_game_region_table(pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS accel_game_regions (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  game_id BIGINT UNSIGNED NOT NULL,
  region_id BIGINT UNSIGNED NOT NULL,
  name VARCHAR(128) NOT NULL,
  area VARCHAR(32) NULL,
  status ENUM('enabled', 'disabled') NOT NULL DEFAULT 'enabled',
  remark VARCHAR(512) NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY uniq_game_region (game_id, region_id),
  INDEX idx_game_status (game_id, status),
  INDEX idx_area (area)
)
"#,
    )
    .execute(pool)
    .await
    .context("failed to create accel_game_regions")?;
    Ok(())
}

async fn ensure_game_route_business_columns(pool: &MySqlPool) -> anyhow::Result<()> {
    ensure_column(
        pool,
        "game_route_rules",
        "region_id",
        "ALTER TABLE game_route_rules ADD COLUMN region_id BIGINT UNSIGNED NULL AFTER game_name",
    )
    .await?;
    ensure_column(
        pool,
        "game_route_rules",
        "region_name",
        "ALTER TABLE game_route_rules ADD COLUMN region_name VARCHAR(128) NULL AFTER region_id",
    )
    .await?;
    ensure_column(
        pool,
        "game_route_rules",
        "sync_source",
        "ALTER TABLE game_route_rules ADD COLUMN sync_source VARCHAR(32) NULL AFTER status",
    )
    .await?;
    ensure_column(
        pool,
        "game_route_rules",
        "external_id",
        "ALTER TABLE game_route_rules ADD COLUMN external_id VARCHAR(128) NULL AFTER sync_source",
    )
    .await?;
    ensure_index(
        pool,
        "game_route_rules",
        "idx_game_region_status_priority",
        "ALTER TABLE game_route_rules ADD INDEX idx_game_region_status_priority (game_id, region_id, status, priority)",
    )
    .await?;
    ensure_index(
        pool,
        "game_route_rules",
        "idx_route_external",
        "ALTER TABLE game_route_rules ADD UNIQUE KEY idx_route_external (sync_source, external_id)",
    )
    .await?;
    Ok(())
}

async fn ensure_game_route_region_indexes(pool: &MySqlPool) -> anyhow::Result<()> {
    drop_index_if_exists(pool, "game_route_rules", "uniq_game_node_target").await?;
    ensure_index(
        pool,
        "game_route_rules",
        "idx_game_node_region_target",
        "ALTER TABLE game_route_rules ADD INDEX idx_game_node_region_target (game_id, region_id, node_id, target_addr, protocol)",
    )
    .await?;
    Ok(())
}

async fn ensure_connect_intent_region_column(pool: &MySqlPool) -> anyhow::Result<()> {
    ensure_column(
        pool,
        "connect_intents",
        "region_id",
        "ALTER TABLE connect_intents ADD COLUMN region_id BIGINT UNSIGNED NULL AFTER game_id",
    )
    .await?;
    ensure_index(
        pool,
        "connect_intents",
        "idx_game_region_created",
        "ALTER TABLE connect_intents ADD INDEX idx_game_region_created (game_id, region_id, created_at)",
    )
    .await?;
    Ok(())
}

async fn ensure_column(
    pool: &MySqlPool,
    table: &'static str,
    column: &'static str,
    alter_sql: &'static str,
) -> anyhow::Result<()> {
    if !mysql_column_exists(pool, table, column)
        .await
        .with_context(|| format!("failed to inspect {table}.{column}"))?
    {
        sqlx::query(alter_sql)
            .execute(pool)
            .await
            .with_context(|| format!("failed to add {table}.{column}"))?;
    }
    Ok(())
}

async fn ensure_index(
    pool: &MySqlPool,
    table: &'static str,
    index_name: &'static str,
    alter_sql: &'static str,
) -> anyhow::Result<()> {
    let exists = sqlx::query_scalar::<_, String>(
        r#"
SELECT INDEX_NAME
FROM information_schema.STATISTICS
WHERE TABLE_SCHEMA = DATABASE()
  AND TABLE_NAME = ?
  AND INDEX_NAME = ?
LIMIT 1
"#,
    )
    .bind(table)
    .bind(index_name)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to inspect index {table}.{index_name}"))?;

    if exists.is_none() {
        sqlx::query(alter_sql)
            .execute(pool)
            .await
            .with_context(|| format!("failed to add index {table}.{index_name}"))?;
    }
    Ok(())
}

async fn drop_index_if_exists(
    pool: &MySqlPool,
    table: &'static str,
    index_name: &'static str,
) -> anyhow::Result<()> {
    let exists = sqlx::query_scalar::<_, String>(
        r#"
SELECT INDEX_NAME
FROM information_schema.STATISTICS
WHERE TABLE_SCHEMA = DATABASE()
  AND TABLE_NAME = ?
  AND INDEX_NAME = ?
LIMIT 1
"#,
    )
    .bind(table)
    .bind(index_name)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to inspect index {table}.{index_name}"))?;

    if exists.is_some() {
        let sql = format!("ALTER TABLE {table} DROP INDEX {index_name}");
        sqlx::query(&sql)
            .execute(pool)
            .await
            .with_context(|| format!("failed to drop index {table}.{index_name}"))?;
    }
    Ok(())
}

async fn mysql_column_exists(
    pool: &MySqlPool,
    table: &'static str,
    column: &'static str,
) -> anyhow::Result<bool> {
    let exists = sqlx::query_scalar::<_, String>(
        r#"
SELECT COLUMN_NAME
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = DATABASE()
  AND TABLE_NAME = ?
  AND COLUMN_NAME = ?
LIMIT 1
"#,
    )
    .bind(table)
    .bind(column)
    .fetch_optional(pool)
    .await?;

    Ok(exists.is_some())
}

async fn mysql_column_data_type(
    pool: &MySqlPool,
    table: &'static str,
    column: &'static str,
) -> anyhow::Result<Option<String>> {
    let data_type = sqlx::query_scalar::<_, String>(
        r#"
SELECT LOWER(DATA_TYPE)
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = DATABASE()
  AND TABLE_NAME = ?
  AND COLUMN_NAME = ?
LIMIT 1
"#,
    )
    .bind(table)
    .bind(column)
    .fetch_optional(pool)
    .await?;

    Ok(data_type)
}

async fn ensure_game_catalog_table(pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS accel_games (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  game_id BIGINT UNSIGNED NOT NULL,
  name VARCHAR(128) NOT NULL,
  platform ENUM('pc', 'android', 'ios', 'multi') NOT NULL DEFAULT 'pc',
  category VARCHAR(64) NULL,
  icon_url VARCHAR(512) NULL,
  status ENUM('enabled', 'disabled') NOT NULL DEFAULT 'enabled',
  remark VARCHAR(512) NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY uniq_game_id (game_id),
  INDEX idx_status_platform (status, platform),
  INDEX idx_category (category)
)
"#,
    )
    .execute(pool)
    .await
    .context("failed to create accel_games")?;
    Ok(())
}

async fn ensure_node_remote_tasks_table(pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS node_remote_tasks (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  task_type VARCHAR(32) NOT NULL,
  status ENUM('pending', 'running', 'succeeded', 'failed', 'canceled') NOT NULL DEFAULT 'pending',
  message VARCHAR(512) NULL,
  output TEXT NULL,
  error_message VARCHAR(512) NULL,
  requested_by VARCHAR(64) NULL,
  claimed_at TIMESTAMP NULL,
  started_at TIMESTAMP NULL,
  finished_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  INDEX idx_node_status_created (node_id, status, created_at),
  INDEX idx_status_created (status, created_at),
  CONSTRAINT fk_remote_task_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
)
"#,
    )
    .execute(pool)
    .await
    .context("failed to create node_remote_tasks")?;
    Ok(())
}

async fn ensure_node_ssh_credentials_table(pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS node_ssh_credentials (
  node_id BIGINT UNSIGNED PRIMARY KEY,
  host VARCHAR(128) NOT NULL,
  port INT UNSIGNED NOT NULL DEFAULT 22,
  username VARCHAR(64) NOT NULL,
  password_ciphertext TEXT NOT NULL,
  password_nonce VARCHAR(64) NOT NULL,
  auth_status ENUM('untested', 'ok', 'failed') NOT NULL DEFAULT 'untested',
  last_error TEXT NULL,
  last_checked_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  CONSTRAINT fk_ssh_credential_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
)
"#,
    )
    .execute(pool)
    .await
    .context("failed to create node_ssh_credentials")?;
    let last_error_type = mysql_column_data_type(pool, "node_ssh_credentials", "last_error")
        .await
        .context("failed to inspect node_ssh_credentials.last_error")?;
    if !matches!(
        last_error_type.as_deref(),
        Some("text" | "mediumtext" | "longtext")
    ) {
        sqlx::query("ALTER TABLE node_ssh_credentials MODIFY COLUMN last_error TEXT NULL")
            .execute(pool)
            .await
            .context("failed to widen node_ssh_credentials.last_error")?;
    }
    Ok(())
}

async fn ensure_node_operation_tasks_table(pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS node_operation_tasks (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  action VARCHAR(64) NOT NULL,
  executor VARCHAR(32) NOT NULL DEFAULT 'control_ssh',
  status ENUM('running', 'succeeded', 'failed') NOT NULL DEFAULT 'running',
  command_label VARCHAR(128) NOT NULL,
  exit_code INT NULL,
  duration_ms BIGINT UNSIGNED NULL,
  output MEDIUMTEXT NULL,
  error_message TEXT NULL,
  version_check_json JSON NULL,
  started_at TIMESTAMP NULL,
  finished_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  INDEX idx_node_created (node_id, created_at),
  INDEX idx_status_created (status, created_at),
  INDEX idx_action_created (action, created_at),
  CONSTRAINT fk_operation_task_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
)
"#,
    )
    .execute(pool)
    .await
    .context("failed to create node_operation_tasks")?;
    Ok(())
}

async fn ensure_node_health_alerts_table(pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS node_health_alerts (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  alert_key VARCHAR(64) NOT NULL,
  severity ENUM('critical', 'warning') NOT NULL,
  title VARCHAR(128) NOT NULL,
  message TEXT NOT NULL,
  status ENUM('open', 'acknowledged', 'resolved', 'ignored') NOT NULL DEFAULT 'open',
  first_seen_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  last_seen_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  acknowledged_at TIMESTAMP NULL,
  acknowledged_by BIGINT UNSIGNED NULL,
  resolved_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY uk_node_health_alert (node_id, alert_key),
  INDEX idx_health_alert_status (status, severity, last_seen_at),
  INDEX idx_health_alert_node (node_id, status, last_seen_at),
  CONSTRAINT fk_health_alert_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
)
"#,
    )
    .execute(pool)
    .await
    .context("failed to create node_health_alerts")?;
    Ok(())
}

async fn seed_game_catalog_from_routes(pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
INSERT IGNORE INTO accel_games (
  game_id,
  name,
  platform,
  status,
  created_at,
  updated_at
)
SELECT
  game_id,
  COALESCE(NULLIF(MAX(game_name), ''), CONCAT('游戏 ', game_id)),
  'pc',
  'enabled',
  CURRENT_TIMESTAMP,
  CURRENT_TIMESTAMP
FROM game_route_rules
GROUP BY game_id
"#,
    )
    .execute(pool)
    .await
    .context("failed to seed accel_games from game_route_rules")?;
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
    if cli
        .business_sync_token
        .as_deref()
        .is_some_and(|token| token.trim().is_empty())
    {
        bail!("--business-sync-token must not be empty when provided");
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

async fn build_system_diagnostics(state: &AppState) -> AdminSystemDiagnosticsResponse {
    let mut checks = Vec::new();
    let mut counts = AdminSystemDiagnosticCounts::default();

    match sqlx::query("SELECT 1").execute(&state.pool).await {
        Ok(_) => push_system_check(
            &mut checks,
            "database",
            "MySQL 连接",
            "ok",
            "数据库连接正常，控制面可以读写数据。",
            None,
        ),
        Err(error) => {
            error!(error = %error, "system diagnostics database ping failed");
            push_system_check(
                &mut checks,
                "database",
                "MySQL 连接",
                "critical",
                "数据库连接失败，控制面服务会降级或不可用。",
                Some("检查 DATABASE_URL、MySQL 用户授权和 mysql 服务状态。"),
            );
        }
    }

    match system_core_table_count(&state.pool).await {
        Ok(found) if found == SYSTEM_DIAGNOSTIC_CORE_TABLES.len() as u64 => push_system_check(
            &mut checks,
            "schema",
            "核心数据表",
            "ok",
            "节点、游戏、路由、上报和运维任务表都已存在。",
            None,
        ),
        Ok(found) => push_system_check(
            &mut checks,
            "schema",
            "核心数据表",
            "critical",
            format!("只检测到 {found}/6 个核心表，数据库结构不完整。"),
            Some("重新运行控制面安装脚本，或导入 db/schema.sql 后再重启服务。"),
        ),
        Err(error) => {
            error!(error = %error, "system diagnostics schema check failed");
            push_system_check(
                &mut checks,
                "schema",
                "核心数据表",
                "critical",
                "无法检查数据库结构。",
                Some("先修复 MySQL 连接，再查看控制面日志。"),
            );
        }
    }

    counts.nodes_total = system_count(
        &state.pool,
        "SELECT CAST(COUNT(*) AS UNSIGNED) FROM accel_nodes",
    )
    .await
    .unwrap_or_default();
    counts.nodes_online = system_count(
        &state.pool,
        "SELECT CAST(COUNT(*) AS UNSIGNED) FROM accel_nodes WHERE status = 'online'",
    )
    .await
    .unwrap_or_default();
    counts.nodes_reporting = system_count(
        &state.pool,
        "SELECT CAST(COUNT(*) AS UNSIGNED) FROM accel_nodes WHERE last_report_at >= DATE_SUB(NOW(), INTERVAL 120 SECOND)",
    )
    .await
    .unwrap_or_default();
    counts.games_enabled = system_count(
        &state.pool,
        "SELECT CAST(COUNT(*) AS UNSIGNED) FROM accel_games WHERE status = 'enabled'",
    )
    .await
    .unwrap_or_default();
    counts.routes_enabled = system_count(
        &state.pool,
        "SELECT CAST(COUNT(*) AS UNSIGNED) FROM game_route_rules WHERE status = 'enabled'",
    )
    .await
    .unwrap_or_default();
    counts.active_alerts = system_count(
        &state.pool,
        "SELECT CAST(COUNT(*) AS UNSIGNED) FROM node_health_alerts WHERE status IN ('open', 'acknowledged')",
    )
    .await
    .unwrap_or_default();

    if counts.nodes_total == 0 {
        push_system_check(
            &mut checks,
            "nodes",
            "节点上报",
            "warning",
            "还没有节点记录。",
            Some("先在节点管理新增节点，然后用一键部署接入 Linux 节点。"),
        );
    } else if counts.nodes_reporting == 0 {
        push_system_check(
            &mut checks,
            "nodes",
            "节点上报",
            "critical",
            format!(
                "{} 台节点里没有最近 120 秒内上报的节点。",
                counts.nodes_total
            ),
            Some("检查节点服务器 xaccel-node 服务、控制面地址和防火墙。"),
        );
    } else {
        push_system_check(
            &mut checks,
            "nodes",
            "节点上报",
            "ok",
            format!(
                "{} 台节点在线，{} 台最近 120 秒内有上报。",
                counts.nodes_online, counts.nodes_reporting
            ),
            None,
        );
    }

    if counts.routes_enabled == 0 {
        push_system_check(
            &mut checks,
            "routes",
            "游戏路由",
            "warning",
            "当前没有启用中的游戏路由。",
            Some("在游戏路由里给游戏绑定节点和目标地址，否则客户端拿不到可用线路。"),
        );
    } else {
        push_system_check(
            &mut checks,
            "routes",
            "游戏路由",
            "ok",
            format!("已启用 {} 条游戏路由。", counts.routes_enabled),
            None,
        );
    }

    if counts.active_alerts > 0 {
        push_system_check(
            &mut checks,
            "alerts",
            "健康告警",
            "warning",
            format!("还有 {} 条告警需要查看。", counts.active_alerts),
            Some("打开健康告警页面，确认或处理节点异常。"),
        );
    } else {
        push_system_check(
            &mut checks,
            "alerts",
            "健康告警",
            "ok",
            "当前没有待处理健康告警。",
            None,
        );
    }

    if state.public_base_url.is_some() {
        push_system_check(
            &mut checks,
            "public_base_url",
            "公网访问地址",
            "ok",
            "控制面已配置公网地址，节点安装令牌会使用这个地址回连。",
            None,
        );
    } else {
        push_system_check(
            &mut checks,
            "public_base_url",
            "公网访问地址",
            "warning",
            "控制面没有配置 public-base-url，会根据请求 Host 推断地址。",
            Some("生产环境建议安装时传入 --public-base-url。"),
        );
    }

    if state.credential_key.is_some() {
        push_system_check(
            &mut checks,
            "credential_key",
            "SSH 凭据加密",
            "ok",
            "已配置凭据加密密钥，可以保存节点 SSH 密码。",
            None,
        );
    } else {
        push_system_check(
            &mut checks,
            "credential_key",
            "SSH 凭据加密",
            "warning",
            "未配置凭据加密密钥，SSH 密码保存功能不可用。",
            Some("重新运行控制面安装脚本，它会自动生成 XACCEL_CREDENTIAL_KEY。"),
        );
    }

    let status = if checks.iter().any(|item| item.status == "critical") {
        "critical"
    } else if checks.iter().any(|item| item.status == "warning") {
        "warning"
    } else {
        "ok"
    };
    let now = now_unix();
    AdminSystemDiagnosticsResponse {
        status,
        version: VERSION,
        listen_addr: state.listen.to_string(),
        public_base_url: state.public_base_url.clone(),
        generated_at: now,
        counts,
        checks,
        server_time: now,
    }
}

async fn system_count(pool: &MySqlPool, sql: &str) -> Result<u64, sqlx::Error> {
    sqlx::query_scalar::<_, u64>(sql).fetch_one(pool).await
}

async fn system_core_table_count(pool: &MySqlPool) -> Result<u64, sqlx::Error> {
    let mut query = QueryBuilder::<MySql>::new(
        "SELECT CAST(COUNT(*) AS UNSIGNED) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name IN (",
    );
    let mut separated = query.separated(", ");
    for table in SYSTEM_DIAGNOSTIC_CORE_TABLES {
        separated.push_bind(table);
    }
    separated.push_unseparated(")");
    query.build_query_scalar::<u64>().fetch_one(pool).await
}

fn push_system_check(
    checks: &mut Vec<AdminSystemDiagnosticCheck>,
    key: &'static str,
    title: &'static str,
    status: &'static str,
    message: impl Into<String>,
    suggestion: Option<&str>,
) {
    checks.push(AdminSystemDiagnosticCheck {
        key,
        title,
        status,
        message: message.into(),
        suggestion: suggestion.map(ToOwned::to_owned),
    });
}

async fn connect_intent(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ConnectIntentRequest>,
) -> Result<Json<ConnectIntentResponse>, AppError> {
    validate_connect_intent_request(&request)?;
    let response = issue_connect_intent(&state.pool, state.token_ttl_sec, request).await?;
    Ok(Json(response))
}

async fn business_sync_catalog(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<BusinessSyncCatalogRequest>,
) -> Result<Json<BusinessSyncCatalogResponse>, AppError> {
    require_business_sync(&state, &headers)?;
    let catalog = normalize_business_sync_catalog(request)?;
    let response = sync_business_catalog(&state.pool, catalog).await?;
    Ok(Json(response))
}

async fn admin_connectivity_diagnostic(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdminConnectivityDiagnosticRequest>,
) -> Result<Json<AdminConnectivityDiagnosticResponse>, AppError> {
    require_admin(&state, &headers)?;
    validate_connectivity_diagnostic_request(&request)?;
    let response = run_connectivity_diagnostic(&state, request).await?;
    Ok(Json(response))
}

async fn admin_system_diagnostics(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AdminSystemDiagnosticsResponse>, AppError> {
    require_admin(&state, &headers)?;
    Ok(Json(build_system_diagnostics(&state).await))
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

async fn node_tasks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<NodeTasksResponse>, AppError> {
    let node_id =
        authenticate_node_request(&state.pool, &headers, "GET", NODE_TASKS_PATH, b"").await?;
    let tasks = claim_next_node_task(&state.pool, node_id)
        .await?
        .into_iter()
        .map(NodeTaskItem::from_row)
        .collect::<Vec<_>>();

    Ok(Json(NodeTasksResponse {
        status: "ok",
        node_id,
        tasks,
        server_time: now_unix(),
    }))
}

async fn node_task_result(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<u64>,
    body: Bytes,
) -> Result<Json<NodeTaskResultResponse>, AppError> {
    let path = format!("/api/node/v1/tasks/{task_id}/result");
    let node_id = authenticate_node_request(&state.pool, &headers, "POST", &path, &body).await?;
    let request = serde_json::from_slice::<NodeTaskResultRequest>(&body).map_err(|error| {
        AppError::bad_request(
            "invalid_task_result",
            format!("invalid node task result body: {error}"),
        )
    })?;
    validate_node_task_result(node_id, task_id, &request)?;
    let status = validate_node_task_result_status(&request.status)?;
    update_node_task_result(&state.pool, node_id, task_id, status, &request).await?;

    Ok(Json(NodeTaskResultResponse {
        status: "ok",
        node_id,
        task_id,
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
    let audit_logs = select_admin_audit_logs(
        &state.pool,
        Some(node_id),
        None,
        clamp_limit(query.limit, 20, 100),
    )
    .await?
    .into_iter()
    .map(AdminAuditLogDetail::from_row)
    .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(AdminNodeDetailResponse {
        node: AdminNodeSummary::from_row(node),
        recent_reports: reports,
        recent_tasks: select_admin_node_tasks(
            &state.pool,
            node_id,
            clamp_limit(query.limit, 20, 100),
        )
        .await?
        .into_iter()
        .map(AdminNodeTaskSummary::from_row)
        .collect(),
        recent_audit_logs: audit_logs,
        server_time: now_unix(),
    }))
}

async fn admin_create_node(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdminCreateNodeRequest>,
) -> Result<Json<AdminCreateNodeResponse>, AppError> {
    require_admin_write(&state, &headers)?;
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
    require_admin_write(&state, &headers)?;
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

async fn admin_delete_node(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
) -> Result<Json<AdminDeleteNodeResponse>, AppError> {
    require_admin_super(&state, &headers)?;
    let deleted = delete_admin_node(&state.pool, node_id).await?;

    Ok(Json(AdminDeleteNodeResponse {
        status: "ok",
        node_id,
        deleted,
        server_time: now_unix(),
    }))
}

async fn admin_update_node_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Json(request): Json<AdminUpdateNodeStatusRequest>,
) -> Result<Json<AdminUpdateNodeStatusResponse>, AppError> {
    let actor = require_admin_write(&state, &headers)?;
    let next_status = validate_admin_node_status(&request.status)?;
    let reason = request
        .reason
        .map(|reason| reason.trim().to_string())
        .filter(|reason| !reason.is_empty());
    let previous_status =
        update_admin_node_status(&state.pool, node_id, next_status, reason.as_deref(), &actor)
            .await?;

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
    require_admin_write(&state, &headers)?;
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

async fn admin_deploy_node(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Json(request): Json<AdminDeployNodeRequest>,
) -> Result<Json<AdminDeployNodeResponse>, AppError> {
    let actor = require_admin_write(&state, &headers)?;
    let before_node = select_admin_node(&state.pool, node_id)
        .await?
        .ok_or_else(|| AppError::not_found("node_not_found", "node does not exist"))?;
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
    let normalized = normalize_ssh_credential_request(&before_node, request.ssh)?;
    let password = normalized.password.clone();
    let save_credential = request.save_credential.unwrap_or(true);
    let credential = if save_credential {
        let key = credential_key(&state)?;
        let (password_ciphertext, password_nonce) =
            encrypt_credential_secret(&key, &normalized.password)?;
        upsert_ssh_credential(
            &state.pool,
            node_id,
            &normalized,
            &password_ciphertext,
            &password_nonce,
            &actor,
        )
        .await?
    } else {
        SshCredentialRow {
            host: normalized.host.clone(),
            port: u32::from(normalized.port),
            username: normalized.username.clone(),
            password_ciphertext: String::new(),
            password_nonce: String::new(),
            auth_status: "untested".to_string(),
            last_error: None,
            last_checked_at: None,
        }
    };
    let is_root = credential.username == "root";
    let expires_at = now_unix() + expires_in_sec;
    let bootstrap_token = create_bootstrap_token(&state.pool, node_id, None, expires_at).await?;
    let bootstrap_url = format!("{public_base_url}{NODE_BOOTSTRAP_PATH}");
    let plan = SshActionPlan {
        command_label: "一键部署 / 升级节点".to_string(),
        remote_command: build_remote_bootstrap_install_command(
            &install_url,
            &bootstrap_url,
            &bootstrap_token,
            !is_root,
            request.enable_control_plane.unwrap_or(true),
            channel.as_deref(),
        ),
        send_password_to_stdin: !is_root,
    };
    let command_label = plan.command_label.clone();
    let operation_task =
        insert_operation_task(&state.pool, node_id, "deploy_node", &command_label).await?;
    let started = Instant::now();
    let result = run_ssh_command(&credential, &password, &plan).await;
    let duration_ms = started.elapsed().as_millis();
    let duration_ms_u64 = duration_ms_to_u64(duration_ms);

    match result {
        Ok(output) => {
            let version_check = wait_for_upgrade_version_check(&state.pool, node_id, &before_node)
                .await
                .ok();
            let task = finish_operation_task(
                &state.pool,
                operation_task.id,
                "succeeded",
                output.exit_code,
                duration_ms_u64,
                Some(&output.combined),
                None,
                version_check.as_ref(),
            )
            .await?;
            if save_credential {
                update_ssh_credential_status(&state.pool, node_id, "ok", None).await?;
            }
            insert_node_audit_log(
                &state.pool,
                node_id,
                actor.audit_actor_type(),
                actor.id,
                "node.deploy",
                serde_json::json!({
                    "operation_task_id": operation_task.id,
                    "command_label": command_label,
                    "ssh_host": credential.host,
                    "ssh_port": credential.port,
                    "ssh_username": credential.username,
                    "duration_ms": duration_ms,
                    "version_check": &version_check,
                    "credential_saved": save_credential,
                }),
            )
            .await?;

            Ok(Json(AdminDeployNodeResponse {
                status: "ok",
                node_id,
                action: "deploy_node".to_string(),
                command_label,
                exit_code: output.exit_code,
                output: output.combined,
                duration_ms,
                version_check,
                task: AdminOperationTaskSummary::from_row(task),
                credential_saved: save_credential,
                server_time: now_unix(),
            }))
        }
        Err(error) => {
            let message = trim_for_log(&error.message, 512);
            let task = finish_operation_task(
                &state.pool,
                operation_task.id,
                "failed",
                error.exit_code,
                duration_ms_u64,
                None,
                Some(&message),
                None,
            )
            .await?;
            if save_credential {
                update_ssh_credential_status(&state.pool, node_id, "failed", Some(&message))
                    .await?;
            }
            insert_node_audit_log(
                &state.pool,
                node_id,
                actor.audit_actor_type(),
                actor.id,
                "node.deploy_failed",
                serde_json::json!({
                    "operation_task_id": operation_task.id,
                    "command_label": command_label,
                    "ssh_host": credential.host,
                    "ssh_port": credential.port,
                    "ssh_username": credential.username,
                    "exit_code": error.exit_code,
                    "error": message,
                    "duration_ms": duration_ms,
                    "credential_saved": save_credential,
                }),
            )
            .await?;

            Ok(Json(AdminDeployNodeResponse {
                status: "failed",
                node_id,
                action: "deploy_node".to_string(),
                command_label,
                exit_code: error.exit_code,
                output: message,
                duration_ms,
                version_check: None,
                task: AdminOperationTaskSummary::from_row(task),
                credential_saved: save_credential,
                server_time: now_unix(),
            }))
        }
    }
}

async fn admin_create_node_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Json(request): Json<AdminCreateNodeTaskRequest>,
) -> Result<Json<AdminCreateNodeTaskResponse>, AppError> {
    let actor = require_admin_write(&state, &headers)?;
    let task_type = validate_admin_node_task_type(&request.task_type)?;
    let message = request
        .message
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(512).collect::<String>());
    let task =
        insert_admin_node_task(&state.pool, node_id, task_type, message.as_deref(), &actor).await?;

    Ok(Json(AdminCreateNodeTaskResponse {
        status: "ok",
        node_id,
        task: AdminNodeTaskSummary::from_row(task),
        server_time: now_unix(),
    }))
}

async fn admin_list_operation_tasks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AdminListOperationTasksQuery>,
) -> Result<Json<AdminListOperationTasksResponse>, AppError> {
    require_admin(&state, &headers)?;
    let status = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(validate_operation_task_status)
        .transpose()?;
    let rows = select_operation_tasks(
        &state.pool,
        query.node_id,
        status,
        clamp_limit(query.limit, 100, 300),
    )
    .await?;
    let tasks = rows
        .into_iter()
        .map(AdminOperationTaskSummary::from_row)
        .collect::<Vec<_>>();

    Ok(Json(AdminListOperationTasksResponse {
        total: tasks.len(),
        tasks,
        server_time: now_unix(),
    }))
}

async fn admin_get_operation_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<u64>,
) -> Result<Json<AdminOperationTaskResponse>, AppError> {
    require_admin(&state, &headers)?;
    let task = select_operation_task(&state.pool, task_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found("operation_task_not_found", "operation task does not exist")
        })?;

    Ok(Json(AdminOperationTaskResponse {
        task: AdminOperationTaskSummary::from_row(task),
        server_time: now_unix(),
    }))
}

async fn admin_list_health_alerts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AdminListHealthAlertsQuery>,
) -> Result<Json<AdminListHealthAlertsResponse>, AppError> {
    require_admin(&state, &headers)?;
    reconcile_node_health_alerts(&state.pool).await?;
    let status = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let severity = query
        .severity
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let alerts = select_health_alerts(
        &state.pool,
        query.node_id,
        status,
        severity,
        clamp_limit(query.limit, 200, 500),
    )
    .await?
    .into_iter()
    .map(AdminHealthAlertSummary::from_row)
    .collect::<Vec<_>>();
    let summary = health_alert_counts(&alerts);

    Ok(Json(AdminListHealthAlertsResponse {
        total: alerts.len(),
        alerts,
        summary,
        server_time: now_unix(),
    }))
}

async fn admin_update_health_alert(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(alert_id): Path<u64>,
    Json(request): Json<AdminUpdateHealthAlertRequest>,
) -> Result<Json<AdminHealthAlertResponse>, AppError> {
    let actor = require_admin_write(&state, &headers)?;
    let status = validate_health_alert_status(&request.status)?;
    let alert = update_health_alert_status(&state.pool, alert_id, status, &actor).await?;
    insert_node_audit_log(
        &state.pool,
        alert.node_id,
        actor.audit_actor_type(),
        actor.id,
        "node.health_alert.update",
        serde_json::json!({
            "alert_id": alert.id,
            "alert_key": alert.alert_key,
            "severity": alert.severity,
            "status": alert.status,
            "title": alert.title,
        }),
    )
    .await?;

    Ok(Json(AdminHealthAlertResponse {
        status: "ok",
        alert: AdminHealthAlertSummary::from_row(alert),
        server_time: now_unix(),
    }))
}

async fn admin_list_audit_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AdminListAuditLogsQuery>,
) -> Result<Json<AdminListAuditLogsResponse>, AppError> {
    require_admin(&state, &headers)?;
    let action = query
        .action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let logs = select_admin_audit_logs(
        &state.pool,
        query.node_id,
        action,
        clamp_limit(query.limit, 100, 500),
    )
    .await?
    .into_iter()
    .map(AdminAuditLogDetail::from_row)
    .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(AdminListAuditLogsResponse {
        total: logs.len(),
        logs,
        server_time: now_unix(),
    }))
}

async fn admin_upsert_ssh_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Json(request): Json<AdminSshCredentialRequest>,
) -> Result<Json<AdminSshCredentialResponse>, AppError> {
    let actor = require_admin_write(&state, &headers)?;
    let node = select_admin_node(&state.pool, node_id)
        .await?
        .ok_or_else(|| AppError::not_found("node_not_found", "node does not exist"))?;
    let normalized = normalize_ssh_credential_request(&node, request)?;
    let key = credential_key(&state)?;
    let (password_ciphertext, password_nonce) =
        encrypt_credential_secret(&key, &normalized.password)?;
    let row = upsert_ssh_credential(
        &state.pool,
        node_id,
        &normalized,
        &password_ciphertext,
        &password_nonce,
        &actor,
    )
    .await?;

    Ok(Json(AdminSshCredentialResponse {
        status: "ok",
        node_id,
        credential: AdminSshCredentialSummary::from_ssh_row(Some(&row)),
        server_time: now_unix(),
    }))
}

async fn admin_delete_ssh_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
) -> Result<Json<AdminDeleteSshCredentialResponse>, AppError> {
    let actor = require_admin_write(&state, &headers)?;
    let deleted = delete_ssh_credential(&state.pool, node_id, &actor).await?;

    Ok(Json(AdminDeleteSshCredentialResponse {
        status: "ok",
        node_id,
        deleted,
        server_time: now_unix(),
    }))
}

async fn admin_run_ssh_action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
    Json(request): Json<AdminSshActionRequest>,
) -> Result<Json<AdminSshActionResponse>, AppError> {
    let actor = require_admin_write(&state, &headers)?;
    let action = validate_ssh_action(&request.action)?;
    let before_node = select_admin_node(&state.pool, node_id)
        .await?
        .ok_or_else(|| AppError::not_found("node_not_found", "node does not exist"))?;
    let credential = select_ssh_credential(&state.pool, node_id)
        .await?
        .ok_or_else(|| {
            AppError::bad_request(
                "ssh_not_configured",
                "node ssh credential is not configured",
            )
        })?;
    let key = credential_key(&state)?;
    let password = decrypt_credential_secret(
        &key,
        &credential.password_ciphertext,
        &credential.password_nonce,
    )?;
    let public_base_url = if action == "upgrade_node" {
        Some(resolve_public_base_url(&state, &headers)?)
    } else {
        None
    };
    let plan = build_ssh_action_plan(
        &state.pool,
        node_id,
        action,
        &credential,
        public_base_url.as_deref(),
    )
    .await?;
    let command_label = plan.command_label.clone();
    let operation_task =
        insert_operation_task(&state.pool, node_id, action, &command_label).await?;
    let started = Instant::now();
    let result = run_ssh_command(&credential, &password, &plan).await;
    let duration_ms = started.elapsed().as_millis();
    let duration_ms_u64 = duration_ms_to_u64(duration_ms);

    match result {
        Ok(output) => {
            let version_check = if action == "upgrade_node" {
                Some(wait_for_upgrade_version_check(&state.pool, node_id, &before_node).await?)
            } else {
                None
            };
            let task = finish_operation_task(
                &state.pool,
                operation_task.id,
                "succeeded",
                output.exit_code,
                duration_ms_u64,
                Some(&output.combined),
                None,
                version_check.as_ref(),
            )
            .await?;
            update_ssh_credential_status(&state.pool, node_id, "ok", None).await?;
            insert_node_audit_log(
                &state.pool,
                node_id,
                actor.audit_actor_type(),
                actor.id,
                "node.ssh_action",
                serde_json::json!({
                    "operation_task_id": operation_task.id,
                    "action": action,
                    "command_label": command_label,
                    "exit_code": output.exit_code,
                    "duration_ms": duration_ms,
                    "version_check": &version_check,
                }),
            )
            .await?;

            Ok(Json(AdminSshActionResponse {
                status: "ok",
                node_id,
                action: action.to_string(),
                command_label,
                exit_code: output.exit_code,
                output: output.combined,
                duration_ms,
                version_check,
                task: Some(AdminOperationTaskSummary::from_row(task)),
                server_time: now_unix(),
            }))
        }
        Err(error) => {
            let message = trim_for_log(&error.message, 512);
            let task = finish_operation_task(
                &state.pool,
                operation_task.id,
                "failed",
                error.exit_code,
                duration_ms_u64,
                None,
                Some(&message),
                None,
            )
            .await?;
            update_ssh_credential_status(&state.pool, node_id, "failed", Some(&message)).await?;
            insert_node_audit_log(
                &state.pool,
                node_id,
                actor.audit_actor_type(),
                actor.id,
                "node.ssh_action_failed",
                serde_json::json!({
                    "operation_task_id": operation_task.id,
                    "action": action,
                    "command_label": command_label,
                    "exit_code": error.exit_code,
                    "error": message,
                    "duration_ms": duration_ms,
                }),
            )
            .await?;
            Ok(Json(AdminSshActionResponse {
                status: "failed",
                node_id,
                action: action.to_string(),
                command_label,
                exit_code: error.exit_code,
                output: message,
                duration_ms,
                version_check: None,
                task: Some(AdminOperationTaskSummary::from_row(task)),
                server_time: now_unix(),
            }))
        }
    }
}

async fn admin_list_games(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AdminListGamesQuery>,
) -> Result<Json<AdminListGamesResponse>, AppError> {
    require_admin(&state, &headers)?;
    validate_game_query(&query)?;
    let limit = clamp_limit(query.limit, 200, 500);
    let rows = select_admin_games(&state.pool, &query, limit).await?;
    let games = rows
        .into_iter()
        .map(AdminGameSummary::from_row)
        .collect::<Vec<_>>();

    Ok(Json(AdminListGamesResponse {
        total: games.len(),
        games,
        server_time: now_unix(),
    }))
}

async fn admin_create_game(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdminGameRequest>,
) -> Result<Json<AdminGameResponse>, AppError> {
    require_admin_write(&state, &headers)?;
    let game_id = insert_admin_game(&state.pool, request).await?;
    let game = select_admin_game(&state.pool, game_id)
        .await?
        .ok_or_else(|| AppError::not_found("game_not_found", "created game does not exist"))?;

    Ok(Json(AdminGameResponse {
        status: "ok",
        game: AdminGameSummary::from_row(game),
        server_time: now_unix(),
    }))
}

async fn admin_update_game(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(game_id): Path<u64>,
    Json(request): Json<AdminUpdateGameRequest>,
) -> Result<Json<AdminGameResponse>, AppError> {
    require_admin_write(&state, &headers)?;
    update_admin_game(&state.pool, game_id, request).await?;
    let game = select_admin_game(&state.pool, game_id)
        .await?
        .ok_or_else(|| AppError::not_found("game_not_found", "updated game does not exist"))?;

    Ok(Json(AdminGameResponse {
        status: "ok",
        game: AdminGameSummary::from_row(game),
        server_time: now_unix(),
    }))
}

async fn admin_delete_game(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(game_id): Path<u64>,
) -> Result<Json<AdminDeleteGameResponse>, AppError> {
    require_admin_super(&state, &headers)?;
    if game_id == 0 {
        return Err(AppError::bad_request(
            "invalid_game",
            "game_id must be positive",
        ));
    }
    let deleted = delete_admin_game(&state.pool, game_id).await?;

    Ok(Json(AdminDeleteGameResponse {
        status: "ok",
        game_id,
        deleted,
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
    require_admin_write(&state, &headers)?;
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
    require_admin_write(&state, &headers)?;
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
    require_admin_super(&state, &headers)?;
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

async fn run_connectivity_diagnostic(
    state: &AppState,
    request: AdminConnectivityDiagnosticRequest,
) -> Result<AdminConnectivityDiagnosticResponse, AppError> {
    let timeout_sec = request
        .timeout_sec
        .unwrap_or(DEFAULT_DIAGNOSTIC_TIMEOUT_SEC);
    let response_timeout_ms = request
        .response_timeout_ms
        .unwrap_or(DEFAULT_DIAGNOSTIC_RESPONSE_TIMEOUT_MS);
    let payload = request
        .payload
        .clone()
        .unwrap_or_else(|| DEFAULT_DIAGNOSTIC_PAYLOAD.to_string());
    let candidate_index = request.candidate_index.unwrap_or(0);
    let connect_request = ConnectIntentRequest {
        user_id: request.user_id,
        device_id: request.device_id.clone(),
        game_id: request.game_id,
        region_id: request.region_id,
        platform: request.platform.clone(),
        client_isp: request.client_isp.clone(),
        client_ip: request.client_ip.clone(),
        bandwidth_quality: request.bandwidth_quality.clone(),
    };
    let connect_intent =
        issue_connect_intent(&state.pool, state.token_ttl_sec, connect_request).await?;
    let candidate = connect_intent
        .candidates
        .get(candidate_index)
        .ok_or_else(|| {
            AppError::bad_request(
                "invalid_candidate_index",
                "candidate_index is outside the returned candidate list",
            )
        })?;

    let mut node = DiagnosticNodeSummary {
        node_id: candidate.node_id,
        node_version: None,
        address: format!("{}:{}", candidate.host, candidate.port),
        area: candidate.area.clone(),
        tag: candidate.tag.clone(),
        transports: candidate
            .transports
            .iter()
            .map(|transport| (*transport).to_string())
            .collect(),
        bandwidth_quality: candidate.bandwidth_quality.clone(),
        route: candidate.route.clone(),
        scheduler: candidate.scheduler.clone(),
    };
    let node_addr = match node.address.parse::<SocketAddr>() {
        Ok(addr) => addr,
        Err(error) => {
            return Ok(failed_diagnostic_response(
                connect_intent,
                candidate_index,
                node,
                "probe",
                "invalid_node_address",
                format!("selected node address is invalid: {error}"),
            ));
        }
    };
    if !candidate
        .transports
        .iter()
        .any(|transport| transport.eq_ignore_ascii_case("udp"))
    {
        return Ok(failed_diagnostic_response(
            connect_intent,
            candidate_index,
            node,
            "probe",
            "udp_not_supported",
            "selected node does not advertise UDP transport",
        ));
    }

    let deadline = Duration::from_secs(timeout_sec);
    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(socket) => socket,
        Err(error) => {
            return Ok(failed_diagnostic_response(
                connect_intent,
                candidate_index,
                node,
                "probe",
                "udp_bind_failed",
                format!("failed to bind local UDP socket: {error}"),
            ));
        }
    };
    if let Err(error) = socket.connect(node_addr).await {
        return Ok(failed_diagnostic_response(
            connect_intent,
            candidate_index,
            node,
            "probe",
            "udp_connect_failed",
            format!("failed to connect UDP socket to node: {error}"),
        ));
    }

    let probe_request = NodeProbeRequest {
        message_type: "probe",
        protocol: PROTOCOL_VERSION,
        client_nonce: format!("diag-probe-{}-{}", request.user_id, now_unix()),
        user_id: request.user_id,
        device_id: request.device_id.clone(),
        game_id: request.game_id,
        transport: "udp",
        token: candidate.credential.token.clone(),
    };
    let probe_timer = Instant::now();
    let probe_value = match send_node_json_udp(&socket, &probe_request, deadline).await {
        Ok(value) => value,
        Err(error) => {
            return Ok(failed_diagnostic_response(
                connect_intent,
                candidate_index,
                node,
                "probe",
                "udp_probe_failed",
                error.to_string(),
            ));
        }
    };
    if let Some(error) = node_response_error(&probe_value, "probe") {
        return Ok(diagnostic_response(
            "failed",
            connect_intent,
            candidate_index,
            node,
            None,
            None,
            Some(error),
        ));
    }
    let probe_response: NodeProbeResponse = match serde_json::from_value(probe_value) {
        Ok(response) => response,
        Err(error) => {
            return Ok(failed_diagnostic_response(
                connect_intent,
                candidate_index,
                node,
                "probe",
                "invalid_probe_response",
                format!("failed to decode probe response: {error}"),
            ));
        }
    };
    if probe_response.message_type != "probe.ok" {
        return Ok(failed_diagnostic_response(
            connect_intent,
            candidate_index,
            node,
            "probe",
            "unexpected_probe_response",
            format!(
                "unexpected probe response type: {}",
                probe_response.message_type
            ),
        ));
    }
    if probe_response
        .node_id
        .is_some_and(|node_id| node_id != node.node_id)
    {
        return Ok(failed_diagnostic_response(
            connect_intent,
            candidate_index,
            node,
            "probe",
            "node_id_mismatch",
            "probe response node_id does not match selected candidate",
        ));
    }

    node.node_version = Some(probe_response.node_version.clone());
    let probe = DiagnosticProbeSummary {
        latency_ms: probe_timer.elapsed().as_millis(),
        transport: probe_response.transport,
        session_id: probe_response.session.session_id.clone(),
        ttl_sec: probe_response.session.ttl_sec,
        intent_id: probe_response.session.intent_id,
        route_target_addr: probe_response.session.route_target_addr,
        credential_valid: probe_response.session.credential_valid,
        credential_expires_at: probe_response.session.credential_expires_at,
    };

    if request.skip_session_data.unwrap_or(false) {
        return Ok(diagnostic_response(
            "ok",
            connect_intent,
            candidate_index,
            node,
            Some(probe),
            None,
            None,
        ));
    }

    let session_result = run_diagnostic_session_data(
        &socket,
        request.user_id,
        &probe.session_id,
        &payload,
        response_timeout_ms,
        deadline,
    )
    .await;
    let session_data = match session_result {
        Ok(session_data) => session_data,
        Err(error) => {
            return Ok(diagnostic_response(
                "failed",
                connect_intent,
                candidate_index,
                node,
                Some(probe),
                None,
                Some(error),
            ));
        }
    };
    if session_data.status != "forwarded" {
        let status = session_data.status.clone();
        return Ok(diagnostic_response(
            "failed",
            connect_intent,
            candidate_index,
            node,
            Some(probe),
            Some(session_data),
            Some(DiagnosticStepError {
                step: "session.data",
                code: status.clone(),
                message: format!("upstream relay did not complete successfully: {status}"),
            }),
        ));
    }

    Ok(diagnostic_response(
        "ok",
        connect_intent,
        candidate_index,
        node,
        Some(probe),
        Some(session_data),
        None,
    ))
}

async fn run_diagnostic_session_data(
    socket: &UdpSocket,
    user_id: u64,
    session_id: &str,
    payload: &str,
    response_timeout_ms: u64,
    deadline: Duration,
) -> Result<DiagnosticSessionDataSummary, DiagnosticStepError> {
    let request = NodeSessionDataRequest {
        message_type: "session.data",
        protocol: PROTOCOL_VERSION,
        session_id: session_id.to_string(),
        client_nonce: format!("diag-data-{}-{}", user_id, now_unix()),
        payload: BASE64.encode(payload.as_bytes()),
        response_timeout_ms,
    };
    let timer = Instant::now();
    let response_value = send_node_json_udp(socket, &request, deadline)
        .await
        .map_err(|error| DiagnosticStepError {
            step: "session.data",
            code: "udp_session_data_failed".to_string(),
            message: error.to_string(),
        })?;
    if let Some(error) = node_response_error(&response_value, "session.data") {
        return Err(error);
    }
    let response: NodeSessionDataResponse =
        serde_json::from_value(response_value).map_err(|error| DiagnosticStepError {
            step: "session.data",
            code: "invalid_session_data_response".to_string(),
            message: format!("failed to decode session.data response: {error}"),
        })?;
    if response.message_type != "session.data.ok" {
        return Err(DiagnosticStepError {
            step: "session.data",
            code: "unexpected_session_data_response".to_string(),
            message: format!(
                "unexpected session.data response type: {}",
                response.message_type
            ),
        });
    }

    Ok(DiagnosticSessionDataSummary {
        latency_ms: timer.elapsed().as_millis(),
        status: response.status,
        request_payload_bytes: response.request_payload_bytes,
        response_payload_bytes: response.payload_bytes,
        response_payload_text: decode_payload_text(&response.payload),
        response_payload_base64: response.payload,
        target_addr: response.target.map(|target| target.address),
        relay: response.relay.map(|relay| DiagnosticRelaySummary {
            mode: relay.mode,
            timeout_ms: relay.timeout_ms,
            timed_out: relay.timed_out,
            upstream_tx_bytes: relay.upstream_tx_bytes,
            upstream_rx_bytes: relay.upstream_rx_bytes,
        }),
    })
}

async fn send_node_json_udp<T: Serialize>(
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

    let mut buf = vec![0_u8; UDP_BUFFER_BYTES];
    let size = timeout(deadline, socket.recv(&mut buf))
        .await
        .context("timed out waiting for UDP response")?
        .context("failed to receive UDP response")?;
    serde_json::from_slice(&buf[..size]).context("failed to decode UDP JSON response")
}

fn node_response_error(value: &Value, step: &'static str) -> Option<DiagnosticStepError> {
    let Some(message_type) = value.get("type").and_then(Value::as_str) else {
        return Some(DiagnosticStepError {
            step,
            code: "missing_response_type".to_string(),
            message: format!("{step} response is missing type"),
        });
    };
    if !message_type.ends_with(".error") {
        return None;
    }
    match serde_json::from_value::<NodeErrorResponse>(value.clone()) {
        Ok(error) => Some(DiagnosticStepError {
            step,
            code: error.error.code,
            message: error.error.message,
        }),
        Err(error) => Some(DiagnosticStepError {
            step,
            code: "invalid_error_response".to_string(),
            message: format!("failed to decode {step} error response: {error}"),
        }),
    }
}

fn diagnostic_response(
    status: &'static str,
    connect_intent: ConnectIntentResponse,
    selected_candidate_index: usize,
    node: DiagnosticNodeSummary,
    probe: Option<DiagnosticProbeSummary>,
    session_data: Option<DiagnosticSessionDataSummary>,
    error: Option<DiagnosticStepError>,
) -> AdminConnectivityDiagnosticResponse {
    AdminConnectivityDiagnosticResponse {
        status,
        version: VERSION,
        server_time: now_unix(),
        connect_intent,
        selected_candidate_index,
        node,
        probe,
        session_data,
        error,
    }
}

fn failed_diagnostic_response(
    connect_intent: ConnectIntentResponse,
    selected_candidate_index: usize,
    node: DiagnosticNodeSummary,
    step: &'static str,
    code: impl Into<String>,
    message: impl Into<String>,
) -> AdminConnectivityDiagnosticResponse {
    diagnostic_response(
        "failed",
        connect_intent,
        selected_candidate_index,
        node,
        None,
        None,
        Some(DiagnosticStepError {
            step,
            code: code.into(),
            message: message.into(),
        }),
    )
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
    let scheduler = CandidateSchedulerInfo::from_candidate_row(&row, issued_at);
    let intent_id = format!(
        "intent-{}-{}-{}-{}",
        request.user_id, request.game_id, issued_at, row.node_id
    );
    let route = ClientRouteClaims {
        target_addr: row.target_addr.clone(),
        protocol: row.protocol.clone(),
        region_id: row.region_id,
        region_name: row.region_name.clone(),
    };
    let claims = ClientTokenClaims {
        node_id: row.node_id,
        user_id: request.user_id,
        device_id: request.device_id.clone(),
        game_id: request.game_id,
        region_id: request.region_id,
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
        region_id: request.region_id,
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
            scheduler,
        }],
    })
}

async fn select_candidate(
    pool: &MySqlPool,
    request: &ConnectIntentRequest,
    requested_quality: &str,
) -> Result<Option<CandidateRow>, AppError> {
    let mut builder = QueryBuilder::<MySql>::new(
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
  r.protocol,
  r.region_id,
  r.region_name,
  r.priority AS route_priority,
  lr.active_sessions AS latest_active_sessions,
  lr.udp_sessions AS latest_udp_sessions,
  lr.tcp_sessions AS latest_tcp_sessions,
  CAST(UNIX_TIMESTAMP(lr.reported_at) AS UNSIGNED) AS latest_reported_at
FROM game_route_rules r
JOIN accel_nodes n ON n.id = r.node_id
LEFT JOIN node_runtime_reports lr
  ON lr.id = (
    SELECT nr.id
    FROM node_runtime_reports nr
    WHERE nr.node_id = n.id
    ORDER BY nr.reported_at DESC, nr.id DESC
    LIMIT 1
  )
WHERE r.game_id =
"#,
    );
    builder.push_bind(request.game_id);
    builder.push(
        r#"
  AND r.status = 'enabled'
  AND r.protocol = 'udp'
  AND n.status = 'online'
  AND n.disable_quic = 0
  AND n.node_secret IS NOT NULL
  AND n.node_secret <> ''
"#,
    );
    if let Some(region_id) = request.region_id {
        builder.push(" AND (r.region_id = ");
        builder.push_bind(region_id);
        builder.push(" OR r.region_id IS NULL)");
    } else {
        builder.push(" AND r.region_id IS NULL");
    }
    builder.push(
        r#"
ORDER BY
"#,
    );
    if let Some(region_id) = request.region_id {
        builder.push("  CASE WHEN r.region_id = ");
        builder.push_bind(region_id);
        builder.push(" THEN 0 WHEN r.region_id IS NULL THEN 1 ELSE 2 END,");
    }
    builder.push(
        r#"
  CASE WHEN n.bandwidth_quality =
"#,
    );
    builder.push_bind(requested_quality);
    builder.push(
        r#"
  THEN 0 ELSE 1 END,
  CASE
    WHEN lr.reported_at IS NULL THEN 1
    WHEN TIMESTAMPDIFF(SECOND, lr.reported_at, CURRENT_TIMESTAMP) > 90 THEN 1
    ELSE 0
  END ASC,
  r.priority ASC,
  COALESCE(lr.active_sessions, 0) ASC,
  COALESCE(lr.udp_sessions, 0) ASC,
  n.last_seen_at DESC,
  n.id ASC
LIMIT 1
"#,
    );

    builder
        .build_query_as::<CandidateRow>()
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
  region_id,
  node_id,
  target_addr,
  protocol,
  client_ip,
  client_isp,
  platform,
  bandwidth_quality,
  expires_at,
  created_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, FROM_UNIXTIME(?), CURRENT_TIMESTAMP)
"#,
    )
    .bind(intent_id)
    .bind(request.user_id)
    .bind(&request.device_id)
    .bind(request.game_id)
    .bind(request.region_id)
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
  CAST(UNIX_TIMESTAMP(lr.reported_at) AS UNSIGNED) AS latest_reported_at,
  CAST(lr.raw_json AS CHAR) AS latest_report_raw_json,
  sc.host AS ssh_host,
  sc.port AS ssh_port,
  sc.username AS ssh_username,
  sc.auth_status AS ssh_auth_status,
  sc.last_error AS ssh_last_error,
  CAST(UNIX_TIMESTAMP(sc.last_checked_at) AS UNSIGNED) AS ssh_last_checked_at
FROM accel_nodes n
LEFT JOIN node_runtime_reports lr
  ON lr.id = (
    SELECT r.id
    FROM node_runtime_reports r
    WHERE r.node_id = n.id
    ORDER BY r.id DESC
    LIMIT 1
  )
LEFT JOIN node_ssh_credentials sc ON sc.node_id = n.id
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
  CAST(UNIX_TIMESTAMP(lr.reported_at) AS UNSIGNED) AS latest_reported_at,
  CAST(lr.raw_json AS CHAR) AS latest_report_raw_json,
  sc.host AS ssh_host,
  sc.port AS ssh_port,
  sc.username AS ssh_username,
  sc.auth_status AS ssh_auth_status,
  sc.last_error AS ssh_last_error,
  CAST(UNIX_TIMESTAMP(sc.last_checked_at) AS UNSIGNED) AS ssh_last_checked_at
FROM accel_nodes n
LEFT JOIN node_runtime_reports lr
  ON lr.id = (
    SELECT r.id
    FROM node_runtime_reports r
    WHERE r.node_id = n.id
    ORDER BY r.id DESC
    LIMIT 1
  )
LEFT JOIN node_ssh_credentials sc ON sc.node_id = n.id
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
    node_id: Option<u64>,
    action: Option<&str>,
    limit: u32,
) -> Result<Vec<AdminAuditLogRow>, AppError> {
    let mut builder = QueryBuilder::<MySql>::new(
        r#"
SELECT
  a.id,
  a.node_id,
  n.name AS node_name,
  n.server_ip AS node_server_ip,
  n.server_port AS node_server_port,
  a.actor_type,
  a.actor_id,
  a.action,
  CAST(UNIX_TIMESTAMP(a.created_at) AS UNSIGNED) AS created_at,
  CAST(a.detail_json AS CHAR) AS detail_json
FROM node_audit_logs a
JOIN accel_nodes n ON n.id = a.node_id
WHERE 1 = 1
"#,
    );
    if let Some(node_id) = node_id {
        builder.push(" AND a.node_id = ");
        builder.push_bind(node_id);
    }
    if let Some(action) = action {
        builder.push(" AND a.action = ");
        builder.push_bind(action);
    }
    builder.push(" ORDER BY a.id DESC LIMIT ");
    builder.push_bind(limit);

    builder
        .build_query_as::<AdminAuditLogRow>()
        .fetch_all(pool)
        .await
        .map_err(AppError::database)
}

async fn reconcile_node_health_alerts(pool: &MySqlPool) -> Result<(), AppError> {
    let nodes = select_admin_nodes(pool, 500).await?;
    let now = now_unix();
    for node in nodes {
        let specs = build_node_health_alert_specs(&node, now);
        for spec in &specs {
            upsert_health_alert(pool, &node, spec).await?;
        }
        resolve_missing_health_alerts(pool, node.id, &specs).await?;
    }
    Ok(())
}

async fn upsert_health_alert(
    pool: &MySqlPool,
    node: &AdminNodeRow,
    spec: &HealthAlertSpec,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
INSERT INTO node_health_alerts (
  node_id,
  alert_key,
  severity,
  title,
  message,
  status,
  first_seen_at,
  last_seen_at
) VALUES (?, ?, ?, ?, ?, 'open', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
ON DUPLICATE KEY UPDATE
  severity = VALUES(severity),
  title = VALUES(title),
  message = VALUES(message),
  last_seen_at = CURRENT_TIMESTAMP,
  status = IF(node_health_alerts.status = 'resolved', 'open', node_health_alerts.status),
  resolved_at = IF(node_health_alerts.status = 'resolved', NULL, node_health_alerts.resolved_at),
  updated_at = CURRENT_TIMESTAMP
"#,
    )
    .bind(node.id)
    .bind(spec.key)
    .bind(spec.severity)
    .bind(&spec.title)
    .bind(&spec.message)
    .execute(pool)
    .await
    .map_err(AppError::database)?;
    Ok(())
}

async fn resolve_missing_health_alerts(
    pool: &MySqlPool,
    node_id: u64,
    active_specs: &[HealthAlertSpec],
) -> Result<(), AppError> {
    let mut builder = QueryBuilder::<MySql>::new(
        r#"
UPDATE node_health_alerts
SET
  status = 'resolved',
  resolved_at = COALESCE(resolved_at, CURRENT_TIMESTAMP),
  updated_at = CURRENT_TIMESTAMP
WHERE node_id =
"#,
    );
    builder.push_bind(node_id);
    builder.push(" AND status IN ('open', 'acknowledged', 'ignored')");
    if !active_specs.is_empty() {
        builder.push(" AND alert_key NOT IN (");
        {
            let mut separated = builder.separated(", ");
            for spec in active_specs {
                separated.push_bind(spec.key);
            }
        }
        builder.push(")");
    }

    builder
        .build()
        .execute(pool)
        .await
        .map_err(AppError::database)?;
    Ok(())
}

async fn select_health_alerts(
    pool: &MySqlPool,
    node_id: Option<u64>,
    status: Option<&str>,
    severity: Option<&str>,
    limit: u32,
) -> Result<Vec<HealthAlertRow>, AppError> {
    let mut builder = QueryBuilder::<MySql>::new(
        r#"
SELECT
  h.id,
  h.node_id,
  n.name AS node_name,
  n.server_ip AS node_server_ip,
  n.server_port AS node_server_port,
  h.alert_key,
  h.severity,
  h.title,
  h.message,
  h.status,
  CAST(UNIX_TIMESTAMP(h.first_seen_at) AS UNSIGNED) AS first_seen_at,
  CAST(UNIX_TIMESTAMP(h.last_seen_at) AS UNSIGNED) AS last_seen_at,
  CAST(UNIX_TIMESTAMP(h.acknowledged_at) AS UNSIGNED) AS acknowledged_at,
  h.acknowledged_by,
  CAST(UNIX_TIMESTAMP(h.resolved_at) AS UNSIGNED) AS resolved_at,
  CAST(UNIX_TIMESTAMP(h.updated_at) AS UNSIGNED) AS updated_at
FROM node_health_alerts h
JOIN accel_nodes n ON n.id = h.node_id
WHERE 1 = 1
"#,
    );
    if let Some(node_id) = node_id {
        builder.push(" AND h.node_id = ");
        builder.push_bind(node_id);
    }
    if let Some(status) = status {
        match status {
            "active" => {
                builder.push(" AND h.status IN ('open', 'acknowledged')");
            }
            "all" => {}
            status => {
                validate_health_alert_filter_status(status)?;
                builder.push(" AND h.status = ");
                builder.push_bind(status);
            }
        };
    }
    if let Some(severity) = severity {
        validate_health_alert_severity(severity)?;
        builder.push(" AND h.severity = ");
        builder.push_bind(severity);
    }
    builder.push(
        r#"
ORDER BY
  FIELD(h.status, 'open', 'acknowledged', 'ignored', 'resolved'),
  FIELD(h.severity, 'critical', 'warning'),
  h.last_seen_at DESC,
  h.id DESC
LIMIT
"#,
    );
    builder.push_bind(limit);

    builder
        .build_query_as::<HealthAlertRow>()
        .fetch_all(pool)
        .await
        .map_err(AppError::database)
}

async fn select_health_alert(pool: &MySqlPool, alert_id: u64) -> Result<HealthAlertRow, AppError> {
    sqlx::query_as::<_, HealthAlertRow>(
        r#"
SELECT
  h.id,
  h.node_id,
  n.name AS node_name,
  n.server_ip AS node_server_ip,
  n.server_port AS node_server_port,
  h.alert_key,
  h.severity,
  h.title,
  h.message,
  h.status,
  CAST(UNIX_TIMESTAMP(h.first_seen_at) AS UNSIGNED) AS first_seen_at,
  CAST(UNIX_TIMESTAMP(h.last_seen_at) AS UNSIGNED) AS last_seen_at,
  CAST(UNIX_TIMESTAMP(h.acknowledged_at) AS UNSIGNED) AS acknowledged_at,
  h.acknowledged_by,
  CAST(UNIX_TIMESTAMP(h.resolved_at) AS UNSIGNED) AS resolved_at,
  CAST(UNIX_TIMESTAMP(h.updated_at) AS UNSIGNED) AS updated_at
FROM node_health_alerts h
JOIN accel_nodes n ON n.id = h.node_id
WHERE h.id = ?
LIMIT 1
"#,
    )
    .bind(alert_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)?
    .ok_or_else(|| AppError::not_found("health_alert_not_found", "health alert does not exist"))
}

async fn update_health_alert_status(
    pool: &MySqlPool,
    alert_id: u64,
    status: &str,
    actor: &AdminActor,
) -> Result<HealthAlertRow, AppError> {
    match status {
        "open" => {
            sqlx::query(
                r#"
UPDATE node_health_alerts
SET status = 'open',
    acknowledged_at = NULL,
    acknowledged_by = NULL,
    resolved_at = NULL,
    updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
            )
            .bind(alert_id)
            .execute(pool)
            .await
            .map_err(AppError::database)?;
        }
        "acknowledged" | "ignored" => {
            sqlx::query(
                r#"
UPDATE node_health_alerts
SET status = ?,
    acknowledged_at = CURRENT_TIMESTAMP,
    acknowledged_by = ?,
    updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
            )
            .bind(status)
            .bind(actor.id)
            .bind(alert_id)
            .execute(pool)
            .await
            .map_err(AppError::database)?;
        }
        _ => unreachable!("validated health alert status"),
    }
    select_health_alert(pool, alert_id).await
}

async fn select_admin_node_tasks(
    pool: &MySqlPool,
    node_id: u64,
    limit: u32,
) -> Result<Vec<NodeTaskRow>, AppError> {
    sqlx::query_as::<_, NodeTaskRow>(
        r#"
SELECT
  id,
  node_id,
  task_type,
  status,
  message,
  output,
  error_message,
  CAST(UNIX_TIMESTAMP(created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(claimed_at) AS UNSIGNED) AS claimed_at,
  CAST(UNIX_TIMESTAMP(started_at) AS UNSIGNED) AS started_at,
  CAST(UNIX_TIMESTAMP(finished_at) AS UNSIGNED) AS finished_at
FROM node_remote_tasks
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

async fn select_operation_tasks(
    pool: &MySqlPool,
    node_id: Option<u64>,
    status: Option<&str>,
    limit: u32,
) -> Result<Vec<OperationTaskRow>, AppError> {
    let mut builder = QueryBuilder::<MySql>::new(operation_task_select_sql());
    if let Some(node_id) = node_id {
        builder.push(" AND t.node_id = ");
        builder.push_bind(node_id);
    }
    if let Some(status) = status {
        builder.push(" AND t.status = ");
        builder.push_bind(status);
    }
    builder.push(" ORDER BY t.id DESC LIMIT ");
    builder.push_bind(limit);

    builder
        .build_query_as::<OperationTaskRow>()
        .fetch_all(pool)
        .await
        .map_err(AppError::database)
}

async fn select_operation_task(
    pool: &MySqlPool,
    task_id: u64,
) -> Result<Option<OperationTaskRow>, AppError> {
    let mut builder = QueryBuilder::<MySql>::new(operation_task_select_sql());
    builder.push(" AND t.id = ");
    builder.push_bind(task_id);
    builder.push(" LIMIT 1");

    builder
        .build_query_as::<OperationTaskRow>()
        .fetch_optional(pool)
        .await
        .map_err(AppError::database)
}

async fn select_admin_users(pool: &MySqlPool) -> Result<Vec<AdminUserRow>, AppError> {
    sqlx::query_as::<_, AdminUserRow>(
        r#"
SELECT
  id,
  username,
  display_name,
  password_hash,
  role,
  status,
  CAST(UNIX_TIMESTAMP(last_login_at) AS UNSIGNED) AS last_login_at,
  CAST(UNIX_TIMESTAMP(created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(updated_at) AS UNSIGNED) AS updated_at
FROM admin_users
ORDER BY id ASC
"#,
    )
    .fetch_all(pool)
    .await
    .map_err(AppError::database)
}

async fn select_admin_user(
    pool: &MySqlPool,
    user_id: u64,
) -> Result<Option<AdminUserRow>, AppError> {
    sqlx::query_as::<_, AdminUserRow>(
        r#"
SELECT
  id,
  username,
  display_name,
  password_hash,
  role,
  status,
  CAST(UNIX_TIMESTAMP(last_login_at) AS UNSIGNED) AS last_login_at,
  CAST(UNIX_TIMESTAMP(created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(updated_at) AS UNSIGNED) AS updated_at
FROM admin_users
WHERE id = ?
LIMIT 1
"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)
}

async fn select_admin_user_by_username(
    pool: &MySqlPool,
    username: &str,
) -> Result<Option<AdminUserRow>, AppError> {
    sqlx::query_as::<_, AdminUserRow>(
        r#"
SELECT
  id,
  username,
  display_name,
  password_hash,
  role,
  status,
  CAST(UNIX_TIMESTAMP(last_login_at) AS UNSIGNED) AS last_login_at,
  CAST(UNIX_TIMESTAMP(created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(updated_at) AS UNSIGNED) AS updated_at
FROM admin_users
WHERE username = ?
LIMIT 1
"#,
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)
}

async fn insert_admin_user(
    pool: &MySqlPool,
    request: AdminCreateUserRequest,
) -> Result<AdminUserRow, AppError> {
    let username = normalize_admin_username(&request.username)?;
    let display_name = normalize_admin_display_name(request.display_name.as_deref())?;
    let role = validate_admin_role(&request.role)?;
    let status = request
        .status
        .as_deref()
        .map(validate_admin_user_status)
        .transpose()?
        .unwrap_or("active");
    let password_hash = hash_admin_password(&request.password)?;

    let result = sqlx::query(
        r#"
INSERT INTO admin_users (
  username,
  display_name,
  password_hash,
  role,
  status,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
"#,
    )
    .bind(&username)
    .bind(&display_name)
    .bind(&password_hash)
    .bind(role)
    .bind(status)
    .execute(pool)
    .await;

    let result = match result {
        Ok(result) => result,
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("1062") => {
            return Err(AppError::conflict(
                "admin_user_exists",
                "admin username already exists",
            ))
        }
        Err(error) => return Err(AppError::database(error)),
    };

    select_admin_user(pool, result.last_insert_id())
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("created admin user is missing")))
}

async fn update_admin_user(
    pool: &MySqlPool,
    user_id: u64,
    request: AdminUpdateUserRequest,
) -> Result<AdminUserRow, AppError> {
    let existing = select_admin_user(pool, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("admin_user_not_found", "admin user does not exist"))?;
    let display_name = match request.display_name {
        Some(value) => normalize_admin_display_name(Some(&value))?,
        None => existing.display_name,
    };
    let role = match request.role {
        Some(value) => validate_admin_role(&value)?.to_string(),
        None => existing.role,
    };
    let status = match request.status {
        Some(value) => validate_admin_user_status(&value)?.to_string(),
        None => existing.status,
    };
    let password_hash = match request.password {
        Some(value) if !value.trim().is_empty() => hash_admin_password(&value)?,
        _ => existing.password_hash,
    };

    sqlx::query(
        r#"
UPDATE admin_users
SET
  display_name = ?,
  password_hash = ?,
  role = ?,
  status = ?,
  updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
    )
    .bind(&display_name)
    .bind(&password_hash)
    .bind(&role)
    .bind(&status)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    select_admin_user(pool, user_id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("updated admin user is missing")))
}

async fn disable_admin_user(pool: &MySqlPool, user_id: u64) -> Result<AdminUserRow, AppError> {
    sqlx::query(
        r#"
UPDATE admin_users
SET status = 'disabled', updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
    )
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    select_admin_user(pool, user_id)
        .await?
        .ok_or_else(|| AppError::not_found("admin_user_not_found", "admin user does not exist"))
}

async fn mark_admin_user_login(pool: &MySqlPool, user_id: u64) -> Result<(), AppError> {
    sqlx::query(
        r#"
UPDATE admin_users
SET last_login_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
    )
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;
    Ok(())
}

async fn insert_operation_task(
    pool: &MySqlPool,
    node_id: u64,
    action: &str,
    command_label: &str,
) -> Result<OperationTaskRow, AppError> {
    let result = sqlx::query(
        r#"
INSERT INTO node_operation_tasks (
  node_id,
  action,
  executor,
  status,
  command_label,
  started_at,
  created_at,
  updated_at
) VALUES (?, ?, 'control_ssh', 'running', ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(action)
    .bind(command_label)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    select_operation_task(pool, result.last_insert_id())
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("created operation task is missing")))
}

async fn finish_operation_task(
    pool: &MySqlPool,
    task_id: u64,
    status: &str,
    exit_code: Option<i32>,
    duration_ms: u64,
    output: Option<&str>,
    error_message: Option<&str>,
    version_check: Option<&AdminSshActionVersionCheck>,
) -> Result<OperationTaskRow, AppError> {
    let version_check_json = version_check
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| AppError::internal(anyhow::anyhow!(error)))?;
    sqlx::query(
        r#"
UPDATE node_operation_tasks
SET
  status = ?,
  exit_code = ?,
  duration_ms = ?,
  output = ?,
  error_message = ?,
  version_check_json = ?,
  finished_at = CURRENT_TIMESTAMP,
  updated_at = CURRENT_TIMESTAMP
WHERE id = ?
"#,
    )
    .bind(status)
    .bind(exit_code)
    .bind(duration_ms)
    .bind(output)
    .bind(error_message)
    .bind(version_check_json)
    .bind(task_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    select_operation_task(pool, task_id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("finished operation task is missing")))
}

fn operation_task_select_sql() -> &'static str {
    r#"
SELECT
  t.id,
  t.node_id,
  n.name AS node_name,
  n.server_ip AS node_server_ip,
  n.server_port AS node_server_port,
  t.action,
  t.executor,
  t.status,
  t.command_label,
  t.exit_code,
  t.duration_ms,
  t.output,
  t.error_message,
  CAST(t.version_check_json AS CHAR) AS version_check_json,
  CAST(UNIX_TIMESTAMP(t.created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(t.started_at) AS UNSIGNED) AS started_at,
  CAST(UNIX_TIMESTAMP(t.finished_at) AS UNSIGNED) AS finished_at
FROM node_operation_tasks t
JOIN accel_nodes n ON n.id = t.node_id
WHERE 1 = 1
"#
}

async fn select_ssh_credential(
    pool: &MySqlPool,
    node_id: u64,
) -> Result<Option<SshCredentialRow>, AppError> {
    sqlx::query_as::<_, SshCredentialRow>(
        r#"
SELECT
  host,
  port,
  username,
  password_ciphertext,
  password_nonce,
  auth_status,
  last_error,
  CAST(UNIX_TIMESTAMP(last_checked_at) AS UNSIGNED) AS last_checked_at
FROM node_ssh_credentials
WHERE node_id = ?
LIMIT 1
"#,
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::database)
}

async fn select_admin_games(
    pool: &MySqlPool,
    query: &AdminListGamesQuery,
    limit: u32,
) -> Result<Vec<AdminGameRow>, AppError> {
    let mut builder = QueryBuilder::<MySql>::new(
        r#"
SELECT
  g.id,
  g.game_id,
  g.name,
  g.platform,
  g.category,
  g.icon_url,
  g.status,
  g.remark,
  CAST(COUNT(r.id) AS UNSIGNED) AS route_count,
  CAST(UNIX_TIMESTAMP(g.created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(g.updated_at) AS UNSIGNED) AS updated_at
FROM accel_games g
LEFT JOIN game_route_rules r ON r.game_id = g.game_id
WHERE 1 = 1
"#,
    );

    if let Some(status) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|status| !status.is_empty())
    {
        builder.push(" AND g.status = ");
        builder.push_bind(status);
    }
    if let Some(platform) = query
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|platform| !platform.is_empty())
    {
        builder.push(" AND g.platform = ");
        builder.push_bind(platform);
    }
    if let Some(keyword) = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
    {
        let like = format!("%{keyword}%");
        builder.push(" AND (g.name LIKE ");
        builder.push_bind(like.clone());
        builder.push(" OR CAST(g.game_id AS CHAR) LIKE ");
        builder.push_bind(like);
        builder.push(")");
    }

    builder.push(
        r#"
GROUP BY
  g.id,
  g.game_id,
  g.name,
  g.platform,
  g.category,
  g.icon_url,
  g.status,
  g.remark,
  g.created_at,
  g.updated_at
ORDER BY g.status ASC, g.game_id ASC
LIMIT
"#,
    );
    builder.push_bind(limit);

    builder
        .build_query_as::<AdminGameRow>()
        .fetch_all(pool)
        .await
        .map_err(AppError::database)
}

async fn select_admin_game(
    pool: &MySqlPool,
    game_id: u64,
) -> Result<Option<AdminGameRow>, AppError> {
    sqlx::query_as::<_, AdminGameRow>(
        r#"
SELECT
  g.id,
  g.game_id,
  g.name,
  g.platform,
  g.category,
  g.icon_url,
  g.status,
  g.remark,
  CAST(COUNT(r.id) AS UNSIGNED) AS route_count,
  CAST(UNIX_TIMESTAMP(g.created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(g.updated_at) AS UNSIGNED) AS updated_at
FROM accel_games g
LEFT JOIN game_route_rules r ON r.game_id = g.game_id
WHERE g.game_id = ?
GROUP BY
  g.id,
  g.game_id,
  g.name,
  g.platform,
  g.category,
  g.icon_url,
  g.status,
  g.remark,
  g.created_at,
  g.updated_at
LIMIT 1
"#,
    )
    .bind(game_id)
    .fetch_optional(pool)
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
  r.region_id,
  r.region_name,
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
  r.sync_source,
  r.external_id,
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
    if let Some(region_id) = query.region_id {
        builder.push(" AND r.region_id = ");
        builder.push_bind(region_id);
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
ORDER BY r.game_id ASC, r.region_id ASC, r.priority ASC, r.id ASC
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
  r.region_id,
  r.region_name,
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
  r.sync_source,
  r.external_id,
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

async fn insert_admin_game(pool: &MySqlPool, request: AdminGameRequest) -> Result<u64, AppError> {
    let game = normalize_game_request(&request)?;
    sqlx::query(
        r#"
INSERT INTO accel_games (
  game_id,
  name,
  platform,
  category,
  icon_url,
  status,
  remark,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
"#,
    )
    .bind(game.game_id)
    .bind(&game.name)
    .bind(&game.platform)
    .bind(&game.category)
    .bind(&game.icon_url)
    .bind(&game.status)
    .bind(&game.remark)
    .execute(pool)
    .await
    .map_err(map_game_write_error)?;

    Ok(game.game_id)
}

async fn update_admin_game(
    pool: &MySqlPool,
    game_id: u64,
    request: AdminUpdateGameRequest,
) -> Result<(), AppError> {
    if game_id == 0 {
        return Err(AppError::bad_request(
            "invalid_game",
            "game_id must be positive",
        ));
    }
    if request.game_id != game_id {
        return Err(AppError::bad_request(
            "invalid_game",
            "request game_id must match path game_id",
        ));
    }
    let game = normalize_game_request(&request)?;
    let result = sqlx::query(
        r#"
UPDATE accel_games
SET
  name = ?,
  platform = ?,
  category = ?,
  icon_url = ?,
  status = ?,
  remark = ?,
  updated_at = CURRENT_TIMESTAMP
WHERE game_id = ?
"#,
    )
    .bind(&game.name)
    .bind(&game.platform)
    .bind(&game.category)
    .bind(&game.icon_url)
    .bind(&game.status)
    .bind(&game.remark)
    .bind(game_id)
    .execute(pool)
    .await
    .map_err(map_game_write_error)?;

    if result.rows_affected() == 0 {
        return Err(AppError::not_found("game_not_found", "game does not exist"));
    }
    Ok(())
}

async fn delete_admin_game(pool: &MySqlPool, game_id: u64) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
DELETE FROM accel_games
WHERE game_id = ?
"#,
    )
    .bind(game_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    Ok(result.rows_affected() > 0)
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
  region_id,
  region_name,
  node_id,
  target_addr,
  protocol,
  area,
  tag,
  priority,
  status,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
"#,
    )
    .bind(rule.game_id)
    .bind(&rule.game_name)
    .bind(rule.region_id)
    .bind(&rule.region_name)
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
  region_id = ?,
  region_name = ?,
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
    .bind(rule.region_id)
    .bind(&rule.region_name)
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

async fn sync_business_catalog(
    pool: &MySqlPool,
    catalog: BusinessSyncCatalog,
) -> Result<BusinessSyncCatalogResponse, AppError> {
    let games_upserted = catalog.games.len();
    let regions_upserted = catalog.regions.len();
    let route_rules_upserted = catalog.route_rules.len();
    let mut tx = pool.begin().await.map_err(AppError::database)?;

    for game in &catalog.games {
        upsert_business_game(&mut tx, game).await?;
    }
    for region in &catalog.regions {
        upsert_business_region(&mut tx, region).await?;
    }
    for rule in &catalog.route_rules {
        upsert_business_route_rule(&mut tx, rule).await?;
    }

    tx.commit().await.map_err(AppError::database)?;

    Ok(BusinessSyncCatalogResponse {
        status: "ok",
        source: catalog.source,
        revision: catalog.revision,
        games_upserted,
        regions_upserted,
        route_rules_upserted,
        server_time: now_unix(),
    })
}

async fn upsert_business_game(
    tx: &mut sqlx::Transaction<'_, MySql>,
    game: &NormalizedGame,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
INSERT INTO accel_games (
  game_id,
  name,
  platform,
  category,
  icon_url,
  status,
  remark,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
ON DUPLICATE KEY UPDATE
  name = VALUES(name),
  platform = VALUES(platform),
  category = VALUES(category),
  icon_url = VALUES(icon_url),
  status = VALUES(status),
  remark = VALUES(remark),
  updated_at = CURRENT_TIMESTAMP
"#,
    )
    .bind(game.game_id)
    .bind(&game.name)
    .bind(&game.platform)
    .bind(&game.category)
    .bind(&game.icon_url)
    .bind(&game.status)
    .bind(&game.remark)
    .execute(&mut **tx)
    .await
    .map_err(map_game_write_error)?;
    Ok(())
}

async fn upsert_business_region(
    tx: &mut sqlx::Transaction<'_, MySql>,
    region: &NormalizedGameRegion,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
INSERT INTO accel_game_regions (
  game_id,
  region_id,
  name,
  area,
  status,
  remark,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
ON DUPLICATE KEY UPDATE
  name = VALUES(name),
  area = VALUES(area),
  status = VALUES(status),
  remark = VALUES(remark),
  updated_at = CURRENT_TIMESTAMP
"#,
    )
    .bind(region.game_id)
    .bind(region.region_id)
    .bind(&region.name)
    .bind(&region.area)
    .bind(&region.status)
    .bind(&region.remark)
    .execute(&mut **tx)
    .await
    .map_err(AppError::database)?;
    Ok(())
}

async fn upsert_business_route_rule(
    tx: &mut sqlx::Transaction<'_, MySql>,
    rule: &NormalizedRouteRule,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
INSERT INTO game_route_rules (
  game_id,
  game_name,
  region_id,
  region_name,
  node_id,
  target_addr,
  protocol,
  area,
  tag,
  priority,
  status,
  sync_source,
  external_id,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
ON DUPLICATE KEY UPDATE
  game_id = VALUES(game_id),
  game_name = VALUES(game_name),
  region_id = VALUES(region_id),
  region_name = VALUES(region_name),
  node_id = VALUES(node_id),
  target_addr = VALUES(target_addr),
  protocol = VALUES(protocol),
  area = VALUES(area),
  tag = VALUES(tag),
  priority = VALUES(priority),
  status = VALUES(status),
  sync_source = VALUES(sync_source),
  external_id = VALUES(external_id),
  updated_at = CURRENT_TIMESTAMP
"#,
    )
    .bind(rule.game_id)
    .bind(&rule.game_name)
    .bind(rule.region_id)
    .bind(&rule.region_name)
    .bind(rule.node_id)
    .bind(&rule.target_addr)
    .bind(&rule.protocol)
    .bind(&rule.area)
    .bind(&rule.tag)
    .bind(rule.priority)
    .bind(&rule.status)
    .bind(&rule.sync_source)
    .bind(&rule.external_id)
    .execute(&mut **tx)
    .await
    .map_err(map_route_rule_write_error)?;
    Ok(())
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

async fn insert_admin_node_task(
    pool: &MySqlPool,
    node_id: u64,
    task_type: &str,
    message: Option<&str>,
    actor: &AdminActor,
) -> Result<NodeTaskRow, AppError> {
    ensure_admin_node_exists(pool, node_id).await?;
    let mut tx = pool.begin().await.map_err(AppError::database)?;

    let result = sqlx::query(
        r#"
INSERT INTO node_remote_tasks (
  node_id,
  task_type,
  status,
  message,
  requested_by,
  created_at,
  updated_at
) VALUES (?, ?, 'pending', ?, 'admin_api', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(task_type)
    .bind(message)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;
    let task_id = result.last_insert_id();

    let detail_json = serde_json::json!({
        "task_id": task_id,
        "task_type": task_type,
        "message": message,
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
) VALUES (?, ?, ?, 'node.task.create', ?, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(actor.audit_actor_type())
    .bind(actor.id)
    .bind(detail_json.to_string())
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    let task = select_node_task_for_update(&mut tx, node_id, task_id).await?;
    tx.commit().await.map_err(AppError::database)?;
    Ok(task)
}

async fn upsert_ssh_credential(
    pool: &MySqlPool,
    node_id: u64,
    credential: &NormalizedSshCredential,
    password_ciphertext: &str,
    password_nonce: &str,
    actor: &AdminActor,
) -> Result<SshCredentialRow, AppError> {
    ensure_admin_node_exists(pool, node_id).await?;
    let mut tx = pool.begin().await.map_err(AppError::database)?;

    sqlx::query(
        r#"
INSERT INTO node_ssh_credentials (
  node_id,
  host,
  port,
  username,
  password_ciphertext,
  password_nonce,
  auth_status,
  last_error,
  last_checked_at,
  created_at,
  updated_at
) VALUES (?, ?, ?, ?, ?, ?, 'untested', NULL, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
ON DUPLICATE KEY UPDATE
  host = VALUES(host),
  port = VALUES(port),
  username = VALUES(username),
  password_ciphertext = VALUES(password_ciphertext),
  password_nonce = VALUES(password_nonce),
  auth_status = 'untested',
  last_error = NULL,
  last_checked_at = NULL,
  updated_at = CURRENT_TIMESTAMP
"#,
    )
    .bind(node_id)
    .bind(&credential.host)
    .bind(u32::from(credential.port))
    .bind(&credential.username)
    .bind(password_ciphertext)
    .bind(password_nonce)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    let detail_json = serde_json::json!({
        "host": credential.host,
        "port": credential.port,
        "username": credential.username,
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
) VALUES (?, ?, ?, 'node.ssh_credential.upsert', ?, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(actor.audit_actor_type())
    .bind(actor.id)
    .bind(detail_json.to_string())
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    tx.commit().await.map_err(AppError::database)?;
    select_ssh_credential(pool, node_id)
        .await?
        .ok_or_else(|| AppError::not_found("ssh_not_found", "ssh credential was not stored"))
}

async fn delete_ssh_credential(
    pool: &MySqlPool,
    node_id: u64,
    actor: &AdminActor,
) -> Result<bool, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::database)?;
    let result = sqlx::query(
        r#"
DELETE FROM node_ssh_credentials
WHERE node_id = ?
"#,
    )
    .bind(node_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;
    let deleted = result.rows_affected() > 0;

    if deleted {
        sqlx::query(
            r#"
INSERT INTO node_audit_logs (
  node_id,
  actor_type,
  actor_id,
  action,
  detail_json,
  created_at
) VALUES (?, ?, ?, 'node.ssh_credential.delete', NULL, CURRENT_TIMESTAMP)
"#,
        )
        .bind(node_id)
        .bind(actor.audit_actor_type())
        .bind(actor.id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::database)?;
    }

    tx.commit().await.map_err(AppError::database)?;
    Ok(deleted)
}

async fn claim_next_node_task(
    pool: &MySqlPool,
    node_id: u64,
) -> Result<Option<NodeTaskRow>, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::database)?;
    let task_id = sqlx::query_scalar::<_, u64>(
        r#"
SELECT id
FROM node_remote_tasks
WHERE node_id = ?
  AND status = 'pending'
ORDER BY id ASC
LIMIT 1
FOR UPDATE
"#,
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::database)?;

    let Some(task_id) = task_id else {
        tx.commit().await.map_err(AppError::database)?;
        return Ok(None);
    };

    sqlx::query(
        r#"
UPDATE node_remote_tasks
SET status = 'running',
    claimed_at = CURRENT_TIMESTAMP,
    started_at = CURRENT_TIMESTAMP,
    updated_at = CURRENT_TIMESTAMP
WHERE id = ?
  AND node_id = ?
"#,
    )
    .bind(task_id)
    .bind(node_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    let task = select_node_task_for_update(&mut tx, node_id, task_id).await?;
    tx.commit().await.map_err(AppError::database)?;
    Ok(Some(task))
}

async fn select_node_task_for_update(
    tx: &mut sqlx::Transaction<'_, MySql>,
    node_id: u64,
    task_id: u64,
) -> Result<NodeTaskRow, AppError> {
    sqlx::query_as::<_, NodeTaskRow>(
        r#"
SELECT
  id,
  node_id,
  task_type,
  status,
  message,
  output,
  error_message,
  CAST(UNIX_TIMESTAMP(created_at) AS UNSIGNED) AS created_at,
  CAST(UNIX_TIMESTAMP(claimed_at) AS UNSIGNED) AS claimed_at,
  CAST(UNIX_TIMESTAMP(started_at) AS UNSIGNED) AS started_at,
  CAST(UNIX_TIMESTAMP(finished_at) AS UNSIGNED) AS finished_at
FROM node_remote_tasks
WHERE id = ?
  AND node_id = ?
LIMIT 1
FOR UPDATE
"#,
    )
    .bind(task_id)
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::database)?
    .ok_or_else(|| AppError::not_found("task_not_found", "node task does not exist"))
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

async fn delete_admin_node(pool: &MySqlPool, node_id: u64) -> Result<bool, AppError> {
    if node_id == 0 {
        return Err(AppError::bad_request(
            "invalid_node",
            "node_id must be positive",
        ));
    }

    let result = sqlx::query(
        r#"
DELETE FROM accel_nodes
WHERE id = ?
"#,
    )
    .bind(node_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;

    Ok(result.rows_affected() > 0)
}

async fn update_admin_node_status(
    pool: &MySqlPool,
    node_id: u64,
    next_status: &str,
    reason: Option<&str>,
    actor: &AdminActor,
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
) VALUES (?, ?, ?, 'node.status.update', ?, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(actor.audit_actor_type())
    .bind(actor.id)
    .bind(detail_json.to_string())
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    tx.commit().await.map_err(AppError::database)?;
    Ok(response_previous_status)
}

async fn update_node_task_result(
    pool: &MySqlPool,
    node_id: u64,
    task_id: u64,
    status: &str,
    request: &NodeTaskResultRequest,
) -> Result<(), AppError> {
    let message = request
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(512).collect::<String>());
    let output = request
        .output
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(4096).collect::<String>());
    let error_message = if status == "failed" {
        message.clone()
    } else {
        None
    };

    let mut tx = pool.begin().await.map_err(AppError::database)?;
    let result = sqlx::query(
        r#"
UPDATE node_remote_tasks
SET status = ?,
    message = COALESCE(?, message),
    output = ?,
    error_message = ?,
    started_at = COALESCE(FROM_UNIXTIME(?), started_at),
    finished_at = COALESCE(FROM_UNIXTIME(?), CURRENT_TIMESTAMP),
    updated_at = CURRENT_TIMESTAMP
WHERE id = ?
  AND node_id = ?
  AND status IN ('pending', 'running')
"#,
    )
    .bind(status)
    .bind(&message)
    .bind(&output)
    .bind(&error_message)
    .bind(request.started_at)
    .bind(request.finished_at)
    .bind(task_id)
    .bind(node_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    if result.rows_affected() == 0 {
        return Err(AppError::not_found(
            "task_not_found",
            "node task does not exist or is already finished",
        ));
    }

    let detail_json = serde_json::json!({
        "task_id": task_id,
        "status": status,
        "message": message,
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
) VALUES (?, 'node', NULL, 'node.task.result', ?, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(detail_json.to_string())
    .execute(&mut *tx)
    .await
    .map_err(AppError::database)?;

    tx.commit().await.map_err(AppError::database)?;
    Ok(())
}

async fn update_ssh_credential_status(
    pool: &MySqlPool,
    node_id: u64,
    auth_status: &str,
    last_error: Option<&str>,
) -> Result<(), AppError> {
    let stored_last_error =
        last_error.map(|error| truncate_chars(error, MAX_STORED_LAST_ERROR_CHARS));
    sqlx::query(
        r#"
UPDATE node_ssh_credentials
SET auth_status = ?,
    last_error = ?,
    last_checked_at = CURRENT_TIMESTAMP,
    updated_at = CURRENT_TIMESTAMP
WHERE node_id = ?
"#,
    )
    .bind(auth_status)
    .bind(stored_last_error.as_deref())
    .bind(node_id)
    .execute(pool)
    .await
    .map_err(AppError::database)?;
    Ok(())
}

async fn insert_node_audit_log(
    pool: &MySqlPool,
    node_id: u64,
    actor_type: &str,
    actor_id: Option<u64>,
    action: &str,
    detail_json: Value,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
INSERT INTO node_audit_logs (
  node_id,
  actor_type,
  actor_id,
  action,
  detail_json,
  created_at
) VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
"#,
    )
    .bind(node_id)
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(detail_json.to_string())
    .execute(pool)
    .await
    .map_err(AppError::database)?;
    Ok(())
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
    if request.region_id == Some(0) {
        return Err(AppError::bad_request(
            "invalid_region",
            "region_id must be positive",
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

fn validate_connectivity_diagnostic_request(
    request: &AdminConnectivityDiagnosticRequest,
) -> Result<(), AppError> {
    let connect_request = ConnectIntentRequest {
        user_id: request.user_id,
        device_id: request.device_id.clone(),
        game_id: request.game_id,
        region_id: request.region_id,
        platform: request.platform.clone(),
        client_isp: request.client_isp.clone(),
        client_ip: request.client_ip.clone(),
        bandwidth_quality: request.bandwidth_quality.clone(),
    };
    validate_connect_intent_request(&connect_request)?;
    if request
        .payload
        .as_deref()
        .unwrap_or(DEFAULT_DIAGNOSTIC_PAYLOAD)
        .len()
        > 1200
    {
        return Err(AppError::bad_request(
            "invalid_payload",
            "payload must not exceed 1200 bytes",
        ));
    }
    let timeout_sec = request
        .timeout_sec
        .unwrap_or(DEFAULT_DIAGNOSTIC_TIMEOUT_SEC);
    if timeout_sec == 0 || timeout_sec > 15 {
        return Err(AppError::bad_request(
            "invalid_timeout",
            "timeout_sec must be between 1 and 15",
        ));
    }
    let response_timeout_ms = request
        .response_timeout_ms
        .unwrap_or(DEFAULT_DIAGNOSTIC_RESPONSE_TIMEOUT_MS);
    if response_timeout_ms == 0 || response_timeout_ms > 10_000 {
        return Err(AppError::bad_request(
            "invalid_response_timeout",
            "response_timeout_ms must be between 1 and 10000",
        ));
    }
    if request.candidate_index.unwrap_or(0) > 32 {
        return Err(AppError::bad_request(
            "invalid_candidate_index",
            "candidate_index is too large",
        ));
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

fn validate_game_query(query: &AdminListGamesQuery) -> Result<(), AppError> {
    if let Some(status) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|status| !status.is_empty())
    {
        validate_game_status(status)?;
    }
    if let Some(platform) = query
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|platform| !platform.is_empty())
    {
        validate_game_platform(platform)?;
    }
    if query
        .keyword
        .as_deref()
        .is_some_and(|keyword| keyword.chars().count() > 128)
    {
        return Err(AppError::bad_request(
            "invalid_keyword",
            "keyword must be at most 128 characters",
        ));
    }
    Ok(())
}

fn normalize_game_request(request: &AdminGameRequest) -> Result<NormalizedGame, AppError> {
    if request.game_id == 0 {
        return Err(AppError::bad_request(
            "invalid_game",
            "game_id must be positive",
        ));
    }
    let platform = validate_game_platform(request.platform.as_deref().unwrap_or("pc"))?;
    let status = validate_game_status(request.status.as_deref().unwrap_or("enabled"))?;
    let icon_url = normalize_optional_text(request.icon_url.as_deref(), 512)?;
    if let Some(icon_url) = icon_url.as_deref() {
        normalize_url_arg(icon_url)?;
    }

    Ok(NormalizedGame {
        game_id: request.game_id,
        name: normalize_required_text(&request.name, "name", 128)?,
        platform: platform.to_string(),
        category: normalize_optional_text(request.category.as_deref(), 64)?,
        icon_url,
        status: status.to_string(),
        remark: normalize_optional_text(request.remark.as_deref(), 512)?,
    })
}

fn normalize_business_sync_catalog(
    request: BusinessSyncCatalogRequest,
) -> Result<BusinessSyncCatalog, AppError> {
    if request.games.is_empty() && request.regions.is_empty() && request.route_rules.is_empty() {
        return Err(AppError::bad_request(
            "empty_sync_catalog",
            "at least one game, region, or route_rule is required",
        ));
    }
    let source = normalize_optional_text(request.source.as_deref(), 32)?
        .unwrap_or_else(|| "business".to_string());
    let revision = normalize_optional_text(request.revision.as_deref(), 128)?;
    let games = request
        .games
        .iter()
        .map(normalize_business_game)
        .collect::<Result<Vec<_>, _>>()?;
    let regions = request
        .regions
        .iter()
        .map(normalize_business_region)
        .collect::<Result<Vec<_>, _>>()?;
    let route_rules = request
        .route_rules
        .iter()
        .map(|rule| normalize_business_route_rule(rule, &source))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(BusinessSyncCatalog {
        source,
        revision,
        games,
        regions,
        route_rules,
    })
}

fn normalize_business_game(request: &BusinessSyncGame) -> Result<NormalizedGame, AppError> {
    normalize_game_request(&AdminGameRequest {
        game_id: request.game_id,
        name: request.name.clone(),
        platform: request.platform.clone(),
        category: request.category.clone(),
        icon_url: request.icon_url.clone(),
        status: request.status.clone(),
        remark: request.remark.clone(),
    })
}

fn normalize_business_region(
    request: &BusinessSyncRegion,
) -> Result<NormalizedGameRegion, AppError> {
    if request.game_id == 0 {
        return Err(AppError::bad_request(
            "invalid_game",
            "game_id must be positive",
        ));
    }
    if request.region_id == 0 {
        return Err(AppError::bad_request(
            "invalid_region",
            "region_id must be positive",
        ));
    }

    Ok(NormalizedGameRegion {
        game_id: request.game_id,
        region_id: request.region_id,
        name: normalize_required_text(&request.name, "name", 128)?,
        area: normalize_optional_text(request.area.as_deref(), 32)?,
        status: validate_game_status(request.status.as_deref().unwrap_or("enabled"))?.to_string(),
        remark: normalize_optional_text(request.remark.as_deref(), 512)?,
    })
}

fn normalize_business_route_rule(
    request: &BusinessSyncRouteRule,
    source: &str,
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
    if request.region_id == Some(0) {
        return Err(AppError::bad_request(
            "invalid_region",
            "region_id must be positive",
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

    let target_addr = validate_target_addr(&request.target_addr)?;
    let game_name = normalize_optional_text(request.game_name.as_deref(), 128)?
        .unwrap_or_else(|| format!("Game {}", request.game_id));
    let external_id = normalize_optional_text(request.external_id.as_deref(), 128)?.or_else(|| {
        Some(default_business_route_external_id(
            request.game_id,
            request.region_id,
            request.node_id,
            &target_addr,
            protocol,
        ))
    });

    Ok(NormalizedRouteRule {
        game_id: request.game_id,
        game_name,
        region_id: request.region_id,
        region_name: normalize_optional_text(request.region_name.as_deref(), 128)?,
        node_id: request.node_id,
        target_addr,
        protocol: protocol.to_string(),
        area: normalize_optional_text(request.area.as_deref(), 32)?,
        tag: normalize_optional_text(request.tag.as_deref(), 64)?,
        priority: request.priority.unwrap_or(100),
        status: validate_route_rule_status(request.status.as_deref().unwrap_or("enabled"))?
            .to_string(),
        sync_source: Some(source.to_string()),
        external_id,
    })
}

fn default_business_route_external_id(
    game_id: u64,
    region_id: Option<u64>,
    node_id: u64,
    target_addr: &str,
    protocol: &str,
) -> String {
    let seed = format!(
        "{game_id}:{}:{node_id}:{target_addr}:{protocol}",
        region_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "global".to_string())
    );
    let digest = Sha256::digest(seed.as_bytes());
    let short_hash = URL_SAFE_NO_PAD.encode(&digest[..9]);
    format!(
        "route-{game_id}-{}-{short_hash}",
        region_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "global".to_string())
    )
}

fn validate_game_platform(platform: &str) -> Result<&'static str, AppError> {
    match platform.trim() {
        "" | "pc" => Ok("pc"),
        "android" => Ok("android"),
        "ios" => Ok("ios"),
        "multi" => Ok("multi"),
        _ => Err(AppError::bad_request(
            "invalid_game_platform",
            "platform must be pc, android, ios, or multi",
        )),
    }
}

fn validate_game_status(status: &str) -> Result<&'static str, AppError> {
    match status.trim() {
        "" | "enabled" => Ok("enabled"),
        "disabled" => Ok("disabled"),
        _ => Err(AppError::bad_request(
            "invalid_game_status",
            "status must be enabled or disabled",
        )),
    }
}

fn validate_route_rule_query(query: &AdminListRouteRulesQuery) -> Result<(), AppError> {
    if query.game_id == Some(0) {
        return Err(AppError::bad_request(
            "invalid_game",
            "game_id must be positive",
        ));
    }
    if query.region_id == Some(0) {
        return Err(AppError::bad_request(
            "invalid_region",
            "region_id must be positive",
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
    if request.region_id == Some(0) {
        return Err(AppError::bad_request(
            "invalid_region",
            "region_id must be positive",
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
        region_id: request.region_id,
        region_name: normalize_optional_text(request.region_name.as_deref(), 128)?,
        node_id: request.node_id,
        target_addr: validate_target_addr(&request.target_addr)?,
        protocol: protocol.to_string(),
        area: normalize_optional_text(request.area.as_deref(), 32)?,
        tag: normalize_optional_text(request.tag.as_deref(), 64)?,
        priority: request.priority.unwrap_or(100),
        status: validate_route_rule_status(request.status.as_deref().unwrap_or("enabled"))?
            .to_string(),
        sync_source: None,
        external_id: None,
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

fn validate_admin_node_task_type(task_type: &str) -> Result<&'static str, AppError> {
    match task_type.trim() {
        "restart_node" => Ok("restart_node"),
        _ => Err(AppError::bad_request(
            "invalid_task_type",
            "task_type must be restart_node",
        )),
    }
}

fn validate_operation_task_status(status: &str) -> Result<&'static str, AppError> {
    match status.trim() {
        "running" => Ok("running"),
        "succeeded" => Ok("succeeded"),
        "failed" => Ok("failed"),
        _ => Err(AppError::bad_request(
            "invalid_operation_task_status",
            "operation task status must be running, succeeded, or failed",
        )),
    }
}

fn validate_ssh_action(action: &str) -> Result<&'static str, AppError> {
    match action.trim() {
        "test_connection" => Ok("test_connection"),
        "restart_node_service" => Ok("restart_node_service"),
        "upgrade_node" => Ok("upgrade_node"),
        "reboot_server" => Ok("reboot_server"),
        _ => Err(AppError::bad_request(
            "invalid_ssh_action",
            "ssh action must be test_connection, restart_node_service, upgrade_node, or reboot_server",
        )),
    }
}

fn operation_action_label(action: &str) -> &'static str {
    match action {
        "test_connection" => "测试 SSH 连接",
        "restart_node_service" => "重启节点服务",
        "upgrade_node" => "升级节点内核",
        "reboot_server" => "重启服务器",
        "deploy_node" => "一键部署节点",
        _ => "远程运维",
    }
}

fn duration_ms_to_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn normalize_ssh_credential_request(
    node: &AdminNodeRow,
    request: AdminSshCredentialRequest,
) -> Result<NormalizedSshCredential, AppError> {
    let host = request
        .host
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&node.server_ip);
    let host = normalize_ssh_host(host)?;
    let port = request.port.unwrap_or(22);
    if port == 0 {
        return Err(AppError::bad_request(
            "invalid_ssh_port",
            "ssh port must be between 1 and 65535",
        ));
    }
    let username = normalize_ssh_username(&request.username)?;
    let password = normalize_required_text(&request.password, "password", 2048)?;

    Ok(NormalizedSshCredential {
        host,
        port,
        username,
        password,
    })
}

fn build_node_health_alert_specs(node: &AdminNodeRow, now: u64) -> Vec<HealthAlertSpec> {
    let mut alerts = Vec::new();
    let endpoint = format!("{}:{}", node.server_ip, node.server_port);

    if node.status != "online" {
        let planned = matches!(
            node.status.as_str(),
            "pending_install" | "disabled" | "draining"
        );
        alerts.push(HealthAlertSpec {
            key: "node_status",
            severity: if planned { "warning" } else { "critical" },
            title: "节点未在线".to_string(),
            message: format!(
                "{}，调度会避开这个节点。",
                node_status_value_text(&node.status)
            ),
        });
    }

    match node.last_report_at {
        None => {
            let planned = matches!(
                node.status.as_str(),
                "pending_install" | "disabled" | "draining"
            );
            alerts.push(HealthAlertSpec {
                key: "report_missing",
                severity: if planned { "warning" } else { "critical" },
                title: "还没有健康上报".to_string(),
                message: "节点没有上报运行数据，先确认内核服务和控制面地址。".to_string(),
            });
        }
        Some(reported_at) => {
            let age = now.saturating_sub(reported_at);
            if age > 180 {
                alerts.push(HealthAlertSpec {
                    key: "report_stale",
                    severity: "critical",
                    title: "健康上报中断".to_string(),
                    message: format!(
                        "最近一次上报在 {} 前，可能是节点掉线或控制面不可达。",
                        age_label(age)
                    ),
                });
            } else if age > 90 {
                alerts.push(HealthAlertSpec {
                    key: "report_slow",
                    severity: "warning",
                    title: "健康上报延迟".to_string(),
                    message: format!(
                        "最近一次上报在 {} 前，建议观察网络和节点负载。",
                        age_label(age)
                    ),
                });
            }
        }
    }

    if let Some(status) = node.latest_report_status.as_deref() {
        if status != "ready" {
            alerts.push(HealthAlertSpec {
                key: "report_status",
                severity: if status == "degraded" {
                    "warning"
                } else {
                    "critical"
                },
                title: "健康上报状态异常".to_string(),
                message: format!(
                    "最新上报状态是 {}，需要查看节点日志。",
                    report_status_value_text(status)
                ),
            });
        }
    }

    if node.status == "online" {
        if let Some((udp, tcp)) = latest_listener_flags(node) {
            if udp == Some(false) || tcp == Some(false) {
                alerts.push(HealthAlertSpec {
                    key: "listener_down",
                    severity: "critical",
                    title: "监听异常".to_string(),
                    message: format!(
                        "{} 的监听不完整：{}，客户端可能无法接入。",
                        endpoint,
                        listener_flags_label(udp, tcp)
                    ),
                });
            }
        }
    }

    if node.ssh_host.is_some() && node.ssh_auth_status.as_deref() == Some("failed") {
        alerts.push(HealthAlertSpec {
            key: "ssh_failed",
            severity: "warning",
            title: "SSH 连接失败".to_string(),
            message: node
                .ssh_last_error
                .clone()
                .unwrap_or_else(|| "服务器账号密码或端口可能不对，一键升级会失败。".to_string()),
        });
    }

    alerts
}

fn latest_listener_flags(node: &AdminNodeRow) -> Option<(Option<bool>, Option<bool>)> {
    let raw = node.latest_report_raw_json.as_deref()?;
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let listeners = value.get("health")?.get("listeners")?;
    Some((
        listeners.get("udp_listening").and_then(Value::as_bool),
        listeners.get("tcp_listening").and_then(Value::as_bool),
    ))
}

fn listener_flags_label(udp: Option<bool>, tcp: Option<bool>) -> String {
    let udp = match udp {
        Some(true) => "UDP 正常",
        Some(false) => "UDP 异常",
        None => "UDP 未上报",
    };
    let tcp = match tcp {
        Some(true) => "TCP 正常",
        Some(false) => "TCP 异常",
        None => "TCP 未上报",
    };
    format!("{udp} / {tcp}")
}

fn node_status_value_text(status: &str) -> &'static str {
    match status {
        "online" => "在线",
        "pending_install" => "待安装",
        "installing" => "安装中",
        "disabled" => "禁用",
        "draining" => "排空",
        "offline" => "离线",
        "install_failed" => "安装失败",
        "degraded" => "降级",
        _ => "未知状态",
    }
}

fn report_status_value_text(status: &str) -> &'static str {
    match status {
        "ready" => "就绪",
        "error" => "异常",
        "degraded" => "降级",
        _ => "未知",
    }
}

fn age_label(age: u64) -> String {
    if age < 60 {
        format!("{age} 秒")
    } else if age < 3600 {
        format!("{} 分钟", age / 60)
    } else {
        format!("{} 小时", age / 3600)
    }
}

fn validate_health_alert_status(status: &str) -> Result<&str, AppError> {
    match status.trim() {
        "open" | "acknowledged" | "ignored" => Ok(status.trim()),
        _ => Err(AppError::bad_request(
            "invalid_health_alert_status",
            "health alert status must be open, acknowledged, or ignored",
        )),
    }
}

fn validate_health_alert_filter_status(status: &str) -> Result<(), AppError> {
    match status {
        "open" | "acknowledged" | "resolved" | "ignored" => Ok(()),
        _ => Err(AppError::bad_request(
            "invalid_health_alert_status",
            "health alert status filter is invalid",
        )),
    }
}

fn validate_health_alert_severity(severity: &str) -> Result<(), AppError> {
    match severity {
        "critical" | "warning" => Ok(()),
        _ => Err(AppError::bad_request(
            "invalid_health_alert_severity",
            "health alert severity must be critical or warning",
        )),
    }
}

fn normalize_ssh_host(value: &str) -> Result<String, AppError> {
    let host = normalize_required_text(value, "ssh host", 128)?;
    let allowed = host
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '_'));
    if !allowed || host.starts_with('-') {
        return Err(AppError::bad_request(
            "invalid_ssh_host",
            "ssh host contains unsupported characters",
        ));
    }
    Ok(host)
}

fn normalize_ssh_username(value: &str) -> Result<String, AppError> {
    let username = normalize_required_text(value, "username", 64)?;
    let allowed = username
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'));
    if !allowed || username.starts_with('-') {
        return Err(AppError::bad_request(
            "invalid_ssh_username",
            "ssh username contains unsupported characters",
        ));
    }
    Ok(username)
}

fn credential_key(state: &AppState) -> Result<[u8; 32], AppError> {
    let key = state.credential_key.as_deref().ok_or_else(|| {
        AppError::service_unavailable(
            "credential_key_missing",
            "XACCEL_CREDENTIAL_KEY is required before storing SSH passwords",
        )
    })?;
    let decoded = BASE64.decode(key).map_err(|_| {
        AppError::service_unavailable(
            "credential_key_invalid",
            "XACCEL_CREDENTIAL_KEY must be base64 encoded 32 bytes",
        )
    })?;
    decoded.try_into().map_err(|_| {
        AppError::service_unavailable(
            "credential_key_invalid",
            "XACCEL_CREDENTIAL_KEY must decode to exactly 32 bytes",
        )
    })
}

fn encrypt_credential_secret(
    key: &[u8; 32],
    plaintext: &str,
) -> Result<(String, String), AppError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|error| {
        AppError::internal(anyhow::anyhow!(
            "failed to initialize credential cipher: {error}"
        ))
    })?;
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|error| {
            AppError::internal(anyhow::anyhow!("failed to encrypt credential: {error}"))
        })?;
    Ok((BASE64.encode(ciphertext), BASE64.encode(nonce_bytes)))
}

fn decrypt_credential_secret(
    key: &[u8; 32],
    ciphertext: &str,
    nonce: &str,
) -> Result<String, AppError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|error| {
        AppError::internal(anyhow::anyhow!(
            "failed to initialize credential cipher: {error}"
        ))
    })?;
    let ciphertext = BASE64.decode(ciphertext).map_err(|_| {
        AppError::service_unavailable("credential_decode_failed", "stored credential is invalid")
    })?;
    let nonce = BASE64.decode(nonce).map_err(|_| {
        AppError::service_unavailable(
            "credential_decode_failed",
            "stored credential nonce is invalid",
        )
    })?;
    if nonce.len() != 12 {
        return Err(AppError::service_unavailable(
            "credential_decode_failed",
            "stored credential nonce has invalid length",
        ));
    }
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| {
            AppError::service_unavailable(
                "credential_decrypt_failed",
                "stored credential cannot be decrypted with current key",
            )
        })?;
    String::from_utf8(plaintext).map_err(|_| {
        AppError::service_unavailable("credential_decode_failed", "stored credential is not UTF-8")
    })
}

async fn build_ssh_action_plan(
    pool: &MySqlPool,
    node_id: u64,
    action: &str,
    credential: &SshCredentialRow,
    public_base_url: Option<&str>,
) -> Result<SshActionPlan, AppError> {
    let is_root = credential.username == "root";
    let sudo = if is_root { "" } else { "sudo -S -p '' " };
    match action {
        "test_connection" => Ok(SshActionPlan {
            command_label: "测试 SSH 连接".to_string(),
            remote_command: "printf 'xaccel-ssh-ok\\n'".to_string(),
            send_password_to_stdin: false,
        }),
        "restart_node_service" => Ok(SshActionPlan {
            command_label: "重启节点服务".to_string(),
            remote_command: format!(
                "{sudo}systemctl restart xaccel-node && {sudo}systemctl is-active xaccel-node"
            ),
            send_password_to_stdin: !is_root,
        }),
        "reboot_server" => Ok(SshActionPlan {
            command_label: "重启服务器".to_string(),
            remote_command: format!("{sudo}shutdown -r +1 'xaccel control scheduled reboot'"),
            send_password_to_stdin: !is_root,
        }),
        "upgrade_node" => {
            let public_base_url = public_base_url.ok_or_else(|| {
                AppError::service_unavailable(
                    "public_base_url_missing",
                    "public base url is required for node upgrade",
                )
            })?;
            let expires_at = now_unix() + SSH_BOOTSTRAP_TTL_SEC;
            let bootstrap_token = create_bootstrap_token(pool, node_id, None, expires_at).await?;
            let bootstrap_url = format!("{public_base_url}{NODE_BOOTSTRAP_PATH}");
            Ok(SshActionPlan {
                command_label: "升级节点内核".to_string(),
                remote_command: build_remote_bootstrap_install_command(
                    DEFAULT_INSTALL_URL,
                    &bootstrap_url,
                    &bootstrap_token,
                    !is_root,
                    true,
                    None,
                ),
                send_password_to_stdin: !is_root,
            })
        }
        _ => Err(AppError::bad_request(
            "invalid_ssh_action",
            "unsupported ssh action",
        )),
    }
}

fn build_remote_bootstrap_install_command(
    install_url: &str,
    bootstrap_url: &str,
    bootstrap_token: &str,
    use_sudo: bool,
    enable_control_plane: bool,
    channel: Option<&str>,
) -> String {
    let runner = if use_sudo {
        "sudo -S -p '' bash"
    } else {
        "bash"
    };
    let install_url = shell_quote(install_url);
    let bootstrap_url = shell_quote(bootstrap_url);
    let bootstrap_token = shell_quote(bootstrap_token);
    let mut command = format!(
        "curl -fsSL {install_url} | {runner} -s -- --bootstrap-url {bootstrap_url} --bootstrap-token {bootstrap_token}"
    );
    if enable_control_plane {
        command.push_str(" --enable-control-plane");
    }
    if let Some(channel) = channel {
        command.push_str(" --channel ");
        command.push_str(&shell_quote(channel));
    }
    command
}

fn shell_quote(value: &str) -> String {
    let mut quoted = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

async fn run_ssh_command(
    credential: &SshCredentialRow,
    password: &str,
    plan: &SshActionPlan,
) -> Result<SshCommandOutput, SshCommandError> {
    if let Some(parent) = FsPath::new(SSH_KNOWN_HOSTS_FILE).parent() {
        std::fs::create_dir_all(parent).map_err(|error| SshCommandError {
            exit_code: None,
            message: format!("failed to prepare ssh known_hosts directory: {error}"),
        })?;
    }

    let mut command = Command::new("sshpass");
    command
        .arg("-e")
        .arg("ssh")
        .arg("-o")
        .arg("BatchMode=no")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg(format!("UserKnownHostsFile={SSH_KNOWN_HOSTS_FILE}"))
        .arg("-p")
        .arg(credential.port.to_string())
        .arg(format!("{}@{}", credential.username, credential.host))
        .arg(&plan.remote_command)
        .env("SSHPASS", password)
        .stdin(if plan.send_password_to_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|error| {
        let message = if error.kind() == ErrorKind::NotFound {
            "sshpass is not installed on the control server; install sshpass and openssh-client"
                .to_string()
        } else {
            format!("failed to start ssh command: {error}")
        };
        SshCommandError {
            exit_code: None,
            message,
        }
    })?;

    if plan.send_password_to_stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let sudo_password = format!("{password}\n{password}\n{password}\n");
            tokio::spawn(async move {
                let _ = stdin.write_all(sudo_password.as_bytes()).await;
            });
        }
    }

    let output = timeout(
        Duration::from_secs(SSH_ACTION_TIMEOUT_SEC),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| SshCommandError {
        exit_code: None,
        message: format!("ssh action timed out after {SSH_ACTION_TIMEOUT_SEC}s"),
    })?
    .map_err(|error| SshCommandError {
        exit_code: None,
        message: format!("failed to wait for ssh command: {error}"),
    })?;

    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        if !combined.trim().is_empty() {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    let combined = trim_for_log(&combined, 4096);
    let exit_code = output.status.code();

    if output.status.success() {
        Ok(SshCommandOutput {
            exit_code,
            combined,
        })
    } else {
        Err(SshCommandError {
            exit_code,
            message: if combined.trim().is_empty() {
                format!("ssh command exited with status {}", output.status)
            } else {
                combined
            },
        })
    }
}

async fn wait_for_upgrade_version_check(
    pool: &MySqlPool,
    node_id: u64,
    before: &AdminNodeRow,
) -> Result<AdminSshActionVersionCheck, AppError> {
    let started = Instant::now();
    loop {
        let after = select_admin_node(pool, node_id).await?.ok_or_else(|| {
            AppError::not_found("node_not_found", "node disappeared after upgrade")
        })?;
        let waited_ms = started.elapsed().as_millis();
        let observation = build_upgrade_version_check(before, &after, waited_ms);

        if observation.report_refreshed
            || started.elapsed() >= Duration::from_secs(SSH_UPGRADE_REPORT_WAIT_SEC)
        {
            return Ok(observation);
        }

        sleep(Duration::from_secs(SSH_UPGRADE_REPORT_POLL_SEC)).await;
    }
}

fn build_upgrade_version_check(
    before: &AdminNodeRow,
    after: &AdminNodeRow,
    waited_ms: u128,
) -> AdminSshActionVersionCheck {
    let report_refreshed = report_refreshed(before.last_report_at, after.last_report_at);
    let before_version = before.kernel_version.clone();
    let after_version = after.kernel_version.clone();
    let version_changed = before_version != after_version && after_version.is_some();
    let message = if version_changed {
        format!(
            "节点版本已从 {} 变为 {}",
            version_label(before_version.as_deref()),
            version_label(after_version.as_deref())
        )
    } else if report_refreshed {
        format!(
            "节点已重新上报，但版本仍是 {}，可能当前节点内核已经是最新版本",
            version_label(after_version.as_deref())
        )
    } else {
        format!(
            "升级命令已执行，但等待 {} 秒内还没有看到新的节点上报",
            SSH_UPGRADE_REPORT_WAIT_SEC
        )
    };

    AdminSshActionVersionCheck {
        before_version,
        after_version,
        version_changed,
        report_refreshed,
        before_report_at: before.last_report_at,
        after_report_at: after.last_report_at,
        waited_ms,
        message,
    }
}

fn report_refreshed(before_report_at: Option<u64>, after_report_at: Option<u64>) -> bool {
    match (before_report_at, after_report_at) {
        (Some(before), Some(after)) => after > before,
        (None, Some(_)) => true,
        _ => false,
    }
}

fn version_label(version: Option<&str>) -> &str {
    version
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("--")
}

fn validate_node_task_result_status(status: &str) -> Result<&'static str, AppError> {
    match status.trim() {
        "succeeded" => Ok("succeeded"),
        "failed" => Ok("failed"),
        _ => Err(AppError::bad_request(
            "invalid_task_status",
            "task result status must be succeeded or failed",
        )),
    }
}

fn validate_node_task_result(
    header_node_id: u64,
    path_task_id: u64,
    request: &NodeTaskResultRequest,
) -> Result<(), AppError> {
    if request.node_id != header_node_id {
        return Err(AppError::bad_request(
            "node_id_mismatch",
            "header node id does not match task result body",
        ));
    }
    if request.task_id != path_task_id {
        return Err(AppError::bad_request(
            "task_id_mismatch",
            "path task id does not match task result body",
        ));
    }
    if request.task_id == 0 {
        return Err(AppError::bad_request(
            "invalid_task_id",
            "task_id must be positive",
        ));
    }
    Ok(())
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

async fn authenticate_node_request(
    pool: &MySqlPool,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<u64, AppError> {
    let node_id = required_header_u64(headers, "X-Node-Id")?;
    let timestamp = required_header_u64(headers, "X-Node-Timestamp")?;
    let nonce = required_header(headers, "X-Node-Nonce")?;
    let body_sha256 = required_header(headers, "X-Node-Body-Sha256")?;
    let signature = required_header(headers, "X-Node-Signature")?;

    validate_node_report_timestamp(timestamp)?;

    let node_secret = select_node_secret(pool, node_id)
        .await?
        .ok_or_else(|| AppError::unauthorized("unknown_node", "node is not registered"))?;
    verify_node_signature(
        method,
        path,
        &node_secret,
        timestamp,
        nonce,
        body_sha256,
        signature,
        body,
    )?;
    Ok(node_id)
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

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<AdminActor, AppError> {
    let configured = state.admin_token.as_deref().ok_or_else(|| {
        AppError::service_unavailable(
            "admin_disabled",
            "admin API is disabled because XACCEL_ADMIN_TOKEN is not configured",
        )
    })?;
    let provided = admin_token_from_headers(headers)
        .ok_or_else(|| AppError::unauthorized("admin_auth_required", "admin token is required"))?;

    if constant_time_eq(configured.as_bytes(), provided.as_bytes()) {
        Ok(AdminActor::bootstrap())
    } else if provided.starts_with(&format!("{ADMIN_SESSION_PREFIX}.{ADMIN_SESSION_VERSION}.")) {
        verify_admin_session_token(configured, provided)
    } else {
        Err(AppError::unauthorized(
            "admin_auth_failed",
            "admin token is invalid",
        ))
    }
}

fn require_admin_write(state: &AppState, headers: &HeaderMap) -> Result<AdminActor, AppError> {
    let actor = require_admin(state, headers)?;
    if actor.can_write() {
        Ok(actor)
    } else {
        Err(AppError::forbidden(
            "permission_denied",
            "current admin role can only view data",
        ))
    }
}

fn require_admin_super(state: &AppState, headers: &HeaderMap) -> Result<AdminActor, AppError> {
    let actor = require_admin(state, headers)?;
    if actor.is_super_admin() {
        Ok(actor)
    } else {
        Err(AppError::forbidden(
            "permission_denied",
            "this operation requires super admin role",
        ))
    }
}

fn require_business_sync(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let configured = state.business_sync_token.as_deref().ok_or_else(|| {
        AppError::service_unavailable(
            "business_sync_disabled",
            "business sync API is disabled because XACCEL_BUSINESS_SYNC_TOKEN is not configured",
        )
    })?;
    let provided = business_sync_token_from_headers(headers).ok_or_else(|| {
        AppError::unauthorized(
            "business_sync_auth_required",
            "business sync token is required",
        )
    })?;

    if constant_time_eq(configured.as_bytes(), provided.as_bytes()) {
        Ok(())
    } else {
        Err(AppError::unauthorized(
            "business_sync_auth_failed",
            "business sync token is invalid",
        ))
    }
}

fn create_admin_session_token(
    signing_secret: &str,
    user: &AdminUserRow,
) -> Result<(String, u64), AppError> {
    let expires_at = now_unix() + ADMIN_SESSION_TTL_SEC;
    let claims = AdminSessionClaims {
        user_id: user.id,
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        role: user.role.clone(),
        exp: expires_at,
        nonce: random_url_token(18),
    };
    let payload =
        serde_json::to_vec(&claims).map_err(|error| AppError::internal(anyhow::anyhow!(error)))?;
    let payload = URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{ADMIN_SESSION_PREFIX}.{ADMIN_SESSION_VERSION}.{payload}");
    let signature = hmac_sha256_base64(signing_secret, signing_input.as_bytes())?;
    Ok((format!("{signing_input}.{signature}"), expires_at))
}

fn verify_admin_session_token(signing_secret: &str, token: &str) -> Result<AdminActor, AppError> {
    let mut parts = token.split('.');
    let prefix = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    let payload = parts.next().unwrap_or_default();
    let signature = parts.next().unwrap_or_default();
    if parts.next().is_some() || prefix != ADMIN_SESSION_PREFIX || version != ADMIN_SESSION_VERSION
    {
        return Err(AppError::unauthorized(
            "admin_auth_failed",
            "admin session token is invalid",
        ));
    }
    let signing_input = format!("{prefix}.{version}.{payload}");
    let expected = hmac_sha256_base64(signing_secret, signing_input.as_bytes())?;
    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return Err(AppError::unauthorized(
            "admin_auth_failed",
            "admin session signature is invalid",
        ));
    }
    let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| {
        AppError::unauthorized("admin_auth_failed", "admin session payload is invalid")
    })?;
    let claims = serde_json::from_slice::<AdminSessionClaims>(&payload).map_err(|_| {
        AppError::unauthorized("admin_auth_failed", "admin session payload is invalid")
    })?;
    if claims.exp < now_unix() {
        return Err(AppError::unauthorized(
            "admin_session_expired",
            "admin session has expired",
        ));
    }
    Ok(AdminActor {
        id: Some(claims.user_id),
        username: claims.username,
        display_name: claims.display_name,
        role: claims.role,
        auth_type: "password".to_string(),
    })
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

fn business_sync_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    if let Some(value) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
    {
        if let Some(token) = value.strip_prefix("Bearer ") {
            return Some(token.trim());
        }
    }

    headers
        .get("X-Business-Sync-Token")
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

fn normalize_admin_username(value: &str) -> Result<String, AppError> {
    let value = normalize_required_text(value, "username", 64)?.to_lowercase();
    if value.len() < 3
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(AppError::bad_request(
            "invalid_username",
            "username must be 3-64 characters and may only contain letters, numbers, dot, underscore, or dash",
        ));
    }
    Ok(value)
}

fn normalize_admin_display_name(value: Option<&str>) -> Result<Option<String>, AppError> {
    normalize_optional_text(value, 128)
}

fn validate_admin_password(value: &str) -> Result<(), AppError> {
    if value.chars().count() < 8 || value.chars().count() > 128 {
        return Err(AppError::bad_request(
            "invalid_password",
            "password must be 8-128 characters",
        ));
    }
    Ok(())
}

fn validate_admin_role(value: &str) -> Result<&'static str, AppError> {
    match value.trim() {
        "super_admin" => Ok("super_admin"),
        "operator" => Ok("operator"),
        "viewer" => Ok("viewer"),
        _ => Err(AppError::bad_request(
            "invalid_admin_role",
            "admin role must be super_admin, operator, or viewer",
        )),
    }
}

fn validate_admin_user_status(value: &str) -> Result<&'static str, AppError> {
    match value.trim() {
        "active" => Ok("active"),
        "disabled" => Ok("disabled"),
        _ => Err(AppError::bad_request(
            "invalid_admin_status",
            "admin user status must be active or disabled",
        )),
    }
}

fn trim_trailing_slash(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn trim_for_log(value: &str, max_chars: usize) -> String {
    let mut output = value.trim().chars().take(max_chars).collect::<String>();
    if value.trim().chars().count() > max_chars {
        output.push_str("...");
    }
    output
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

fn random_url_token(bytes_len: usize) -> String {
    let mut bytes = vec![0_u8; bytes_len];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hmac_sha256_base64(secret: &str, data: &[u8]) -> Result<String, AppError> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
        .map_err(|error| AppError::internal(anyhow::anyhow!(error)))?;
    mac.update(data);
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn hash_admin_password(password: &str) -> Result<String, AppError> {
    validate_admin_password(password)?;
    let mut salt = [0_u8; 16];
    OsRng.fill_bytes(&mut salt);
    let derived = pbkdf2_hmac_sha256(password.as_bytes(), &salt, ADMIN_PASSWORD_ITERATIONS, 32)?;
    Ok(format!(
        "{ADMIN_PASSWORD_SCHEME}${ADMIN_PASSWORD_ITERATIONS}${}${}",
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(derived)
    ))
}

fn verify_admin_password(password: &str, encoded: &str) -> Result<bool, AppError> {
    let parts = encoded.split('$').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != ADMIN_PASSWORD_SCHEME {
        return Ok(false);
    }
    let iterations = parts[1]
        .parse::<u32>()
        .map_err(|_| AppError::internal(anyhow::anyhow!("invalid admin password hash")))?;
    let salt = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| AppError::internal(anyhow::anyhow!("invalid admin password salt")))?;
    let expected = URL_SAFE_NO_PAD
        .decode(parts[3])
        .map_err(|_| AppError::internal(anyhow::anyhow!("invalid admin password digest")))?;
    let actual = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations, expected.len())?;
    Ok(constant_time_eq(&actual, &expected))
}

fn pbkdf2_hmac_sha256(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    output_len: usize,
) -> Result<Vec<u8>, AppError> {
    if iterations == 0 || output_len == 0 {
        return Err(AppError::internal(anyhow::anyhow!(
            "invalid pbkdf2 parameters"
        )));
    }
    let mut output = Vec::with_capacity(output_len);
    let blocks = output_len.div_ceil(32);
    for block_index in 1..=blocks {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(password)
            .map_err(|error| AppError::internal(anyhow::anyhow!(error)))?;
        mac.update(salt);
        mac.update(&(block_index as u32).to_be_bytes());
        let mut u = mac.finalize().into_bytes().to_vec();
        let mut block = u.clone();

        for _ in 1..iterations {
            let mut mac = <HmacSha256 as Mac>::new_from_slice(password)
                .map_err(|error| AppError::internal(anyhow::anyhow!(error)))?;
            mac.update(&u);
            u = mac.finalize().into_bytes().to_vec();
            for (left, right) in block.iter_mut().zip(u.iter()) {
                *left ^= *right;
            }
        }
        output.extend_from_slice(&block);
    }
    output.truncate(output_len);
    Ok(output)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    value.chars().take(max_chars).collect()
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

fn map_game_write_error(error: sqlx::Error) -> AppError {
    match error {
        sqlx::Error::Database(error) if error.code().as_deref() == Some("1062") => {
            AppError::conflict("game_exists", "a game with the same game_id already exists")
        }
        error => AppError::database(error),
    }
}

fn map_route_rule_write_error(error: sqlx::Error) -> AppError {
    match error {
        sqlx::Error::Database(error) if error.code().as_deref() == Some("1062") => {
            AppError::conflict(
                "route_rule_exists",
                "a route rule with the same business sync source and external_id already exists",
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

    fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
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
            ssh_credential: AdminSshCredentialSummary {
                configured: row.ssh_host.is_some(),
                host: row.ssh_host,
                port: row.ssh_port,
                username: row.ssh_username,
                auth_status: row.ssh_auth_status,
                last_error: row.ssh_last_error,
                last_checked_at: row.ssh_last_checked_at,
            },
        }
    }
}

impl AdminSshCredentialSummary {
    fn from_ssh_row(row: Option<&SshCredentialRow>) -> Self {
        let Some(row) = row else {
            return Self {
                configured: false,
                host: None,
                port: None,
                username: None,
                auth_status: None,
                last_error: None,
                last_checked_at: None,
            };
        };
        Self {
            configured: true,
            host: Some(row.host.clone()),
            port: Some(row.port),
            username: Some(row.username.clone()),
            auth_status: Some(row.auth_status.clone()),
            last_error: row.last_error.clone(),
            last_checked_at: row.last_checked_at,
        }
    }
}

impl AdminNodeTaskSummary {
    fn from_row(row: NodeTaskRow) -> Self {
        Self {
            id: row.id,
            node_id: row.node_id,
            task_type: row.task_type,
            status: row.status,
            message: row.message,
            output: row.output,
            error_message: row.error_message,
            created_at: row.created_at,
            claimed_at: row.claimed_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
        }
    }
}

impl AdminOperationTaskSummary {
    fn from_row(row: OperationTaskRow) -> Self {
        let version_check = row
            .version_check_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<AdminSshActionVersionCheck>(value).ok());
        Self {
            id: row.id,
            node_id: row.node_id,
            node_name: row.node_name,
            node_endpoint: format!("{}:{}", row.node_server_ip, row.node_server_port),
            action_label: operation_action_label(&row.action).to_string(),
            action: row.action,
            executor: row.executor,
            status: row.status,
            command_label: row.command_label,
            exit_code: row.exit_code,
            duration_ms: row.duration_ms,
            output: row.output,
            error_message: row.error_message,
            version_check,
            created_at: row.created_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
        }
    }
}

impl AdminHealthAlertSummary {
    fn from_row(row: HealthAlertRow) -> Self {
        Self {
            id: row.id,
            node_id: row.node_id,
            node_name: row.node_name,
            node_endpoint: format!("{}:{}", row.node_server_ip, row.node_server_port),
            alert_key: row.alert_key,
            severity: row.severity,
            title: row.title,
            message: row.message,
            status: row.status,
            first_seen_at: row.first_seen_at,
            last_seen_at: row.last_seen_at,
            acknowledged_at: row.acknowledged_at,
            acknowledged_by: row.acknowledged_by,
            resolved_at: row.resolved_at,
            updated_at: row.updated_at,
        }
    }
}

fn health_alert_counts(alerts: &[AdminHealthAlertSummary]) -> AdminHealthAlertCounts {
    let mut counts = AdminHealthAlertCounts::default();
    for alert in alerts {
        match alert.status.as_str() {
            "open" => counts.open += 1,
            "acknowledged" => counts.acknowledged += 1,
            "ignored" => counts.ignored += 1,
            "resolved" => counts.resolved += 1,
            _ => {}
        }
        match alert.severity.as_str() {
            "critical" => counts.critical += 1,
            "warning" => counts.warning += 1,
            _ => {}
        }
    }
    counts
}

impl AdminUserSummary {
    fn from_row(row: AdminUserRow) -> Self {
        Self {
            id: row.id,
            username: row.username,
            display_name: row.display_name,
            role: row.role,
            status: row.status,
            last_login_at: row.last_login_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl AdminActor {
    fn bootstrap() -> Self {
        Self {
            id: None,
            username: "admin-token".to_string(),
            display_name: Some("控制面令牌".to_string()),
            role: "super_admin".to_string(),
            auth_type: "admin_token".to_string(),
        }
    }

    fn can_write(&self) -> bool {
        self.role == "super_admin" || self.role == "operator"
    }

    fn is_super_admin(&self) -> bool {
        self.role == "super_admin"
    }

    fn audit_actor_type(&self) -> &'static str {
        if self.id.is_some() {
            "admin_user"
        } else {
            "admin_api"
        }
    }

    fn current_user(&self) -> AdminCurrentUser {
        AdminCurrentUser {
            id: self.id,
            username: self.username.clone(),
            display_name: self.display_name.clone(),
            role: self.role.clone(),
            auth_type: self.auth_type.clone(),
        }
    }
}

impl NodeTaskItem {
    fn from_row(row: NodeTaskRow) -> Self {
        Self {
            task_id: row.id,
            task_type: row.task_type,
            status: row.status,
            message: row.message,
            created_at: row.created_at,
        }
    }
}

impl AdminGameSummary {
    fn from_row(row: AdminGameRow) -> Self {
        Self {
            id: row.id,
            game_id: row.game_id,
            name: row.name,
            platform: row.platform,
            category: row.category,
            icon_url: row.icon_url,
            status: row.status,
            remark: row.remark,
            route_count: row.route_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl AdminRouteRuleSummary {
    fn from_row(row: AdminRouteRuleRow) -> Self {
        Self {
            id: row.id,
            game_id: row.game_id,
            game_name: row.game_name,
            region_id: row.region_id,
            region_name: row.region_name,
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
            sync_source: row.sync_source,
            external_id: row.external_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl CandidateSchedulerInfo {
    fn from_candidate_row(row: &CandidateRow, now: u64) -> Self {
        let latest_report_age_sec = row
            .latest_reported_at
            .map(|reported_at| now.saturating_sub(reported_at));
        Self {
            route_priority: row.route_priority,
            latest_active_sessions: row.latest_active_sessions.unwrap_or_default(),
            latest_udp_sessions: row.latest_udp_sessions.unwrap_or_default(),
            latest_tcp_sessions: row.latest_tcp_sessions.unwrap_or_default(),
            latest_reported_at: row.latest_reported_at,
            latest_report_age_sec,
            report_fresh: latest_report_age_sec.is_some_and(|age| age <= 90),
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
            node_name: row.node_name,
            node_endpoint: format!("{}:{}", row.node_server_ip, row.node_server_port),
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
            region_id: None,
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

    fn valid_admin_node_row() -> AdminNodeRow {
        AdminNodeRow {
            id: 1,
            name: "node-1".to_string(),
            server_ip: "103.201.131.99".to_string(),
            server_port: 666,
            relay_server_ip: None,
            relay_server_port: None,
            is_support_ipv6: 0,
            area: "UNKNOWN".to_string(),
            tag: Some("test".to_string()),
            bandwidth_quality: "fast".to_string(),
            disable_quic: 0,
            telecom_ip: None,
            mobile_ip: None,
            unicom_ip: None,
            status: "online".to_string(),
            kernel_version: Some("0.34.0".to_string()),
            config_revision: 1,
            last_seen_at: None,
            last_report_at: None,
            latest_report_id: None,
            latest_report_status: None,
            latest_active_sessions: None,
            latest_udp_sessions: None,
            latest_tcp_sessions: None,
            latest_reported_at: None,
            latest_report_raw_json: None,
            ssh_host: None,
            ssh_port: None,
            ssh_username: None,
            ssh_auth_status: None,
            ssh_last_error: None,
            ssh_last_checked_at: None,
        }
    }

    fn valid_route_rule_request() -> AdminRouteRuleRequest {
        AdminRouteRuleRequest {
            game_id: 8888,
            game_name: "Local Echo Test".to_string(),
            region_id: None,
            region_name: None,
            node_id: 1,
            target_addr: "127.0.0.1:7777".to_string(),
            protocol: Some("udp".to_string()),
            area: Some("UNKNOWN".to_string()),
            tag: Some("test".to_string()),
            priority: Some(90),
            status: Some("enabled".to_string()),
        }
    }

    fn valid_game_request() -> AdminGameRequest {
        AdminGameRequest {
            game_id: 8888,
            name: "Local Echo Test".to_string(),
            platform: Some("pc".to_string()),
            category: Some("测试".to_string()),
            icon_url: Some("https://example.com/game.png".to_string()),
            status: Some("enabled".to_string()),
            remark: Some("UDP echo route".to_string()),
        }
    }

    #[test]
    fn validates_connect_intent_request() {
        validate_connect_intent_request(&valid_request()).expect("request is valid");
    }

    #[test]
    fn validates_connectivity_diagnostic_request() {
        let request = AdminConnectivityDiagnosticRequest {
            user_id: 1001,
            device_id: "pc-001".to_string(),
            game_id: 8888,
            region_id: None,
            platform: Some("pc".to_string()),
            client_isp: Some("telecom".to_string()),
            client_ip: Some("127.0.0.1".to_string()),
            bandwidth_quality: Some("fast".to_string()),
            payload: Some("hello".to_string()),
            timeout_sec: Some(3),
            response_timeout_ms: Some(500),
            candidate_index: Some(0),
            skip_session_data: Some(false),
        };
        validate_connectivity_diagnostic_request(&request).expect("diagnostic request is valid");

        let invalid = AdminConnectivityDiagnosticRequest {
            timeout_sec: Some(0),
            ..request
        };
        let error = validate_connectivity_diagnostic_request(&invalid).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_timeout");
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
    fn rejects_zero_region_connect_intent() {
        let mut request = valid_request();
        request.region_id = Some(0);

        let error = validate_connect_intent_request(&request).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_region");
    }

    #[test]
    fn signs_xat_v1_token() {
        let claims = ClientTokenClaims {
            node_id: 1,
            user_id: 1001,
            device_id: "pc-001".to_string(),
            game_id: 8888,
            region_id: None,
            intent_id: Some("intent-test".to_string()),
            route: Some(ClientRouteClaims {
                target_addr: "127.0.0.1:7777".to_string(),
                protocol: "udp".to_string(),
                region_id: None,
                region_name: None,
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
        let body = br#"{"node_id":1,"node_version":"0.29.0","os":"linux","arch":"x86_64","boot_id":"boot-1","timestamp":1779250000,"nonce":"handshake-nonce","config_revision":1,"listen_addr":"0.0.0.0:666"}"#;
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
            node_version: "0.29.0".to_string(),
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
    fn validates_admin_node_task_type() {
        assert_eq!(
            validate_admin_node_task_type("restart_node").expect("task type"),
            "restart_node"
        );

        let error = validate_admin_node_task_type("shell").unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_task_type");
    }

    #[test]
    fn validates_ssh_action_whitelist() {
        for action in [
            "test_connection",
            "restart_node_service",
            "upgrade_node",
            "reboot_server",
        ] {
            assert_eq!(validate_ssh_action(action).expect("ssh action"), action);
        }

        let error = validate_ssh_action("shell").unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_ssh_action");
    }

    #[test]
    fn truncates_stored_errors_on_character_boundaries() {
        let value = "连接失败".repeat(8);
        let truncated = truncate_chars(&value, 5);

        assert_eq!(truncated, "连接失败连");
        assert_eq!(truncate_chars("short", 4096), "short");
    }

    #[test]
    fn validates_operation_task_status() {
        for status in ["running", "succeeded", "failed"] {
            assert_eq!(
                validate_operation_task_status(status).expect("operation task status"),
                status
            );
        }

        let error = validate_operation_task_status("pending").unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_operation_task_status");
    }

    #[test]
    fn parses_operation_task_version_check_summary() {
        let row = OperationTaskRow {
            id: 7,
            node_id: 2,
            node_name: "香港节点".to_string(),
            node_server_ip: "47.83.160.126".to_string(),
            node_server_port: 666,
            action: "upgrade_node".to_string(),
            executor: "control_ssh".to_string(),
            status: "succeeded".to_string(),
            command_label: "升级节点内核".to_string(),
            exit_code: Some(0),
            duration_ms: Some(5151),
            output: Some("installed".to_string()),
            error_message: None,
            version_check_json: Some(
                serde_json::json!({
                    "before_version": "0.35.0",
                    "after_version": "0.36.0",
                    "version_changed": true,
                    "report_refreshed": true,
                    "before_report_at": 1779500000_u64,
                    "after_report_at": 1779500030_u64,
                    "waited_ms": 3000_u128,
                    "message": "节点版本已更新"
                })
                .to_string(),
            ),
            created_at: Some(1779500000),
            started_at: Some(1779500000),
            finished_at: Some(1779500005),
        };

        let summary = AdminOperationTaskSummary::from_row(row);
        assert_eq!(summary.id, 7);
        assert_eq!(summary.node_endpoint, "47.83.160.126:666");
        assert_eq!(summary.action_label, "升级节点内核");
        assert_eq!(
            summary
                .version_check
                .as_ref()
                .unwrap()
                .after_version
                .as_deref(),
            Some("0.36.0")
        );
        assert!(summary.version_check.unwrap().version_changed);
    }

    #[test]
    fn normalizes_ssh_credential_request() {
        let node = valid_admin_node_row();
        let credential = normalize_ssh_credential_request(
            &node,
            AdminSshCredentialRequest {
                host: None,
                port: None,
                username: "root".to_string(),
                password: "secret-password".to_string(),
            },
        )
        .expect("ssh credential is valid");

        assert_eq!(credential.host, "103.201.131.99");
        assert_eq!(credential.port, 22);
        assert_eq!(credential.username, "root");
        assert_eq!(credential.password, "secret-password");
    }

    #[test]
    fn rejects_invalid_ssh_credential_fields() {
        let node = valid_admin_node_row();
        let bad_host = normalize_ssh_credential_request(
            &node,
            AdminSshCredentialRequest {
                host: Some(";rm -rf /".to_string()),
                port: Some(22),
                username: "root".to_string(),
                password: "secret-password".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(bad_host.status, StatusCode::BAD_REQUEST);
        assert_eq!(bad_host.code, "invalid_ssh_host");

        let bad_user = normalize_ssh_credential_request(
            &node,
            AdminSshCredentialRequest {
                host: Some("103.201.131.99".to_string()),
                port: Some(22),
                username: "-oProxyCommand".to_string(),
                password: "secret-password".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(bad_user.status, StatusCode::BAD_REQUEST);
        assert_eq!(bad_user.code, "invalid_ssh_username");
    }

    #[test]
    fn encrypts_and_decrypts_credential_secret() {
        let key = [7u8; 32];
        let (ciphertext, nonce) =
            encrypt_credential_secret(&key, "secret-password").expect("credential encrypts");

        assert_ne!(ciphertext, "secret-password");
        assert_eq!(BASE64.decode(&nonce).expect("nonce decodes").len(), 12);

        let plaintext =
            decrypt_credential_secret(&key, &ciphertext, &nonce).expect("credential decrypts");
        assert_eq!(plaintext, "secret-password");
    }

    #[test]
    fn builds_upgrade_version_check_for_unchanged_refreshed_report() {
        let mut before = valid_admin_node_row();
        before.kernel_version = Some("0.33.0".to_string());
        before.last_report_at = Some(1_779_500_000);
        let mut after = valid_admin_node_row();
        after.kernel_version = Some("0.33.0".to_string());
        after.last_report_at = Some(1_779_500_030);

        let check = build_upgrade_version_check(&before, &after, 30_000);

        assert!(!check.version_changed);
        assert!(check.report_refreshed);
        assert_eq!(check.before_version.as_deref(), Some("0.33.0"));
        assert_eq!(check.after_version.as_deref(), Some("0.33.0"));
        assert!(check.message.contains("版本仍是 0.33.0"));
    }

    #[test]
    fn builds_upgrade_version_check_for_changed_version() {
        let mut before = valid_admin_node_row();
        before.kernel_version = Some("0.33.0".to_string());
        before.last_report_at = Some(1_779_500_000);
        let mut after = valid_admin_node_row();
        after.kernel_version = Some("0.35.0".to_string());
        after.last_report_at = Some(1_779_500_030);

        let check = build_upgrade_version_check(&before, &after, 12_000);

        assert!(check.version_changed);
        assert!(check.report_refreshed);
        assert_eq!(check.before_version.as_deref(), Some("0.33.0"));
        assert_eq!(check.after_version.as_deref(), Some("0.35.0"));
        assert!(check.message.contains("0.33.0"));
        assert!(check.message.contains("0.35.0"));
    }

    #[test]
    fn validates_node_task_result_request() {
        let request = NodeTaskResultRequest {
            node_id: 2,
            task_id: 9,
            status: "succeeded".to_string(),
            message: None,
            output: None,
            started_at: Some(1_779_500_000),
            finished_at: Some(1_779_500_001),
        };

        validate_node_task_result(2, 9, &request).expect("task result is valid");
        assert_eq!(
            validate_node_task_result_status(&request.status).expect("status"),
            "succeeded"
        );

        let node_error = validate_node_task_result(3, 9, &request).unwrap_err();
        assert_eq!(node_error.status, StatusCode::BAD_REQUEST);
        assert_eq!(node_error.code, "node_id_mismatch");

        let task_error = validate_node_task_result(2, 10, &request).unwrap_err();
        assert_eq!(task_error.status, StatusCode::BAD_REQUEST);
        assert_eq!(task_error.code, "task_id_mismatch");

        let status_error = validate_node_task_result_status("running").unwrap_err();
        assert_eq!(status_error.status, StatusCode::BAD_REQUEST);
        assert_eq!(status_error.code, "invalid_task_status");
    }

    #[test]
    fn parses_admin_audit_log_detail() {
        let row = AdminAuditLogRow {
            id: 9,
            node_id: 2,
            node_name: "香港节点".to_string(),
            node_server_ip: "47.83.160.126".to_string(),
            node_server_port: 666,
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
        assert_eq!(detail.node_name, "香港节点");
        assert_eq!(detail.node_endpoint, "47.83.160.126:666");
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
    fn hashes_and_verifies_admin_passwords() {
        let hash = hash_admin_password("strong-password-2026").expect("password hashes");

        assert!(verify_admin_password("strong-password-2026", &hash).expect("password verifies"));
        assert!(!verify_admin_password("wrong-password", &hash).expect("wrong password rejects"));

        let error = hash_admin_password("short").unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_password");
    }

    #[test]
    fn creates_and_verifies_admin_session_tokens() {
        let user = AdminUserRow {
            id: 42,
            username: "ops-admin".to_string(),
            display_name: Some("运维管理员".to_string()),
            password_hash: "hash".to_string(),
            role: "operator".to_string(),
            status: "active".to_string(),
            last_login_at: None,
            created_at: Some(1_779_500_000),
            updated_at: Some(1_779_500_000),
        };

        let (token, expires_at) =
            create_admin_session_token("signing-secret", &user).expect("token is created");
        assert!(token.starts_with("xas.v1."));
        assert!(expires_at > now_unix());

        let actor = verify_admin_session_token("signing-secret", &token).expect("token verifies");
        assert_eq!(actor.id, Some(42));
        assert_eq!(actor.username, "ops-admin");
        assert_eq!(actor.role, "operator");
        assert!(actor.can_write());
        assert!(!actor.is_super_admin());

        let error = verify_admin_session_token("other-secret", &token).unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED);
        assert_eq!(error.code, "admin_auth_failed");
    }

    #[test]
    fn dashboard_contains_admin_login_and_permissions_ui() {
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/login"));
        assert!(ADMIN_DASHBOARD_HTML.contains("loginUsernameInput"));
        assert!(ADMIN_DASHBOARD_HTML.contains("权限管理"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/admin-users"));
    }

    #[test]
    fn reads_business_sync_token_header() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Business-Sync-Token", "sync-secret".parse().unwrap());
        assert_eq!(
            business_sync_token_from_headers(&headers),
            Some("sync-secret")
        );
    }

    #[test]
    fn normalizes_business_sync_catalog() {
        let request = BusinessSyncCatalogRequest {
            source: Some("billing".to_string()),
            revision: Some("rev-1".to_string()),
            games: vec![BusinessSyncGame {
                game_id: 8888,
                name: "Local Echo Test".to_string(),
                platform: Some("pc".to_string()),
                category: Some("test".to_string()),
                icon_url: None,
                status: Some("enabled".to_string()),
                remark: None,
            }],
            regions: vec![BusinessSyncRegion {
                game_id: 8888,
                region_id: 1,
                name: "Default Region".to_string(),
                area: Some("UNKNOWN".to_string()),
                status: Some("enabled".to_string()),
                remark: None,
            }],
            route_rules: vec![BusinessSyncRouteRule {
                external_id: Some("route-8888-default".to_string()),
                game_id: 8888,
                game_name: Some("Local Echo Test".to_string()),
                region_id: Some(1),
                region_name: Some("Default Region".to_string()),
                node_id: 1,
                target_addr: "127.0.0.1:7777".to_string(),
                protocol: Some("udp".to_string()),
                area: Some("UNKNOWN".to_string()),
                tag: Some("standalone".to_string()),
                priority: Some(10),
                status: Some("enabled".to_string()),
            }],
        };

        let catalog = normalize_business_sync_catalog(request).expect("catalog");
        assert_eq!(catalog.source, "billing");
        assert_eq!(catalog.revision.as_deref(), Some("rev-1"));
        assert_eq!(catalog.games[0].name, "Local Echo Test");
        assert_eq!(catalog.regions[0].region_id, 1);
        assert_eq!(catalog.route_rules[0].region_id, Some(1));
        assert_eq!(
            catalog.route_rules[0].sync_source.as_deref(),
            Some("billing")
        );
        assert_eq!(
            catalog.route_rules[0].external_id.as_deref(),
            Some("route-8888-default")
        );
    }

    #[test]
    fn builds_candidate_scheduler_info() {
        let row = CandidateRow {
            node_id: 1,
            server_ip: "103.201.131.99".to_string(),
            server_port: 666,
            area: "UNKNOWN".to_string(),
            tag: Some("standalone".to_string()),
            bandwidth_quality: "fast".to_string(),
            node_secret: "secret".to_string(),
            target_addr: "127.0.0.1:7777".to_string(),
            protocol: "udp".to_string(),
            region_id: Some(1),
            region_name: Some("Default Region".to_string()),
            route_priority: 10,
            latest_active_sessions: Some(5),
            latest_udp_sessions: Some(4),
            latest_tcp_sessions: Some(1),
            latest_reported_at: Some(1_000),
        };

        let scheduler = CandidateSchedulerInfo::from_candidate_row(&row, 1_030);

        assert_eq!(scheduler.route_priority, 10);
        assert_eq!(scheduler.latest_active_sessions, 5);
        assert_eq!(scheduler.latest_report_age_sec, Some(30));
        assert!(scheduler.report_fresh);
    }

    #[test]
    fn generates_business_route_external_id_when_missing() {
        let request = BusinessSyncRouteRule {
            external_id: None,
            game_id: 8888,
            game_name: Some("Local Echo Test".to_string()),
            region_id: Some(1),
            region_name: Some("Default Region".to_string()),
            node_id: 2,
            target_addr: "127.0.0.1:7777".to_string(),
            protocol: Some("udp".to_string()),
            area: None,
            tag: None,
            priority: None,
            status: None,
        };

        let first = normalize_business_route_rule(&request, "billing").expect("route is valid");
        let second = normalize_business_route_rule(&request, "billing").expect("route is valid");

        assert_eq!(first.external_id, second.external_id);
        assert!(first
            .external_id
            .as_deref()
            .unwrap_or_default()
            .starts_with("route-8888-1-"));
    }

    #[test]
    fn embeds_admin_dashboard_html() {
        assert!(ADMIN_DASHBOARD_HTML.contains("XAccel 控制台"));
        assert!(ADMIN_DASHBOARD_HTML.contains("登录节点控制台"));
        assert!(ADMIN_DASHBOARD_HTML.contains("新增节点"));
        assert!(ADMIN_DASHBOARD_HTML.contains("编辑配置"));
        assert!(ADMIN_DASHBOARD_HTML.contains("控制总览"));
        assert!(ADMIN_DASHBOARD_HTML.contains("游戏管理"));
        assert!(ADMIN_DASHBOARD_HTML.contains("游戏路由"));
        assert!(ADMIN_DASHBOARD_HTML.contains("操作日志"));
        assert!(ADMIN_DASHBOARD_HTML.contains("系统设置"));
        assert!(ADMIN_DASHBOARD_HTML.contains("部署和维护命令"));
        assert!(ADMIN_DASHBOARD_HTML.contains("canWrite()"));
        assert!(ADMIN_DASHBOARD_HTML.contains("readonlyActionsCell"));
        assert!(ADMIN_DASHBOARD_HTML.contains("assertCanWrite"));
        assert!(ADMIN_DASHBOARD_HTML.contains("assertSuperAdmin"));
        assert!(ADMIN_DASHBOARD_HTML.contains("renderPermissionMatrix"));
        assert!(ADMIN_DASHBOARD_HTML.contains("permissionMatrix"));
        assert!(ADMIN_DASHBOARD_HTML.contains("overviewAlertRows"));
        assert!(ADMIN_DASHBOARD_HTML.contains("nodeHealthAlerts"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-alert-filter=\"critical\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-view=\"alerts\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("healthAlertRows"));
        assert!(ADMIN_DASHBOARD_HTML.contains("healthAlertPager"));
        assert!(ADMIN_DASHBOARD_HTML.contains("loadHealthAlerts"));
        assert!(ADMIN_DASHBOARD_HTML.contains("updateHealthAlert"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/health-alerts"));
        assert!(ADMIN_DASHBOARD_HTML.contains("node.health_alert.update"));
        assert!(!ADMIN_DASHBOARD_HTML.contains("CONTROL_DASHBOARD_VERSION"));
        assert!(!ADMIN_DASHBOARD_HTML.contains("版本落后"));
        assert!(ADMIN_DASHBOARD_HTML.contains("账号管理只对超级管理员开放"));
        assert!(ADMIN_DASHBOARD_HTML.contains("showPermissionError"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-write-action"));
        assert!(ADMIN_DASHBOARD_HTML.contains("当前账号不能"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-view=\"audit\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("auditRows"));
        assert!(ADMIN_DASHBOARD_HTML.contains("loadAuditLogs"));
        assert!(ADMIN_DASHBOARD_HTML.contains("auditActionFilter"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/audit-logs"));
        assert!(ADMIN_DASHBOARD_HTML.contains("恢复调度"));
        assert!(ADMIN_DASHBOARD_HTML.contains("调度诊断"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-node-action"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-node-action=\"edit\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-node-action=\"deploy\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-node-action=\"edit-area\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-node-action=\"edit-tag\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-node-action=\"delete\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-node-action=\"restart\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-detail-restart"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-resume-node"));
        assert!(ADMIN_DASHBOARD_HTML.contains("restartNode"));
        assert!(ADMIN_DASHBOARD_HTML.contains("createNodeTask"));
        assert!(ADMIN_DASHBOARD_HTML.contains("node.task.create"));
        assert!(ADMIN_DASHBOARD_HTML.contains("node.task.result"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/nodes/${nodeId}/tasks"));
        assert!(ADMIN_DASHBOARD_HTML.contains("服务器账号控制"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-action=\"ssh-credential\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-ssh-action=\"test_connection\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-ssh-action=\"restart_node_service\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-ssh-action=\"upgrade_node\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-ssh-action=\"reboot_server\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-delete-ssh"));
        assert!(ADMIN_DASHBOARD_HTML.contains("批量升级节点"));
        assert!(ADMIN_DASHBOARD_HTML.contains("batchUpgradeModal"));
        assert!(ADMIN_DASHBOARD_HTML.contains("batchSelectAllNodes"));
        assert!(ADMIN_DASHBOARD_HTML.contains("selectUpgradeableNodes"));
        assert!(ADMIN_DASHBOARD_HTML.contains("startBatchUpgrade"));
        assert!(ADMIN_DASHBOARD_HTML.contains("batchUpgradeFilter"));
        assert!(ADMIN_DASHBOARD_HTML.contains("batchUpgradeFilters"));
        assert!(ADMIN_DASHBOARD_HTML.contains("batchCopySummary"));
        assert!(ADMIN_DASHBOARD_HTML.contains("batchTaskPicker"));
        assert!(ADMIN_DASHBOARD_HTML.contains("batchTaskAction"));
        assert!(ADMIN_DASHBOARD_HTML.contains("nodeBatchEligibility"));
        assert!(ADMIN_DASHBOARD_HTML.contains("runBatchTaskNode"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-batch-action=\"test_connection\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-batch-action=\"restart_node_service\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-batch-action=\"resume_scheduling\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("batchUpgradeSummaryText"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-batch-filter=\"pending\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("retryBatchUpgradeNode"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-batch-retry"));
        assert!(ADMIN_DASHBOARD_HTML.contains("nodeUpgradeEligibility"));
        assert!(ADMIN_DASHBOARD_HTML.contains("ssh-credential"));
        assert!(ADMIN_DASHBOARD_HTML.contains("ssh-actions"));
        assert!(ADMIN_DASHBOARD_HTML.contains("版本检查"));
        assert!(ADMIN_DASHBOARD_HTML.contains("sshVersionCheckLines"));
        assert!(ADMIN_DASHBOARD_HTML.contains("运维任务中心"));
        assert!(ADMIN_DASHBOARD_HTML.contains("opsTaskRows"));
        assert!(ADMIN_DASHBOARD_HTML.contains("operationStatusText"));
        assert!(ADMIN_DASHBOARD_HTML.contains("operationTaskSummary"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/operation-tasks"));
        assert!(ADMIN_DASHBOARD_HTML.contains("renderPager"));
        assert!(ADMIN_DASHBOARD_HTML.contains("paginatedItems"));
        assert!(ADMIN_DASHBOARD_HTML.contains("nodePager"));
        assert!(ADMIN_DASHBOARD_HTML.contains("gamePager"));
        assert!(ADMIN_DASHBOARD_HTML.contains("routePager"));
        assert!(ADMIN_DASHBOARD_HTML.contains("opsTaskPager"));
        assert!(ADMIN_DASHBOARD_HTML.contains("auditPager"));
        assert!(ADMIN_DASHBOARD_HTML.contains("adminUserPager"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-page-action"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/nodes/${nodeId}/deploy"));
        assert!(ADMIN_DASHBOARD_HTML.contains("deployNodeViaSsh"));
        assert!(ADMIN_DASHBOARD_HTML.contains("deploySshHost"));
        assert!(ADMIN_DASHBOARD_HTML.contains("deploySshPassword"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-generate-deploy-command"));
        assert!(ADMIN_DASHBOARD_HTML.contains("createNodeMeta"));
        assert!(ADMIN_DASHBOARD_HTML.contains("submitCreateNode"));
        assert!(ADMIN_DASHBOARD_HTML.contains("openEditNodeModal"));
        assert!(ADMIN_DASHBOARD_HTML.contains("saveNode"));
        assert!(ADMIN_DASHBOARD_HTML.contains("deployNodeModal"));
        assert!(ADMIN_DASHBOARD_HTML.contains("openDeployNodeModal"));
        assert!(ADMIN_DASHBOARD_HTML.contains("controlUpgradeCommand"));
        assert!(ADMIN_DASHBOARD_HTML.contains("systemInstallMysqlCommand"));
        assert!(ADMIN_DASHBOARD_HTML.contains("systemUninstallCommand"));
        assert!(ADMIN_DASHBOARD_HTML.contains("systemPurgeCommand"));
        assert!(ADMIN_DASHBOARD_HTML.contains("--init-mysql"));
        assert!(ADMIN_DASHBOARD_HTML.contains("control-api-uninstall.sh"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-copy-system-command"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-readonly-only"));
        assert!(ADMIN_DASHBOARD_HTML.contains("控制面自检"));
        assert!(ADMIN_DASHBOARD_HTML.contains("runSystemDiagnostics"));
        assert!(ADMIN_DASHBOARD_HTML.contains("systemDiagnosticActions"));
        assert!(ADMIN_DASHBOARD_HTML.contains("handleSystemDiagnosticAction"));
        assert!(ADMIN_DASHBOARD_HTML.contains("openAttentionNodeFromDiagnostics"));
        assert!(ADMIN_DASHBOARD_HTML.contains("openSuggestedRouteFromDiagnostics"));
        assert!(ADMIN_DASHBOARD_HTML.contains("openBatchUpgradeFromDiagnostics"));
        assert!(ADMIN_DASHBOARD_HTML.contains("data-system-fix"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/system/diagnostics"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/nodes"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/games"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/game-route-rules"));
        assert!(ADMIN_DASHBOARD_HTML.contains("/api/admin/v1/connectivity-diagnostic"));
        assert!(ADMIN_DASHBOARD_HTML.contains("method: \"PATCH\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("method: \"DELETE\""));
        assert!(ADMIN_DASHBOARD_HTML.contains("bootstrap-token"));
    }

    #[test]
    fn system_diagnostics_use_current_core_table_names() {
        assert!(SYSTEM_DIAGNOSTIC_CORE_TABLES.contains(&"node_operation_tasks"));
        assert!(!SYSTEM_DIAGNOSTIC_CORE_TABLES.contains(&"operation_tasks"));
        assert_eq!(SYSTEM_DIAGNOSTIC_CORE_TABLES.len(), 6);
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
    fn normalizes_game_request() {
        let game = normalize_game_request(&valid_game_request()).expect("game is valid");

        assert_eq!(game.game_id, 8888);
        assert_eq!(game.name, "Local Echo Test");
        assert_eq!(game.platform, "pc");
        assert_eq!(game.category.as_deref(), Some("测试"));
        assert_eq!(game.status, "enabled");
    }

    #[test]
    fn rejects_invalid_game_platform() {
        let mut request = valid_game_request();
        request.platform = Some("console".to_string());

        let error = normalize_game_request(&request).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_game_platform");
    }

    #[test]
    fn rejects_invalid_game_icon_url() {
        let mut request = valid_game_request();
        request.icon_url = Some("ftp://example.com/game.png".to_string());

        let error = normalize_game_request(&request).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_url");
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
    fn health_alerts_do_not_compare_node_kernel_to_control_panel_version() {
        let mut node = valid_admin_node_row();
        node.kernel_version = Some("0.33.0".to_string());
        node.last_report_at = Some(2_000);
        node.latest_report_status = Some("ready".to_string());
        node.latest_report_raw_json = Some(
            serde_json::json!({
                "health": {
                    "listeners": {
                        "udp_listening": true,
                        "tcp_listening": true
                    }
                }
            })
            .to_string(),
        );

        let alerts = build_node_health_alert_specs(&node, 2_020);

        assert!(alerts.iter().all(|alert| alert.key != "version_lag"));
        assert!(alerts.is_empty());
    }

    #[test]
    fn health_alerts_detect_listener_down_from_latest_report() {
        let mut node = valid_admin_node_row();
        node.last_report_at = Some(2_000);
        node.latest_report_status = Some("ready".to_string());
        node.latest_report_raw_json = Some(
            serde_json::json!({
                "health": {
                    "listeners": {
                        "udp_listening": true,
                        "tcp_listening": false
                    }
                }
            })
            .to_string(),
        );

        let alerts = build_node_health_alert_specs(&node, 2_020);

        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].key, "listener_down");
        assert_eq!(alerts[0].severity, "critical");
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

    #[test]
    fn quotes_remote_bootstrap_install_command_args() {
        assert_eq!(
            shell_quote("http://install.test/a'b"),
            "'http://install.test/a'\\''b'"
        );

        let command = build_remote_bootstrap_install_command(
            "http://install.test/a'b",
            "http://control.test/bootstrap?x=1;touch /tmp/pwn",
            "xbt.token",
            false,
            true,
            None,
        );

        assert_eq!(
            command,
            "curl -fsSL 'http://install.test/a'\\''b' | bash -s -- --bootstrap-url 'http://control.test/bootstrap?x=1;touch /tmp/pwn' --bootstrap-token 'xbt.token' --enable-control-plane"
        );

        let command = build_remote_bootstrap_install_command(
            "http://install.test/install.sh",
            "http://control.test/bootstrap",
            "xbt.token",
            true,
            false,
            Some("beta"),
        );
        assert_eq!(
            command,
            "curl -fsSL 'http://install.test/install.sh' | sudo -S -p '' bash -s -- --bootstrap-url 'http://control.test/bootstrap' --bootstrap-token 'xbt.token' --channel 'beta'"
        );
    }
}
