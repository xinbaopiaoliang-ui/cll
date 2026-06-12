use anyhow::{bail, Context};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use clap::Parser;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

type HmacSha256 = Hmac<Sha256>;

const TOKEN_PREFIX: &str = "xat";
const TOKEN_VERSION: &str = "v1";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Parser)]
#[command(name = "xaccel-backend-mock")]
#[command(about = "Development backend for XAccel connect-intent tokens")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:18080")]
    listen: SocketAddr,

    #[arg(long, default_value_t = 1)]
    node_id: u64,

    #[arg(long, env = "XACCEL_NODE_SECRET")]
    node_secret: String,

    #[arg(long, default_value = "127.0.0.1")]
    node_host: String,

    #[arg(long, default_value_t = 666)]
    node_port: u16,

    #[arg(long, default_value = "127.0.0.1:7777")]
    target_addr: String,

    #[arg(long, default_value = "UNKNOWN")]
    area: String,

    #[arg(long, default_value = "standalone")]
    tag: String,

    #[arg(long, default_value_t = 120)]
    ttl_sec: u64,
}

#[derive(Clone)]
struct AppState {
    node_id: u64,
    node_secret: Arc<str>,
    node_host: Arc<str>,
    node_port: u16,
    target_addr: Arc<str>,
    area: Arc<str>,
    tag: Arc<str>,
    ttl_sec: u64,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    business: Option<BusinessAuthContext>,
    intent_id: Option<String>,
    route: Option<ClientRouteClaims>,
    expires_at: u64,
    issued_at: Option<u64>,
    nonce: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BusinessAuthContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    entitlement_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscription_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    business_session_id: Option<String>,
    entitlement_verified: bool,
    device_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    entitlement_expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    risk_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    business_trace_id: Option<String>,
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
    node_id: u64,
    node_host: String,
    node_port: u16,
    target_addr: String,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    validate_cli(&cli)?;

    let state = AppState {
        node_id: cli.node_id,
        node_secret: Arc::from(cli.node_secret),
        node_host: Arc::from(cli.node_host),
        node_port: cli.node_port,
        target_addr: Arc::from(cli.target_addr),
        area: Arc::from(cli.area),
        tag: Arc::from(cli.tag),
        ttl_sec: cli.ttl_sec.max(1),
    };

    let listener = TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("failed to bind backend mock {}", cli.listen))?;
    println!("xaccel-backend-mock {VERSION} listening on {}", cli.listen);

    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, state).await {
                eprintln!("backend request failed: {error:?}");
            }
        });
    }
}

fn validate_cli(cli: &Cli) -> anyhow::Result<()> {
    if cli.node_secret.trim().is_empty() {
        bail!("--node-secret or XACCEL_NODE_SECRET is required");
    }
    if cli.node_host.trim().is_empty() {
        bail!("--node-host is required");
    }
    if cli.target_addr.trim().is_empty() {
        bail!("--target-addr is required");
    }
    Ok(())
}

async fn handle_connection(mut stream: TcpStream, state: AppState) -> anyhow::Result<()> {
    let request = read_http_request(&mut stream).await?;
    let response = route_request(&request, &state).unwrap_or_else(error_response);
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn route_request(request: &HttpRequest, state: &AppState) -> anyhow::Result<String> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => json_response(
            "200 OK",
            &HealthResponse {
                status: "ready",
                version: VERSION,
                node_id: state.node_id,
                node_host: state.node_host.to_string(),
                node_port: state.node_port,
                target_addr: state.target_addr.to_string(),
            },
        ),
        ("POST", "/api/client/v1/connect-intent") => {
            let request: ConnectIntentRequest =
                serde_json::from_slice(&request.body).context("invalid connect-intent JSON")?;
            json_response("200 OK", &build_connect_intent(request, state)?)
        }
        _ => json_response(
            "404 Not Found",
            &ErrorBody {
                error: ErrorMessage {
                    code: "not_found",
                    message: "route not found".to_string(),
                },
            },
        ),
    }
}

