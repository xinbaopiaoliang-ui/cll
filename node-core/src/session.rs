use crate::{
    auth::{verify_probe_token, AuthDecision, ClientTokenClaims},
    route_policy::{
        hash_route_policy, match_route_policy_target, RoutePolicy, SessionDataTarget, TargetMatch,
    },
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
    io::{AsyncReadExt, AsyncWriteExt},
    net::{lookup_host, TcpStream, UdpSocket},
    time::{timeout, Duration},
};

pub const PROTOCOL_VERSION: &str = "xaccel/1";
const RAW_UDP_TUNNEL_MAGIC: &[u8; 4] = b"XAU1";
const RAW_UDP_TUNNEL_VERSION: u8 = 1;
const RAW_UDP_KIND_PACKET: u8 = 1;
const RAW_UDP_KIND_RESPONSE: u8 = 2;
const RAW_UDP_HEADER_BYTES: usize = 20;
const PROBE_TTL_SEC: u64 = 30;
const DEFAULT_RELAY_TIMEOUT_MS: u64 = 200;
const MAX_RELAY_TIMEOUT_MS: u64 = 1000;
const MAX_RELAY_RESPONSE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Tcp,
    Udp,
    Quic,
}

impl TransportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::Quic => "quic",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ClientProbeRequest {
    pub client_nonce: Option<String>,
    pub user_id: Option<u64>,
    pub device_id: Option<String>,
    pub game_id: Option<u64>,
    pub region_id: Option<u64>,
    pub transport: Option<String>,
    pub token: Option<String>,
    pub route_policy: Option<RoutePolicy>,
}

#[derive(Debug, Deserialize)]
pub struct ClientSessionDataRequest {
    pub session_id: Option<String>,
    pub client_nonce: Option<String>,
    pub payload: Option<String>,
    pub target_addr: Option<String>,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    pub target_protocol: Option<String>,
    pub target: Option<SessionDataTarget>,
    pub response_timeout_ms: Option<u64>,
}

#[derive(Debug)]
pub enum ParsedClientMessage {
    LegacyPing,
    Probe(ClientProbeRequest),
    SessionData(ClientSessionDataRequest),
    RawUdpTunnel(RawUdpTunnelRequest),
    Invalid(String),
}

#[derive(Debug)]
pub struct RawUdpTunnelRequest {
    pub session_id: String,
    pub target_id: Option<String>,
    pub host: String,
    pub port: u16,
    pub payload: Vec<u8>,
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
    route_policy_id: Option<String>,
    route_policy_hash: Option<String>,
    auth_required: bool,
    credential_present: bool,
    credential_valid: bool,
    credential_expires_at: Option<u64>,
    user_id: Option<u64>,
    device_id: Option<String>,
    game_id: Option<u64>,
    region_id: Option<u64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    target_id: Option<String>,
    protocol: &'static str,
    address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_policy: Option<String>,
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
    route_policy_id: Option<String>,
    route_policy_hash: Option<String>,
    user_id: Option<u64>,
    device_id: Option<String>,
    game_id: Option<u64>,
    region_id: Option<u64>,
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
    route_policy: Option<RoutePolicy>,
    route_policy_id: Option<String>,
    route_policy_hash: Option<String>,
    user_id: Option<u64>,
    device_id: Option<String>,
    game_id: Option<u64>,
    region_id: Option<u64>,
}

