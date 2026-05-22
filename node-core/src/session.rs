use crate::{
    auth::{verify_probe_token, AuthDecision, ClientTokenClaims},
    state::RuntimeState,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};

pub const PROTOCOL_VERSION: &str = "xaccel/1";
const PROBE_TTL_SEC: u64 = 30;

#[derive(Debug, Clone, Copy, Serialize)]
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
    #[serde(rename = "type")]
    pub message_type: Option<String>,
    pub protocol: Option<String>,
    pub client_nonce: Option<String>,
    pub user_id: Option<u64>,
    pub device_id: Option<String>,
    pub game_id: Option<u64>,
    pub transport: Option<String>,
    pub token: Option<String>,
}

#[derive(Debug)]
pub enum ParsedClientMessage {
    LegacyPing,
    Probe(ClientProbeRequest),
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
    auth_required: bool,
    credential_present: bool,
    credential_valid: bool,
    credential_expires_at: Option<u64>,
    user_id: Option<u64>,
    device_id: Option<String>,
    game_id: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ClientProbeError {
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

    let Ok(request) = serde_json::from_str::<ClientProbeRequest>(trimmed) else {
        return ParsedClientMessage::Invalid("expected JSON probe request".to_string());
    };

    if !request
        .message_type
        .as_deref()
        .is_some_and(|message_type| message_type == "probe")
    {
        return ParsedClientMessage::Invalid("type must be probe".to_string());
    }

    if !request
        .protocol
        .as_deref()
        .is_some_and(|protocol| protocol == PROTOCOL_VERSION)
    {
        return ParsedClientMessage::Invalid(format!("protocol must be {PROTOCOL_VERSION}"));
    }

    ParsedClientMessage::Probe(request)
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
            "session_stats",
        ],
    };

    encode_json_line(&response)
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
    let response = ClientProbeError {
        message_type: "probe.error",
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
            user_id: Some(claims.user_id),
            device_id: Some(claims.device_id),
            game_id: Some(claims.game_id),
        }
    }
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
    fn rejects_wrong_protocol() {
        let payload = br#"{"type":"probe","protocol":"bad"}"#;
        let ParsedClientMessage::Invalid(message) = parse_client_message(payload) else {
            panic!("expected invalid request");
        };

        assert!(message.contains(PROTOCOL_VERSION));
    }
}