fn build_connect_intent(
    request: ConnectIntentRequest,
    state: &AppState,
) -> anyhow::Result<ConnectIntentResponse> {
    validate_connect_intent_request(&request)?;

    let issued_at = now_unix();
    let expires_at = issued_at + state.ttl_sec;
    let intent_id = format!(
        "intent-{}-{}-{}-{}",
        request.user_id, request.game_id, issued_at, state.node_id
    );
    let route = ClientRouteClaims {
        target_addr: state.target_addr.to_string(),
        protocol: "udp".to_string(),
    };
    let claims = ClientTokenClaims {
        node_id: state.node_id,
        user_id: request.user_id,
        device_id: request.device_id.clone(),
        game_id: request.game_id,
        business: None,
        intent_id: Some(intent_id.clone()),
        route: Some(route.clone()),
        expires_at,
        issued_at: Some(issued_at),
        nonce: Some(format!("{}-{}", issued_at, request.device_id)),
    };
    let token = sign_client_token(&claims, &state.node_secret)?;

    let bandwidth_quality = request
        .bandwidth_quality
        .clone()
        .unwrap_or_else(|| "normal".to_string());
    let client = ClientContext {
        platform: request.platform.clone(),
        client_isp: request.client_isp.clone(),
        client_ip: request.client_ip.clone(),
        bandwidth_quality: bandwidth_quality.clone(),
    };

    Ok(ConnectIntentResponse {
        intent_id: intent_id.clone(),
        ttl_sec: state.ttl_sec,
        client,
        candidates: vec![NodeCandidate {
            node_id: state.node_id,
            area: state.area.to_string(),
            tag: state.tag.to_string(),
            host: state.node_host.to_string(),
            port: state.node_port,
            transports: vec!["udp"],
            bandwidth_quality,
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

fn validate_connect_intent_request(request: &ConnectIntentRequest) -> anyhow::Result<()> {
    if request.user_id == 0 {
        bail!("user_id must be positive");
    }
    if request.device_id.trim().is_empty() {
        bail!("device_id is required");
    }
    if request.game_id == 0 {
        bail!("game_id must be positive");
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

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

async fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<HttpRequest> {
    let mut buffer = Vec::new();
    let mut scratch = [0_u8; 1024];

    loop {
        let read = stream.read(&mut scratch).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&scratch[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            bail!("request too large");
        }

        if let Some(header_end) = find_header_end(&buffer) {
            let content_length = parse_content_length(&buffer[..header_end])?;
            let total = header_end + 4 + content_length;
            while buffer.len() < total {
                let read = stream.read(&mut scratch).await?;
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&scratch[..read]);
                if buffer.len() > MAX_REQUEST_BYTES {
                    bail!("request too large");
                }
            }
            return parse_http_request(&buffer[..total], header_end, content_length);
        }
    }

    bail!("empty or incomplete HTTP request")
}

fn parse_http_request(
    buffer: &[u8],
    header_end: usize,
    content_length: usize,
) -> anyhow::Result<HttpRequest> {
    let headers = std::str::from_utf8(&buffer[..header_end]).context("headers must be UTF-8")?;
    let mut lines = headers.lines();
    let request_line = lines.next().context("missing request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().context("missing HTTP method")?.to_string();
    let path = parts
        .next()
        .context("missing HTTP path")?
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let body_start = header_end + 4;
    let body = buffer[body_start..body_start + content_length].to_vec();

    Ok(HttpRequest { method, path, body })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> anyhow::Result<usize> {
    let headers = std::str::from_utf8(headers).context("headers must be UTF-8")?;
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .context("invalid Content-Length");
        }
    }
    Ok(0)
}

fn json_response(status: &str, value: &impl Serialize) -> anyhow::Result<String> {
    let body = serde_json::to_string_pretty(value)?;
    Ok(format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    ))
}

fn error_response(error: anyhow::Error) -> String {
    json_response(
        "400 Bad Request",
        &ErrorBody {
            error: ErrorMessage {
                code: "bad_request",
                message: error.to_string(),
            },
        },
    )
    .expect("error response serializes")
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
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn state() -> AppState {
        AppState {
            node_id: 1,
            node_secret: Arc::from("secret"),
            node_host: Arc::from("103.201.131.99"),
            node_port: 666,
            target_addr: Arc::from("127.0.0.1:7777"),
            area: Arc::from("UNKNOWN"),
            tag: Arc::from("standalone"),
            ttl_sec: 120,
        }
    }

    #[test]
    fn issues_connect_intent_with_route_token() {
        let response = build_connect_intent(
            ConnectIntentRequest {
                user_id: 1001,
                device_id: "pc-001".to_string(),
                game_id: 8888,
                platform: Some("pc".to_string()),
                client_isp: Some("telecom".to_string()),
                client_ip: Some("127.0.0.1".to_string()),
                bandwidth_quality: Some("fast".to_string()),
            },
            &state(),
        )
        .expect("intent builds");

        let candidate = response.candidates.first().expect("candidate exists");
        assert_eq!(candidate.node_id, 1);
        assert_eq!(candidate.route.target_addr, "127.0.0.1:7777");
        assert!(candidate.credential.token.starts_with("xat.v1."));

        let claims = decode_unsigned_claims(&candidate.credential.token);
        assert_eq!(
            claims.intent_id.as_deref(),
            Some(response.intent_id.as_str())
        );
        assert_eq!(
            claims
                .route
                .as_ref()
                .map(|route| route.target_addr.as_str()),
            Some("127.0.0.1:7777")
        );
    }

    #[test]
    fn rejects_empty_device() {
        let error = build_connect_intent(
            ConnectIntentRequest {
                user_id: 1001,
                device_id: " ".to_string(),
                game_id: 8888,
                platform: None,
                client_isp: None,
                client_ip: None,
                bandwidth_quality: None,
            },
            &state(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("device_id"));
    }

    fn decode_unsigned_claims(token: &str) -> ClientTokenClaims {
        let parts = token.split('.').collect::<Vec<_>>();
        assert_eq!(parts.len(), 4);
        let payload = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        serde_json::from_slice(&payload).unwrap()
    }
}