pub fn parse_client_message(payload: &[u8]) -> ParsedClientMessage {
    if payload.starts_with(RAW_UDP_TUNNEL_MAGIC) {
        return match parse_raw_udp_tunnel_frame(payload) {
            Ok(request) => ParsedClientMessage::RawUdpTunnel(request),
            Err(message) => ParsedClientMessage::Invalid(message),
        };
    }

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
            match ProbeIdentity::from_claims(claims, request.route_policy.clone()) {
                Ok(identity) => identity,
                Err((code, message)) => {
                    state.stats().record_auth_failed();
                    return build_probe_error_with_code(state, transport, code, message);
                }
            }
        }
        AuthDecision::Invalid { code, message } => {
            state.stats().record_auth_failed();
            return build_probe_error_with_code(state, transport, code, message);
        }
    };

    let sequence = state.stats().next_probe_sequence();
    let session_id = build_session_id(transport, peer, sequence);
    state.stats().record_probe_session(session_id.clone());
    state.sessions().register_udp_session(UdpSession::new(
        session_id.clone(),
        identity.user_id,
        identity.device_id.clone(),
        identity.game_id,
        identity.region_id,
        identity.credential_valid,
        identity.intent_id.clone(),
        identity.route_target_addr.clone(),
        identity.route_policy.clone(),
        identity.route_policy_id.clone(),
        identity.route_policy_hash.clone(),
        PROBE_TTL_SEC,
        peer,
    ));

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
            route_policy_id: identity.route_policy_id,
            route_policy_hash: identity.route_policy_hash,
            auth_required: true,
            credential_present: identity.credential_present,
            credential_valid: identity.credential_valid,
            credential_expires_at: identity.credential_expires_at,
            user_id: identity.user_id,
            device_id: identity.device_id,
            game_id: identity.game_id,
            region_id: identity.region_id,
        },
        capabilities: vec![
            "tcp_probe",
            "udp_probe",
            "token_auth_hmac_v1",
            "udp_session_echo",
            "udp_target_relay",
            "tcp_target_relay",
            "connect_intent_route",
            "dynamic_route_policy",
            "port_range_target",
            "domain_target",
            "long_lived_tcp_channel",
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

    let target = match resolve_session_target(
        &request,
        session.route_target_addr.as_deref(),
        session.route_policy.as_ref(),
    )
    .await
    {
        Ok(target) => target,
        Err(error) => {
            state.stats().record_udp_relay_error();
            return build_session_error(state, transport, error.code, error.message);
        }
    };

    if let Some(target) = target {
        let timeout_ms = clamp_relay_timeout(request.response_timeout_ms);
        let relay = match target.protocol {
            RelayProtocol::Udp => {
                match relay_udp_payload(target.socket_addr, &payload_bytes, timeout_ms).await {
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
                }
            }
            RelayProtocol::Tcp => {
                match relay_tcp_payload(target.socket_addr, &payload_bytes, timeout_ms).await {
                    Ok(relay) => relay,
                    Err(error) => {
                        state.stats().record_udp_relay_error();
                        return build_session_error(
                            state,
                            transport,
                            "relay_error",
                            format!("tcp relay failed: {error}"),
                        );
                    }
                }
            }
        };

        state.stats().record_udp_relay_tx(relay.upstream_tx_bytes);
        target_info = Some(SessionTargetInfo {
            target_id: target
                .policy_match
                .as_ref()
                .map(|matched| matched.target_id.clone()),
            protocol: target.protocol.as_str(),
            address: target.socket_addr.to_string(),
            matched_policy: target.policy_match.map(|matched| matched.policy_id),
        });
        relay_info = Some(RelayInfo {
            mode: target.protocol.relay_mode(),
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
            route_policy_id: session.route_policy_id,
            route_policy_hash: session.route_policy_hash,
            user_id: session.user_id,
            device_id: session.device_id,
            game_id: session.game_id,
            region_id: session.region_id,
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

pub async fn build_raw_udp_tunnel_response(
    state: &RuntimeState,
    transport: TransportKind,
    peer: SocketAddr,
    request: RawUdpTunnelRequest,
) -> anyhow::Result<Vec<u8>> {
    if !matches!(transport, TransportKind::Udp | TransportKind::Quic) {
        return Ok(encode_raw_udp_tunnel_response(
            &request.session_id,
            "unsupported_transport",
            &[],
        ));
    }

    let session = match state.sessions().record_udp_session_io(
        &request.session_id,
        peer,
        request.payload.len() as u64,
        0,
    ) {
        Ok(session) => session,
        Err(UdpSessionError::Missing | UdpSessionError::LockPoisoned) => {
            state.stats().record_udp_session_miss();
            return Ok(encode_raw_udp_tunnel_response(
                &request.session_id,
                "session_not_found",
                &[],
            ));
        }
        Err(UdpSessionError::Expired) => {
            state.stats().record_udp_session_expired();
            return Ok(encode_raw_udp_tunnel_response(
                &request.session_id,
                "session_expired",
                &[],
            ));
        }
    };

    if !session.authenticated {
        state.stats().record_udp_relay_error();
        return Ok(encode_raw_udp_tunnel_response(
            &request.session_id,
            "relay_auth_required",
            &[],
        ));
    }

    let target = SessionDataTarget {
        target_id: request.target_id,
        protocol: Some("udp".to_string()),
        host: request.host,
        port: request.port,
        original_domain: None,
    };
    let target_request = ClientSessionDataRequest {
        session_id: Some(request.session_id.clone()),
        client_nonce: None,
        payload: None,
        target_addr: None,
        target_host: None,
        target_port: None,
        target_protocol: Some("udp".to_string()),
        target: Some(target),
        response_timeout_ms: Some(DEFAULT_RELAY_TIMEOUT_MS),
    };

    let target = match resolve_session_target(
        &target_request,
        session.route_target_addr.as_deref(),
        session.route_policy.as_ref(),
    )
    .await
    {
        Ok(Some(target)) => target,
        Ok(None) => {
            state.stats().record_udp_relay_error();
            return Ok(encode_raw_udp_tunnel_response(
                &request.session_id,
                "missing_target",
                &[],
            ));
        }
        Err(error) => {
            state.stats().record_udp_relay_error();
            return Ok(encode_raw_udp_tunnel_response(
                &request.session_id,
                error.code,
                &[],
            ));
        }
    };

    let relay = match relay_udp_payload(
        target.socket_addr,
        &request.payload,
        DEFAULT_RELAY_TIMEOUT_MS,
    )
    .await
    {
        Ok(relay) => relay,
        Err(_) => {
            state.stats().record_udp_relay_error();
            return Ok(encode_raw_udp_tunnel_response(
                &request.session_id,
                "relay_error",
                &[],
            ));
        }
    };

    state.stats().record_udp_relay_tx(relay.upstream_tx_bytes);
    if relay.timed_out {
        state.stats().record_udp_relay_timeout();
        return Ok(encode_raw_udp_tunnel_response(
            &request.session_id,
            "upstream_timeout",
            &[],
        ));
    }

    state
        .stats()
        .record_udp_relay_rx(relay.payload.len() as u64);
    state
        .stats()
        .record_udp_session_rx(request.payload.len() as u64);
    state
        .stats()
        .record_udp_session_tx(relay.payload.len() as u64);
    let _ = state.sessions().record_udp_session_io(
        &request.session_id,
        peer,
        0,
        relay.payload.len() as u64,
    );

    Ok(encode_raw_udp_tunnel_response(
        &request.session_id,
        "forwarded",
        &relay.payload,
    ))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayProtocol {
    Udp,
    Tcp,
}

impl RelayProtocol {
    fn parse(value: &str) -> Result<Self, TargetResolveError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "udp" => Ok(Self::Udp),
            "tcp" => Ok(Self::Tcp),
            _ => Err(TargetResolveError {
                code: "target_protocol_unsupported",
                message: "target protocol must be udp or tcp".to_string(),
            }),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
        }
    }

    fn relay_mode(self) -> &'static str {
        match self {
            Self::Udp => "udp_target",
            Self::Tcp => "tcp_target",
        }
    }
}

struct ResolvedTarget {
    protocol: RelayProtocol,
    socket_addr: SocketAddr,
    policy_match: Option<TargetMatch>,
}

fn has_session_target(request: &ClientSessionDataRequest) -> bool {
    request.target.is_some()
        || request
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
    route_policy: Option<&RoutePolicy>,
) -> Result<Option<ResolvedTarget>, TargetResolveError> {
    if let Some(route_policy) = route_policy {
        let Some(target) = request.target.as_ref() else {
            return Err(TargetResolveError {
                code: "missing_target",
                message: "target is required for dynamic route_policy sessions".to_string(),
            });
        };
        let protocol = RelayProtocol::parse(
            target
                .protocol
                .as_deref()
                .or(route_policy.default_protocol.as_deref())
                .unwrap_or("udp"),
        )?;
        let policy_match =
            match_route_policy_target(route_policy, target).map_err(|code| TargetResolveError {
                code,
                message: "target does not match route_policy".to_string(),
            })?;
        let socket_addr = resolve_host_port(&target.host, target.port).await?;
        return Ok(Some(ResolvedTarget {
            protocol,
            socket_addr,
            policy_match: Some(policy_match),
        }));
    }

    let request_protocol =
        RelayProtocol::parse(request.target_protocol.as_deref().unwrap_or("udp"))?;

    if let Some(session_target_addr) = session_target_addr
        .map(str::trim)
        .filter(|session_target_addr| !session_target_addr.is_empty())
    {
        return resolve_socket_addr(session_target_addr)
            .await
            .map(|socket_addr| {
                Some(ResolvedTarget {
                    protocol: request_protocol,
                    socket_addr,
                    policy_match: None,
                })
            });
    }

    if let Some(target_addr) = request
        .target_addr
        .as_deref()
        .map(str::trim)
        .filter(|target_addr| !target_addr.is_empty())
    {
        return resolve_socket_addr(target_addr).await.map(|socket_addr| {
            Some(ResolvedTarget {
                protocol: request_protocol,
                socket_addr,
                policy_match: None,
            })
        });
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

    resolve_host_port(target_host, target_port)
        .await
        .map(|socket_addr| {
            Some(ResolvedTarget {
                protocol: request_protocol,
                socket_addr,
                policy_match: None,
            })
        })
}

async fn resolve_host_port(host: &str, port: u16) -> Result<SocketAddr, TargetResolveError> {
    let endpoint = match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    };

    resolve_socket_addr(&endpoint).await
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

async fn relay_tcp_payload(
    target: SocketAddr,
    payload: &[u8],
    timeout_ms: u64,
) -> std::io::Result<RelayOutcome> {
    let mut stream = timeout(
        Duration::from_millis(timeout_ms),
        TcpStream::connect(target),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "tcp connect timed out"))??;
    timeout(Duration::from_millis(timeout_ms), stream.write_all(payload))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "tcp write timed out"))??;
    let upstream_tx_bytes = payload.len() as u64;
    let _ = stream.shutdown().await;

    let mut buf = vec![0_u8; MAX_RELAY_RESPONSE_BYTES];
    match timeout(Duration::from_millis(timeout_ms), stream.read(&mut buf)).await {
        Ok(Ok(size)) => {
            buf.truncate(size);
            Ok(RelayOutcome {
                payload: buf,
                upstream_tx_bytes,
                timed_out: false,
            })
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(RelayOutcome {
            payload: Vec::new(),
            upstream_tx_bytes,
            timed_out: true,
        }),
    }
}

fn parse_raw_udp_tunnel_frame(payload: &[u8]) -> Result<RawUdpTunnelRequest, String> {
    if payload.len() < RAW_UDP_HEADER_BYTES {
        return Err("raw UDP tunnel frame is too short".to_string());
    }
    if &payload[..4] != RAW_UDP_TUNNEL_MAGIC {
        return Err("raw UDP tunnel magic mismatch".to_string());
    }
    if payload[4] != RAW_UDP_TUNNEL_VERSION {
        return Err("raw UDP tunnel version is unsupported".to_string());
    }
    if payload[5] != RAW_UDP_KIND_PACKET {
        return Err("raw UDP tunnel kind must be packet".to_string());
    }

    let session_id_len = read_u16(payload, 8)? as usize;
    let target_id_len = read_u16(payload, 10)? as usize;
    let host_len = read_u16(payload, 12)? as usize;
    let port = read_u16(payload, 14)?;
    let payload_len = read_u32(payload, 16)? as usize;
    let total_len = RAW_UDP_HEADER_BYTES
        .checked_add(session_id_len)
        .and_then(|value| value.checked_add(target_id_len))
        .and_then(|value| value.checked_add(host_len))
        .and_then(|value| value.checked_add(payload_len))
        .ok_or_else(|| "raw UDP tunnel frame length overflow".to_string())?;
    if payload.len() != total_len {
        return Err("raw UDP tunnel frame length mismatch".to_string());
    }

    let mut offset = RAW_UDP_HEADER_BYTES;
    let session_id = read_utf8_field(payload, &mut offset, session_id_len, "session_id")?;
    let target_id = if target_id_len == 0 {
        None
    } else {
        Some(read_utf8_field(
            payload,
            &mut offset,
            target_id_len,
            "target_id",
        )?)
    };
    let host = read_utf8_field(payload, &mut offset, host_len, "host")?;
    let tunneled_payload = payload[offset..offset + payload_len].to_vec();

    if session_id.trim().is_empty() {
        return Err("raw UDP tunnel session_id is required".to_string());
    }
    if host.trim().is_empty() {
        return Err("raw UDP tunnel host is required".to_string());
    }
    if port == 0 {
        return Err("raw UDP tunnel port is required".to_string());
    }

    Ok(RawUdpTunnelRequest {
        session_id,
        target_id,
        host,
        port,
        payload: tunneled_payload,
    })
}

fn encode_raw_udp_tunnel_response(session_id: &str, status: &str, payload: &[u8]) -> Vec<u8> {
    let session_bytes = session_id.as_bytes();
    let status_bytes = status.as_bytes();
    let session_len = u16::try_from(session_bytes.len()).unwrap_or(u16::MAX);
    let status_len = u16::try_from(status_bytes.len()).unwrap_or(u16::MAX);
    let payload_len = u32::try_from(payload.len()).unwrap_or(u32::MAX);

    let mut encoded = Vec::with_capacity(
        RAW_UDP_HEADER_BYTES + session_bytes.len() + status_bytes.len() + payload.len(),
    );
    encoded.extend_from_slice(RAW_UDP_TUNNEL_MAGIC);
    encoded.push(RAW_UDP_TUNNEL_VERSION);
    encoded.push(RAW_UDP_KIND_RESPONSE);
    encoded.push(raw_status_code(status));
    encoded.push(0);
    encoded.extend_from_slice(&session_len.to_be_bytes());
    encoded.extend_from_slice(&status_len.to_be_bytes());
    encoded.extend_from_slice(&0_u16.to_be_bytes());
    encoded.extend_from_slice(&0_u16.to_be_bytes());
    encoded.extend_from_slice(&payload_len.to_be_bytes());
    encoded.extend_from_slice(session_bytes);
    encoded.extend_from_slice(status_bytes);
    encoded.extend_from_slice(payload);
    encoded
}

fn raw_status_code(status: &str) -> u8 {
    match status {
        "forwarded" => 0,
        "upstream_timeout" => 1,
        _ => 2,
    }
}

fn read_u16(payload: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = payload
        .get(offset..offset + 2)
        .ok_or_else(|| "raw UDP tunnel frame is truncated".to_string())?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = payload
        .get(offset..offset + 4)
        .ok_or_else(|| "raw UDP tunnel frame is truncated".to_string())?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_utf8_field(
    payload: &[u8],
    offset: &mut usize,
    len: usize,
    field: &str,
) -> Result<String, String> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| "raw UDP tunnel field length overflow".to_string())?;
    let bytes = payload
        .get(*offset..end)
        .ok_or_else(|| "raw UDP tunnel frame is truncated".to_string())?;
    *offset = end;
    std::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|_| format!("raw UDP tunnel {field} must be UTF-8"))
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
            route_policy: None,
            route_policy_id: None,
            route_policy_hash: None,
            user_id: request.user_id,
            device_id: request.device_id.clone(),
            game_id: request.game_id,
            region_id: request.region_id,
        }
    }

    fn from_claims(
        claims: ClientTokenClaims,
        route_policy: Option<RoutePolicy>,
    ) -> Result<Self, (&'static str, String)> {
        if let Some(expected_hash) = claims.route_policy_hash.as_deref() {
            let Some(route_policy) = route_policy.as_ref() else {
                return Err((
                    "missing_route_policy",
                    "route_policy is required by token".to_string(),
                ));
            };
            let actual_hash = hash_route_policy(route_policy).map_err(|error| {
                (
                    "invalid_route_policy",
                    format!("failed to hash route_policy: {error}"),
                )
            })?;
            if actual_hash != expected_hash {
                return Err((
                    "route_policy_mismatch",
                    "route_policy hash does not match token".to_string(),
                ));
            }
            if let Some(policy_id) = claims.route_policy_id.as_deref() {
                if route_policy.policy_id != policy_id {
                    return Err((
                        "route_policy_mismatch",
                        "route_policy policy_id does not match token".to_string(),
                    ));
                }
            }
        }

        let dynamic_route = claims.route_policy_hash.is_some();
        Ok(Self {
            credential_present: true,
            credential_valid: true,
            credential_expires_at: Some(claims.expires_at),
            intent_id: claims.intent_id,
            route_target_addr: if dynamic_route {
                None
            } else {
                claims.route.map(|route| route.target_addr)
            },
            route_policy: if dynamic_route { route_policy } else { None },
            route_policy_id: claims.route_policy_id,
            route_policy_hash: claims.route_policy_hash,
            user_id: Some(claims.user_id),
            device_id: Some(claims.device_id),
            game_id: Some(claims.game_id),
            region_id: claims.region_id,
        })
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
        assert_eq!(request.target_protocol.as_deref(), None);
        assert!(request.target.is_none());
        assert_eq!(request.response_timeout_ms, Some(50));
    }

    #[test]
    fn parses_dynamic_session_data_target() {
        let payload = br#"{"type":"session.data","protocol":"xaccel/1","session_id":"s1","payload":"aGVsbG8=","target":{"target_id":"gameplay","protocol":"udp","host":"198.51.100.20","port":27015}}"#;
        let ParsedClientMessage::SessionData(request) = parse_client_message(payload) else {
            panic!("expected session.data request");
        };

        let target = request.target.expect("dynamic target");
        assert_eq!(target.target_id.as_deref(), Some("gameplay"));
        assert_eq!(target.protocol.as_deref(), Some("udp"));
        assert_eq!(target.host, "198.51.100.20");
        assert_eq!(target.port, 27015);
    }

    #[test]
    fn parses_raw_udp_tunnel_frame() {
        let frame = raw_udp_frame("s1", Some("gameplay"), "198.51.100.20", 27015, b"hello");
        let ParsedClientMessage::RawUdpTunnel(request) = parse_client_message(&frame) else {
            panic!("expected raw UDP tunnel request");
        };

        assert_eq!(request.session_id, "s1");
        assert_eq!(request.target_id.as_deref(), Some("gameplay"));
        assert_eq!(request.host, "198.51.100.20");
        assert_eq!(request.port, 27015);
        assert_eq!(request.payload, b"hello");
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
    async fn relays_tcp_payload_to_target() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, b"hello");
            stream.write_all(b"tcp:hello").await.unwrap();
        });

        let outcome = relay_tcp_payload(target, b"hello", 500).await.unwrap();

        assert!(!outcome.timed_out);
        assert_eq!(outcome.upstream_tx_bytes, 5);
        assert_eq!(outcome.payload, b"tcp:hello");
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
            target_protocol: None,
            target: None,
            response_timeout_ms: None,
        };

        let target = resolve_session_target(&request, Some("127.0.0.1:7777"), None)
            .await
            .expect("target resolves")
            .expect("target exists");

        assert_eq!(target.socket_addr.port(), 7777);
        assert_eq!(target.protocol, RelayProtocol::Udp);
    }

    #[tokio::test]
    async fn resolves_tcp_request_target_protocol() {
        let request = ClientSessionDataRequest {
            session_id: Some("s1".to_string()),
            client_nonce: None,
            payload: Some("aGVsbG8=".to_string()),
            target_addr: Some("127.0.0.1:443".to_string()),
            target_host: None,
            target_port: None,
            target_protocol: Some("tcp".to_string()),
            target: None,
            response_timeout_ms: None,
        };

        let target = resolve_session_target(&request, None, None)
            .await
            .expect("target resolves")
            .expect("target exists");

        assert_eq!(target.socket_addr.port(), 443);
        assert_eq!(target.protocol, RelayProtocol::Tcp);
    }

    #[test]
    fn rejects_wrong_protocol() {
        let payload = br#"{"type":"probe","protocol":"bad"}"#;
        let ParsedClientMessage::Invalid(message) = parse_client_message(payload) else {
            panic!("expected invalid request");
        };

        assert!(message.contains(PROTOCOL_VERSION));
    }

    fn raw_udp_frame(
        session_id: &str,
        target_id: Option<&str>,
        host: &str,
        port: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let session_bytes = session_id.as_bytes();
        let target_bytes = target_id.unwrap_or("").as_bytes();
        let host_bytes = host.as_bytes();
        let mut frame = Vec::new();
        frame.extend_from_slice(RAW_UDP_TUNNEL_MAGIC);
        frame.push(RAW_UDP_TUNNEL_VERSION);
        frame.push(RAW_UDP_KIND_PACKET);
        frame.push(0);
        frame.push(0);
        frame.extend_from_slice(&(session_bytes.len() as u16).to_be_bytes());
        frame.extend_from_slice(&(target_bytes.len() as u16).to_be_bytes());
        frame.extend_from_slice(&(host_bytes.len() as u16).to_be_bytes());
        frame.extend_from_slice(&port.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(session_bytes);
        frame.extend_from_slice(target_bytes);
        frame.extend_from_slice(host_bytes);
        frame.extend_from_slice(payload);
        frame
    }
}
