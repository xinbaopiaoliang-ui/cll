use crate::session::ClientProbeRequest;
use anyhow::{bail, Context};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const TOKEN_PREFIX: &str = "xat";
const TOKEN_VERSION: &str = "v1";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClientTokenClaims {
    pub node_id: u64,
    pub user_id: u64,
    pub device_id: String,
    pub game_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub business: Option<BusinessAuthContext>,
    pub intent_id: Option<String>,
    pub route: Option<ClientRouteClaims>,
    pub expires_at: u64,
    pub issued_at: Option<u64>,
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BusinessAuthContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entitlement_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub business_session_id: Option<String>,
    pub entitlement_verified: bool,
    pub device_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entitlement_expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub business_trace_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClientRouteClaims {
    pub target_addr: String,
    pub protocol: String,
}

#[derive(Debug, Clone)]
pub enum AuthDecision {
    Missing,
    Valid(ClientTokenClaims),
    Invalid { code: &'static str, message: String },
}

pub fn verify_probe_token(
    request: &ClientProbeRequest,
    expected_node_id: Option<u64>,
    secret: Option<&str>,
) -> AuthDecision {
    let Some(token) = request
        .token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        return AuthDecision::Missing;
    };

    let Some(secret) = secret.filter(|secret| !secret.trim().is_empty()) else {
        return AuthDecision::Invalid {
            code: "auth_not_ready",
            message: "node_secret is missing".to_string(),
        };
    };

    match verify_token(token, expected_node_id, secret) {
        Ok(claims) => match_request_claims(request, claims),
        Err(error) => AuthDecision::Invalid {
            code: "invalid_token",
            message: error.to_string(),
        },
    }
}

fn verify_token(
    token: &str,
    expected_node_id: Option<u64>,
    secret: &str,
) -> anyhow::Result<ClientTokenClaims> {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != TOKEN_PREFIX || parts[1] != TOKEN_VERSION {
        bail!("token format must be xat.v1.payload.signature");
    }

    let signing_input = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
    let signature = URL_SAFE_NO_PAD
        .decode(parts[3])
        .context("token signature is not valid base64url")?;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
        .context("failed to initialize token verifier")?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&signature)
        .context("token signature mismatch")?;

    let payload = URL_SAFE_NO_PAD
        .decode(parts[2])
        .context("token payload is not valid base64url")?;
    let claims: ClientTokenClaims =
        serde_json::from_slice(&payload).context("token payload is not valid JSON")?;

    if let Some(node_id) = expected_node_id {
        if claims.node_id != node_id {
            bail!("token node_id does not match this node");
        }
    }

    if claims.expires_at <= now_unix() {
        bail!("token expired");
    }

    if claims.device_id.trim().is_empty() {
        bail!("token device_id is required");
    }

    if let Some(route) = claims.route.as_ref() {
        if route.target_addr.trim().is_empty() {
            bail!("token route.target_addr is required when route is present");
        }
        if route.protocol != "udp" {
            bail!("token route.protocol must be udp");
        }
    }

    Ok(claims)
}

fn match_request_claims(request: &ClientProbeRequest, claims: ClientTokenClaims) -> AuthDecision {
    if request
        .user_id
        .is_some_and(|user_id| user_id != claims.user_id)
    {
        return AuthDecision::Invalid {
            code: "claim_mismatch",
            message: "request user_id does not match token".to_string(),
        };
    }

    if request
        .device_id
        .as_deref()
        .is_some_and(|device_id| device_id != claims.device_id)
    {
        return AuthDecision::Invalid {
            code: "claim_mismatch",
            message: "request device_id does not match token".to_string(),
        };
    }

    if request
        .game_id
        .is_some_and(|game_id| game_id != claims.game_id)
    {
        return AuthDecision::Invalid {
            code: "claim_mismatch",
            message: "request game_id does not match token".to_string(),
        };
    }

    AuthDecision::Valid(claims)
}

pub fn sign_client_token(claims: &ClientTokenClaims, secret: &str) -> anyhow::Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn claims() -> ClientTokenClaims {
        ClientTokenClaims {
            node_id: 1,
            user_id: 1001,
            device_id: "pc-001".to_string(),
            game_id: 8888,
            business: None,
            intent_id: Some("intent-test".to_string()),
            route: Some(ClientRouteClaims {
                target_addr: "127.0.0.1:7777".to_string(),
                protocol: "udp".to_string(),
            }),
            expires_at: now_unix() + 60,
            issued_at: Some(now_unix()),
            nonce: Some("n1".to_string()),
        }
    }

    #[test]
    fn accepts_valid_token() {
        let token = sign_client_token(&claims(), "secret").expect("test token signs");
        let request = ClientProbeRequest {
            client_nonce: None,
            user_id: Some(1001),
            device_id: Some("pc-001".to_string()),
            game_id: Some(8888),
            transport: None,
            token: Some(token),
        };

        assert!(matches!(
            verify_probe_token(&request, Some(1), Some("secret")),
            AuthDecision::Valid(_)
        ));
    }

    #[test]
    fn keeps_connect_intent_route_claims() {
        let token = sign_client_token(&claims(), "secret").expect("test token signs");
        let request = ClientProbeRequest {
            client_nonce: None,
            user_id: Some(1001),
            device_id: Some("pc-001".to_string()),
            game_id: Some(8888),
            transport: None,
            token: Some(token),
        };

        let AuthDecision::Valid(claims) = verify_probe_token(&request, Some(1), Some("secret"))
        else {
            panic!("expected valid token");
        };

        assert_eq!(claims.intent_id.as_deref(), Some("intent-test"));
        assert_eq!(
            claims
                .route
                .as_ref()
                .map(|route| route.target_addr.as_str()),
            Some("127.0.0.1:7777")
        );
    }

    #[test]
    fn rejects_mismatched_claim() {
        let token = sign_client_token(&claims(), "secret").expect("test token signs");
        let request = ClientProbeRequest {
            client_nonce: None,
            user_id: Some(1002),
            device_id: Some("pc-001".to_string()),
            game_id: Some(8888),
            transport: None,
            token: Some(token),
        };

        assert!(matches!(
            verify_probe_token(&request, Some(1), Some("secret")),
            AuthDecision::Invalid {
                code: "claim_mismatch",
                ..
            }
        ));
    }

    #[test]
    fn treats_absent_token_as_missing() {
        let request = ClientProbeRequest {
            client_nonce: None,
            user_id: None,
            device_id: None,
            game_id: None,
            transport: None,
            token: None,
        };

        assert!(matches!(
            verify_probe_token(&request, Some(1), Some("secret")),
            AuthDecision::Missing
        ));
    }
}
