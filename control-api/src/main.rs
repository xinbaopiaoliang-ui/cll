use anyhow::{bail, Context};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use clap::Parser;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
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
}

#[derive(Clone)]
struct AppState {
    pool: MySqlPool,
    token_ttl_sec: u64,
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
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/client/v1/connect-intent", post(connect_intent))
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
}
