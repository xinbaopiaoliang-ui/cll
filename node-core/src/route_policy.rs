use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::IpAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutePolicy {
    pub policy_id: String,
    pub policy_version: u32,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_strategy: Option<String>,
    #[serde(default)]
    pub targets: Vec<RouteTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteTarget {
    pub target_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    pub host_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default)]
    pub resolved_ips: Vec<String>,
    #[serde(default)]
    pub observed_ips: Vec<String>,
    #[serde(default)]
    pub cidrs: Vec<String>,
    #[serde(default)]
    pub ports: Vec<PortRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_client_observed_ip: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_ttl_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortRange {
    pub protocol: String,
    pub from: u16,
    pub to: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionDataTarget {
    pub target_id: Option<String>,
    pub protocol: Option<String>,
    pub host: String,
    pub port: u16,
    pub original_domain: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TargetMatch {
    pub target_id: String,
    pub policy_id: String,
}

pub fn hash_route_policy(policy: &RoutePolicy) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec(policy).context("failed to encode route_policy")?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes)))
}

pub fn match_route_policy_target(
    policy: &RoutePolicy,
    target: &SessionDataTarget,
) -> Result<TargetMatch, &'static str> {
    let protocol = target
        .protocol
        .as_deref()
        .unwrap_or(policy.default_protocol.as_deref().unwrap_or("udp"))
        .to_ascii_lowercase();

    if protocol != "udp" {
        return Err("target_protocol_unsupported");
    }

    for rule in &policy.targets {
        if let Some(target_id) = target.target_id.as_deref() {
            if rule.target_id != target_id {
                continue;
            }
        }

        if !rule.ports.iter().any(|range| {
            range.protocol.eq_ignore_ascii_case(&protocol)
                && target.port >= range.from
                && target.port <= range.to
        }) {
            continue;
        }

        if target_matches_host(rule, target) {
            return Ok(TargetMatch {
                target_id: rule.target_id.clone(),
                policy_id: policy.policy_id.clone(),
            });
        }
    }

    Err("target_not_allowed")
}

fn target_matches_host(rule: &RouteTarget, target: &SessionDataTarget) -> bool {
    let host = target.host.trim();
    if host.is_empty() {
        return false;
    }

    let host_type = rule.host_type.to_ascii_lowercase();
    match host_type.as_str() {
        "any" => true,
        "domain" => {
            rule.host
                .as_deref()
                .is_some_and(|rule_host| host.eq_ignore_ascii_case(rule_host))
                || target
                    .original_domain
                    .as_deref()
                    .zip(rule.host.as_deref())
                    .is_some_and(|(original, rule_host)| original.eq_ignore_ascii_case(rule_host))
                || contains_text(&rule.resolved_ips, host)
        }
        "ipv4" | "ipv6" => {
            rule.host
                .as_deref()
                .is_some_and(|rule_host| host.eq_ignore_ascii_case(rule_host))
                || contains_text(&rule.resolved_ips, host)
        }
        "observed_ip" => {
            contains_text(&rule.observed_ips, host) || contains_text(&rule.resolved_ips, host)
        }
        "cidr" => rule.cidrs.iter().any(|cidr| ip_in_cidr(host, cidr)),
        "steam_sdr" => {
            rule.host
                .as_deref()
                .is_some_and(|rule_host| host.eq_ignore_ascii_case(rule_host))
                || contains_text(&rule.resolved_ips, host)
                || contains_text(&rule.observed_ips, host)
                || rule.cidrs.iter().any(|cidr| ip_in_cidr(host, cidr))
                || (rule.host.is_none()
                    && rule.resolved_ips.is_empty()
                    && rule.observed_ips.is_empty()
                    && rule.cidrs.is_empty())
        }
        _ => false,
    }
}

fn contains_text(values: &[String], needle: &str) -> bool {
    values
        .iter()
        .any(|value| value.trim().eq_ignore_ascii_case(needle))
}

fn ip_in_cidr(host: &str, cidr: &str) -> bool {
    let Ok(ip) = host.parse::<IpAddr>() else {
        return false;
    };
    let Some((base, prefix)) = cidr.split_once('/') else {
        return host.eq_ignore_ascii_case(cidr);
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    let Ok(base) = base.parse::<IpAddr>() else {
        return false;
    };

    match (ip, base) {
        (IpAddr::V4(ip), IpAddr::V4(base)) if prefix <= 32 => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(ip) & mask) == (u32::from(base) & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(base)) if prefix <= 128 => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(ip) & mask) == (u128::from(base) & mask)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_observed_ip_and_port_range() {
        let policy = RoutePolicy {
            policy_id: "rp-1".to_string(),
            policy_version: 1,
            mode: "dynamic_targets".to_string(),
            default_protocol: Some("udp".to_string()),
            dns_strategy: None,
            targets: vec![RouteTarget {
                target_id: "gameplay".to_string(),
                purpose: None,
                host_type: "observed_ip".to_string(),
                host: None,
                resolved_ips: Vec::new(),
                observed_ips: vec!["198.51.100.20".to_string()],
                cidrs: Vec::new(),
                ports: vec![PortRange {
                    protocol: "udp".to_string(),
                    from: 27000,
                    to: 27050,
                }],
                allow_client_observed_ip: None,
                resolve_ttl_sec: None,
                required: None,
            }],
            capture: None,
        };
        let target = SessionDataTarget {
            target_id: Some("gameplay".to_string()),
            protocol: Some("udp".to_string()),
            host: "198.51.100.20".to_string(),
            port: 27015,
            original_domain: None,
        };

        assert!(match_route_policy_target(&policy, &target).is_ok());
    }

    #[test]
    fn rejects_port_outside_range() {
        let policy = RoutePolicy {
            policy_id: "rp-1".to_string(),
            policy_version: 1,
            mode: "dynamic_targets".to_string(),
            default_protocol: Some("udp".to_string()),
            dns_strategy: None,
            targets: vec![RouteTarget {
                target_id: "gameplay".to_string(),
                purpose: None,
                host_type: "any".to_string(),
                host: None,
                resolved_ips: Vec::new(),
                observed_ips: Vec::new(),
                cidrs: Vec::new(),
                ports: vec![PortRange {
                    protocol: "udp".to_string(),
                    from: 27000,
                    to: 27050,
                }],
                allow_client_observed_ip: None,
                resolve_ttl_sec: None,
                required: None,
            }],
            capture: None,
        };
        let target = SessionDataTarget {
            target_id: Some("gameplay".to_string()),
            protocol: Some("udp".to_string()),
            host: "198.51.100.20".to_string(),
            port: 27100,
            original_domain: None,
        };

        assert_eq!(
            match_route_policy_target(&policy, &target).unwrap_err(),
            "target_not_allowed"
        );
    }
}
