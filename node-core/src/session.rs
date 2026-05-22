use crate::{
    auth::{verify_probe_token, AuthDecision, ClientTokenClaims},
    session_store::{UdpSession, UdpSessionError},
    state::RuntimeState,
};
use anyhow::Context;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{lookup_host, UdpSocket},
    time::{timeout, Duration},
};

pub const PROTOCOL_VERSION: &str = "xaccel/1";
const PROBE_TTL_SEC: u64 = 30;
const DEFAULT_RELAY_TIMEOUT_MS: u64 = 200;
const MAX_RELAY_TIMEOUT_MS: u64 = 1000;
const MAX_RELAY_RESPONSE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Tcp,
    Udp,
}

impl TransportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ClientProbeRequest {
    pub client_nonce: Option<String>,
    pub user_id: Option<u64>,
    pub device_id: Option<String>,
    pub game_id: Option<u64>,
    pub transport: Option<String>,
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ClientSessionDataRequest {
    pub session_id: Option<String>,
    pub client_nonce: Option<String>,
    pub payload: Option<String>,
    pub target_addr: Option<String>,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    pub response_timeout_ms: Option<u64>,
}

#[derive(Debug)]
pub enum ParsedClientMessage {
    LegacyPing,
    Probe(ClientProbeRequest),
    SessionData(ClientSessionDataRequest),
    Invalid(String),
}

#[derive(Debug, Serialize)]
struct ClientProbeResponse {
    #[serde(rename = "type")]
    message_type: &'static str,
    protocol: &'static str,
    node_id: Option<u64>,
    node_version: &'static str,
    server_time: u64,
    transport: TransportKind,
    requested_transport: Option<String>,
    client_nonce: Option<String>,
    session: ProbeSession,
    capabilities: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct ProbeSession {
    session_id: String,
    status: &'static str,
    ttl_sec: u64,
    intent_id: Option<String>,
    route_target_addr: Option<String>,
    auth_required: bool,
    credential_present: bool,
    credential_valid: bool,
    credential_expires_at: Option<u64>,
    user_id: Option<u64>,
    device_id: Option<String>,
    game_id: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ClientSessionDataResponse {
    #[serde(rename = "type")]
    message_type: &'static str,
    protocol: &'static str,
    node_id: Option<u64>,
    node_version: &'static str,
    server_time: u64,
    transport: TransportKind,
    session_id: String,
    client_nonce: Option<String>,
    status: &'static str,
    payload: String,
    payload_bytes: u64,
    request_payload_bytes: u64,
    target: Option<SessionTargetInfo>,
    relay: Option<RelayInfo>,
    session: SessionDataInfo,
}

#[derive(Debug, Serialize)]
struct SessionTargetInfo {
    address: String,
}

#[derive(Debug, Serialize)]
struct RelayInfo {
    mode: &'static str,
    timeout_ms: u64,
    timed_out: bool,
    upstream_tx_bytes: u64,
    upstream_rx_bytes: u64,
}

#[derive(Debug, Serialize)]
struct SessionDataInfo {
    created_at: u64,
    expires_at: u64,
    authenticated: bool,
    intent_id: Option<String>,
    route_target_addr: Option<String>,
    user_id: Option<u64>,
    device_id: Option<String>,
    game_id: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ClientError {
    #[serde(rename = "type")]
    message_type: &'static str,
    protocol: &'static str,
    node_version: &'static str,
    server_time: u64,
    transport: TransportKind,
    error: ProbeErrorBody,
}

#[derive(Debug, Serialize)]
struct ProbeErrorBody {
    code: &'static str,
    message: String,
}

struct ProbeIdentity {
    credential_present: bool,
    credential_valid: bool,
    credential_expires_at: Option<u64>,
    intent_id: Option<String>,
    route_target_addr: Option<String>,
    user_id: Option<u64>,
    device_id: Option<String>,
    game_id: Option<u64>,
}

pub fn parse_client_message(payload: &[u8]) -> ParsedClientMessage {
    let Ok(text) = std::str::from_utf8(payload) else {
        return ParsedClientMessage::Invalid("payload must be UTF-8".to_string());
    };

    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("ping") {
        return ParsedClientMessage::LegacyPing;
    }

    let Ok(header) = serde_json::from_str::<MessageHeader>(trimmed) else {
        return ParsedClientMessage::Invalid("expected JSON request".to_string());
    };

    if !header
        .protocol
        .as_deref()
        .is_some_and(|protocol| protocol == PROTOCOL_VERSION)
    {
        return ParsedClientMessage::Invalid(format!("protocol must be {PROTOCOL_VERSION}"));
    }

    match header.message_type.as_deref() {
        Some("probe") => match serde_json::from_str::<ClientProbeRequest>(trimmed) {
            Ok(request) => ParsedClientMessage::Probe(request),
            Err(_) => ParsedClientMessage::Invalid("invalid probe request".to_string()),
        },
        Some("session.data") => match serde_json::from_str::<ClientSessionDataRequest>(trimmed) {
            Ok(request) => ParsedClientMessage::SessionData(request),
            Err(_) => ParsedClientMessage::Invalid("invalid session.data request".to_string()),
        },
        Some(_) => ParsedClientMessage::Invalid("type must be probe or session.data".to_string()),
        None => ParsedClientMessage::Invalid("type is required".to_string()),
    }
}

pub fn build_probe_response(
    state: &RuntimeState,
    transport: TransportKind,
    peer: SocketAddr,
    request: ClientProbeRequest,
) -> anyhow::Result<Vec<u8>> {
    let identity = match verify_probe_token(
        &request,
        state.identity().node_id,
        state.identity().node_secret(),
    ) {
        AuthDecision::Missing => {
            state.stats().record_auth_missing();
            ProbeIdentity::from_request(&request)
        }
        AuthDecision::Valid(claims) => {
            state.stats().record_auth_ok();
            ProbeIdentity::from_claims(claims)
        }
        AuthDecision::Invalid { code, message } => {
            state.stats().record_auth_failed();
            return build_probe_error_with_code(state, transport, code, message);
        }
    };

    let sequence = state.stats().next_probe_sequence();
    let session_id = build_session_id(transport, peer, sequence);
    state.stats().record_probe_session(session_id.clone());
    if transport == TransportKind::Udp {
        state.sessions().register_udp_session(UdpSession::new(
            session_id.clone(),
            identity.user_id,
            identity.device_id.clone(),
            identity.game_id,
            identity.credential_valid,
            identity.intent_id.clone(),
            identity.route_target_addr.clone(),
            PROBE_TTL_SEC,
            peer,
        ));
    }

    let response = ClientProbeResponse {
        message_type: "probe.ok",
        protocol: PROTOCOL_VERSION,
        node_id: state.identity().node_id,
        node_version: env!("CARGO_PKG_VERSION"),
        server_time: now_unix(),
        transport,
        requested_transport: request.transport,
        client_nonce: request.client_nonce,
        session: ProbeSession {
            session_id,
            status: "probe_only",
            ttl_sec: PROBE_TTL_SEC,
            intent_id: identity.intent_id,
            route_target_addr: identity.route_target_addr,
            auth_required: true,
            credential_present: identity.credential_present,
            credential_valid: identity.credential_valid,
            credential_expires_at: identity.credential_expires_at,
            user_id: identity.user_id,
            device_id: identity.device_id,
            game_id: identity.game_id,
        },
        capabilities: vec![
            "tcp_probe",
            "udp_probe",
            "token_auth_hmac_v1",
            "udp_session_echo",
            "udp_target_relay",
            "connect_intent_route",
            "session_stats",
        ],
    };

    encode_json_line(&response)
}

pub async fn build_session_data_response(
    state: &RuntimeState,
    transport: TransportKind,
    peer: SocketAddr,
    request: ClientSessionDataRequest,
) -> anyhow::Result<Vec<u8>> {
    if transport != TransportKind::Udp {
        state.stats().record_udp_session_miss();
        return build_session_error(
            state,
            transport,
            "unsupported_transport",
            "session.data is UDP-only",
        );
    }

    let Some(session_id) = request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
    else {
        state.stats().record_udp_session_miss();
        return build_session_error(
            state,
            transport,
            "missing_session_id",
            "session_id is required",
        );
    };

    let Some(payload) = request.payload.as_deref() else {
        state.stats().record_udp_session_miss();
        return build_session_error(state, transport, "missing_payload", "payload is required");
    };

    let payload = payload.to_string();
    let payload_bytes = match BASE64.decode(payload.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => {
            state.stats().record_udp_session_miss();
            return build_session_error(
                state,
                transport,
                "invalid_payload",
                "payload must be base64",
            );
        }
    };

    let session = match state.sessions().record_udp_session_io(
        session_id,
        peer,
        payload_bytes.len() as u64,
        0,
    ) {
        Ok(session) => session,
        Err(UdpSessionError::Missing | UdpSessionError::LockPoisoned) => {
            state.stats().record_udp_session_miss();
            return build_session_error(
                state,
                transport,
                "session_not_found",
                "session_id not found",
            );
        }
        Err(UdpSessionError::Expired) => {
            state.stats().record_udp_session_expired();
            return build_session_error(state, transport, "session_expired", "session expired");
        }
    };

    let mut status = "echo";
    let mut response_payload_bytes = payload_bytes.clone();
    let mut response_payload = payload;
    let mut target_info = None;
    let mut relay_info = None;

    if (has_session_target(&request) || session.route_target_addr.is_some())
        && !session.authenticated
    {
        state.stats().record_udp_relay_error();
        return build_session_error(
            state,
            transport,
            "relay_auth_required",
            "target relay requires a valid client token",
        );
    }

    let target = match resolve_session_target(&request, session.route_target_addr.as_deref()).await
    {
        Ok(target) => target,
        Err(error) => {
            state.stats().record_udp_relay_error();
            return build_session_error(state, transport, error.code, error.message);
        }
    };

    if let Some(target) = target {
        let timeout_ms = clamp_relay_timeout(request.response_timeout_ms);
        let relay = match relay_udp_payload(target, &payload_bytes, timeout_ms).await {
            Ok(relay) => relay,
            Err(error) => {
                state.stats().record_udp_relay_error();
                return build_session_error(
                    state,
                    transport,
                    "relay_error",
                    format!("udp relay failed: {error}"),
                );
            }
        };

        state.stats().record_udp_relay_tx(relay.upstream_tx_bytes);
        target_info = Some(SessionTargetInfo {
            address: target.to_string(),
        });
        relay_info = Some(RelayInfo {
            mode: "udp_target",
            timeout_ms,
            timed_out: relay.timed_out,
            upstream_tx_bytes: relay.upstream_tx_bytes,
            upstream_rx_bytes: relay.payload.len() as u64,
        });

        if relay.timed_out {
            state.stats().record_udp_relay_timeout();
            status = "upstream_timeout";
            response_payload_bytes = Vec::new();
            response_payload = String::new();
        } else {
            state
                .stats()
                .record_udp_relay_rx(relay.payload.len() as u64);
            status = "forwarded";
            response_payload = BASE64.encode(&relay.payload);
            response_payload_bytes = relay.payload;
        }
    }

    let response = ClientSessionDataResponse {
        message_type: "session.data.ok",
        protocol: PROTOCOL_VERSION,
        node_id: state.identity().node_id,
        node_version: env!("CARGO_PKG_VERSION"),
        server_time: now_unix(),
        transport,
        session_id: session.session_id.clone(),
        client_nonce: request.client_nonce,
        status,
        payload: response_payload,
        payload_bytes: response_payload_bytes.len() as u64,
        request_payload_bytes: payload_bytes.len() as u64,
        target: target_info,
        relay: relay_info,
        session: SessionDataInfo {
            created_at: session.created_at,
            expires_at: session.expires_at,
            authenticated: session.authenticated,
            intent_id: session.intent_id,
            route_target_addr: session.route_target_addr,
            user_id: session.user_id,
            device_id: session.device_id,
            game_id: session.game_id,
        },
    };
    let encoded = encode_json_line(&response)?;
    let _ = state
        .sessions()
        .record_udp_session_io(session_id, peer, 0, encoded.len() as u64);
    state
        .stats()
        .record_udp_session_rx(payload_bytes.len() as u64);
    state.stats().record_udp_session_tx(encoded.len() as u64);

    Ok(encoded)
}

#[derive(Debug)]
struct TargetResolveError {
    code: &'static str,
    message: String,
}

struct RelayOutcome {
    payload: Vec<u8>,
    upstream_tx_bytes: u64,
    timed_out: bool,
}

fn has_session_target(request: &ClientSessionDataRequest) -> bool {
    request
        .target_addr
        .as_deref()
        .is_some_and(|target_addr| !target_addr.trim().is_empty())
        || request
            .target_host
            .as_deref()
            .is_some_and(|target_host| !target_host.trim().is_empty())
}

async fn resolve_session_target(
    request: &ClientSessionDataRequest,
    session_target_addr: Option<&str>,
) -> Result<Option<SocketAddr>, TargetResolveError> {
    if let Some(session_target_addr) = session_target_addr
        .map(str::trim)
        .filter(|session_target_addr| !session_target_addr.is_empty())
    {
        return resolve_socket_addr(session_target_addr).await.map(Some);
    }

    if let Some(target_addr) = request
        .target_addr
        .as_deref()
        .map(str::trim)
        .filter(|target_addr| !target_addr.is_empty())
    {
        return resolve_socket_addr(target_addr).await.map(Some);
    }

    let Some(target_host) = request
        .target_host
        .as_deref()
        .map(str::trim)
        .filter(|target_host| !target_host.is_empty())
    else {
        return Ok(None);
    };

    let Some(target_port) = request.target_port else {
        return Err(TargetResolveError {
            code: "missing_target_port",
            message: "target_port is required when target_host is provided".to_string(),
        });
    };

    let endpoint = match target_host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{target_host}]:{target_port}"),
        _ => format!("{target_host}:{target_port}"),
    };

    resolve_socket_addr(&endpoint).await.map(Some)
}

async fn resolve_socket_addr(endpoint: &str) -> Result<SocketAddr, TargetResolveError> {
    let mut resolved = lookup_host(endpoint)
        .await
        .map_err(|error| TargetResolveError {
            code: "invalid_target",
            message: format!("invalid target endpoint: {error}"),
        })?;

    resolved.next().ok_or_else(|| TargetResolveError {
        code: "invalid_target",
        message: "target endpoint did not resolve".to_string(),
    })
}

fn clamp_relay_timeout(timeout_ms: Option<u64>) -> u64 {
    timeout_ms
        .unwrap_or(DEFAULT_RELAY_TIMEOUT_MS)
        .clamp(1, MAX_RELAY_TIMEOUT_MS)
}

async fn relay_udp_payload(
    target: SocketAddr,
    payload: &[u8],
    timeout_ms: u64,
) -> std::io::Result<RelayOutcome> {
    let bind_addr = if target.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let socket = UdpSocket::bind(bind_addr).await?;
    socket.connect(target).await?;
    let sent = socket.send(payload).await?;

    let mut buf = vec![0_u8; MAX_RELAY_RESPONSE_BYTES];
    match timeout(Duration::from_millis(timeout_ms), socket.recv(&mut buf)).await {
        Ok(Ok(size)) => {
            buf.truncate(size);
            Ok(RelayOutcome {
                payload: buf,
                upstream_tx_bytes: sent as u64,
                timed_out: false,
            })
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(RelayOutcome {
            payload: Vec::new(),
            upstream_tx_bytes: sent as u64,
            timed_out: true,
        }),
    }
}

pub fn build_probe_error(
    state: &RuntimeState,
    transport: TransportKind,
    message: String,
) -> anyhow::Result<Vec<u8>> {
    build_probe_error_with_code(state, transport, "invalid_probe", message)
}

fn build_probe_error_with_code(
    state: &RuntimeState,
    transport: TransportKind,
    code: &'static str,
    message: String,
) -> anyhow::Result<Vec<u8>> {
    state.stats().record_probe_rejected();
    build_client_error(transport, "probe.error", code, message)
}

fn build_session_error(
    _state: &RuntimeState,
    transport: TransportKind,
    code: &'static str,
    message: impl Into<String>,
) -> anyhow::Result<Vec<u8>> {
    build_client_error(transport, "session.error", code, message.into())
}

fn build_client_error(
    transport: TransportKind,
    message_type: &'static str,
    code: &'static str,
    message: String,
) -> anyhow::Result<Vec<u8>> {
    let response = ClientError {
        message_type,
        protocol: PROTOCOL_VERSION,
        node_version: env!("CARGO_PKG_VERSION"),
        server_time: now_unix(),
        transport,
        error: ProbeErrorBody { code, message },
    };

    encode_json_line(&response)
}

impl ProbeIdentity {
    fn from_request(request: &ClientProbeRequest) -> Self {
        Self {
            credential_present: false,
            credential_valid: false,
            credential_expires_at: None,
            intent_id: None,
            route_target_addr: None,
            user_id: request.user_id,
            device_id: request.device_id.clone(),
            game_id: request.game_id,
        }
    }

    fn from_claims(claims: ClientTokenClaims) -> Self {
        Self {
            credential_present: true,
            credential_valid: true,
            credential_expires_at: Some(claims.expires_at),
            intent_id: claims.intent_id,
            route_target_addr: claims.route.map(|route| route.target_addr),
            user_id: Some(claims.user_id),
            device_id: Some(claims.device_id),
            game_id: Some(claims.game_id),
        }
    }
}

#[derive(Debug, Deserialize)]
struct MessageHeader {
    #[serde(rename = "type")]
    message_type: Option<String>,
    protocol: Option<String>,
}

fn encode_json_line(value: &impl Serialize) -> anyhow::Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(value).context("failed to encode probe response")?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn build_session_id(transport: TransportKind, peer: SocketAddr, sequence: u64) -> String {
    let peer = peer.to_string().replace(':', "-").replace('.', "-");
    format!(
        "ps-{}-{}-{}-{sequence}",
        transport.as_str(),
        now_unix(),
        peer
    )
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
    fn parses_legacy_ping() {
        assert!(matches!(
            parse_client_message(b"ping\n"),
            ParsedClientMessage::LegacyPing
        ));
    }

    #[test]
    fn parses_probe_request() {
        let payload = br#"{"type":"probe","protocol":"xaccel/1","client_nonce":"n1","game_id":88}"#;
        let ParsedClientMessage::Probe(request) = parse_client_message(payload) else {
            panic!("expected probe request");
        };

        assert_eq!(request.client_nonce.as_deref(), Some("n1"));
        assert_eq!(request.game_id, Some(88));
    }

    #[test]
    fn parses_session_data_request() {
        let payload = br#"{"type":"session.data","protocol":"xaccel/1","session_id":"s1","client_nonce":"d1","payload":"aGVsbG8=","target_host":"127.0.0.1","target_port":7777,"response_timeout_ms":50}"#;
        let ParsedClientMessage::SessionData(request) = parse_client_message(payload) else {
            panic!("expected session.data request");
        };

        assert_eq!(request.session_id.as_deref(), Some("s1"));
        assert_eq!(request.client_nonce.as_deref(), Some("d1"));
        assert_eq!(request.payload.as_deref(), Some("aGVsbG8="));
        assert_eq!(request.target_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(request.target_port, Some(7777));
        assert_eq!(request.response_timeout_ms, Some(50));
    }

    #[tokio::test]
    async fn relays_udp_payload_to_target() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = server.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut buf = [0_u8; 64];
            let (size, peer) = server.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..size], b"hello");
            server.send_to(b"upstream:hello", peer).await.unwrap();
        });

        let outcome = relay_udp_payload(target, b"hello", 500).await.unwrap();

        assert!(!outcome.timed_out);
        assert_eq!(outcome.upstream_tx_bytes, 5);
        assert_eq!(outcome.payload, b"upstream:hello");
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn resolves_session_bound_target_before_request_target() {
        let request = ClientSessionDataRequest {
            session_id: Some("s1".to_string()),
            client_nonce: None,
            payload: Some("aGVsbG8=".to_string()),
            target_addr: Some("127.0.0.1:9999".to_string()),
            target_host: None,
            target_port: None,
            response_timeout_ms: None,
        };

        let target = resolve_session_target(&request, Some("127.0.0.1:7777"))
            .await
            .expect("target resolves")
            .expect("target exists");

        assert_eq!(target.port(), 7777);
    }

    #[test]
    fn rejects_wrong_protocol() {
        let payload = br#"{"type":"probe","protocol":"bad"}"#;
        let ParsedClientMessage::Invalid(message) = parse_client_message(payload) else {
            panic!("expected invalid request");
        };

        assert!(message.contains(PROTOCOL_VERSION));
    }
}
